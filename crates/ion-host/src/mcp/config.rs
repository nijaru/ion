//! MCP configuration validation and serialized atomic publication.
use super::http_config;
use crate::model_setup::{write_json, write_shared_json};
use anyhow::{Context, Result, bail, ensure};
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{DirBuilder, File},
    io::Read,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};
use url::Url;

#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum McpServer {
    Stdio(McpStdioServer),
    Http(McpHttpServer),
}

impl McpServer {
    pub fn enabled(&self) -> bool {
        match self {
            Self::Stdio(server) => server.enabled,
            Self::Http(server) => server.enabled,
        }
    }

    pub fn exposure(&self) -> McpExposure {
        match self {
            Self::Stdio(server) => server.exposure,
            Self::Http(server) => server.exposure,
        }
    }

    pub fn tool_exposure(&self) -> &IndexMap<String, McpExposure> {
        match self {
            Self::Stdio(server) => &server.tool_exposure,
            Self::Http(server) => &server.tool_exposure,
        }
    }
}

/// How a configured MCP server's tools reach the model. Exposure is not
/// authority: every registered executor remains callable by the frozen
/// catalogue. Hidden tools are omitted; a hidden server's resources stay omitted
/// even when individual tool overrides expose some tools.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpExposure {
    #[default]
    Direct,
    Deferred,
    Hidden,
}

impl McpExposure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Deferred => "deferred",
            Self::Hidden => "hidden",
        }
    }

    fn is_direct(&self) -> bool {
        *self == Self::Direct
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpStdioServer {
    #[serde(default = "enabled_by_default", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Relative paths are resolved against the Session working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Overrides inherited variables; `${NAME}` references the parent environment.
    #[serde(
        default,
        deserialize_with = "deserialize_value_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "McpExposure::is_direct")]
    pub exposure: McpExposure,
    /// Exact remote names win; otherwise the first matching `*` pattern wins.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub tool_exposure: IndexMap<String, McpExposure>,
}

// Generic string deserialization can echo malformed values (for example a
// numeric PIN). Keep secret value-map errors value-free at this boundary.
fn deserialize_value_map<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    let error = || D::Error::custom("MCP values must map names to strings");
    let Value::Object(entries) = Value::deserialize(deserializer)? else {
        return Err(error());
    };
    entries
        .into_iter()
        .map(|(name, value)| match value {
            Value::String(value) => Ok((name, value)),
            _ => Err(error()),
        })
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpHttpServer {
    #[serde(default = "enabled_by_default", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env: Option<String>,
    /// Header values use the same non-recursive environment templates as stdio env.
    #[serde(
        default,
        deserialize_with = "deserialize_value_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "McpExposure::is_direct")]
    pub exposure: McpExposure,
    /// Exact remote names win; otherwise the first matching `*` pattern wins.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub tool_exposure: IndexMap<String, McpExposure>,
}

fn enabled_by_default() -> bool {
    true
}

fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServers {
    // Retain nested rule order and malformed peers through unrelated config edits.
    servers: BTreeMap<String, Box<RawValue>>,
}

pub struct McpListing {
    pub servers: BTreeMap<String, McpServer>,
    pub diagnostics: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    #[serde(default)]
    projects: BTreeMap<String, String>,
}

struct ProjectConfig {
    root: PathBuf,
    path: PathBuf,
}

pub struct McpConfig {
    user: PathBuf,
    project: Option<ProjectConfig>,
    trust: PathBuf,
}

impl McpConfig {
    pub fn new(config_root: &Path) -> Self {
        Self {
            user: config_root.join("mcp.json"),
            project: None,
            trust: config_root.join("mcp-trust.json"),
        }
    }

    /// User configuration plus the project layer rooted at the nearest Git
    /// ancestor (or `cwd`). Project entries stay inert until explicitly trusted
    /// for their current digest.
    pub fn with_project(config_root: &Path, cwd: &Path) -> Result<Self> {
        let root = project_root(cwd)?;
        let path = root.join(".ion/mcp.json");
        Ok(Self {
            user: config_root.join("mcp.json"),
            project: (!crate::same_directory(config_root, &root.join(".ion")))
                .then_some(ProjectConfig { root, path }),
            trust: config_root.join("mcp-trust.json"),
        })
    }

    pub fn project_path(&self) -> Option<&Path> {
        self.project.as_ref().map(|project| project.path.as_path())
    }

    pub fn list(&self) -> Result<McpListing> {
        let mut servers = BTreeMap::new();
        let mut diagnostics = Vec::new();
        parse_servers(
            read_servers(&self.user)?,
            &mut servers,
            &mut diagnostics,
            ConfigScope::User,
        );
        if let Some(project) = &self.project {
            match read_config(&project.path) {
                Ok(bytes) => {
                    let digest = digest(&bytes);
                    match self.project_is_trusted(project, &digest) {
                        Ok(true) => match serde_json::from_slice::<RawServers>(&bytes) {
                            Ok(raw) => parse_servers(
                                raw.servers,
                                &mut servers,
                                &mut diagnostics,
                                ConfigScope::Project,
                            ),
                            Err(error) => diagnostics.push(format!(
                                "invalid project MCP config {}: {error}",
                                project.path.display()
                            )),
                        },
                        Ok(false) => diagnostics.push(format!(
                            "project MCP config {} is not trusted for its current contents; run `ion trust` in that project",
                            project.path.display()
                        )),
                        Err(error) => diagnostics.push(format!(
                            "cannot read MCP trust store {}: {error:#}",
                            self.trust.display()
                        )),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => diagnostics.push(format!(
                    "cannot read project MCP config {}: {error}",
                    project.path.display()
                )),
            }
        }
        Ok(McpListing {
            servers,
            diagnostics,
        })
    }

    pub(super) fn load_startup(&self) -> Result<(BTreeMap<String, McpServer>, Vec<String>)> {
        let listing = self.list()?;
        Ok((listing.servers, listing.diagnostics))
    }

    /// Trust the current project configuration digest. The configuration is
    /// validated before trust is recorded; editing the file invalidates it.
    pub fn trust_project(&self) -> Result<()> {
        let project = self
            .project
            .as_ref()
            .context("no project MCP configuration")?;
        let bytes = read_config(&project.path).with_context(|| {
            format!(
                "cannot read project MCP config {}; create it with `ion mcp add --project`",
                project.path.display()
            )
        })?;
        let raw = serde_json::from_slice::<RawServers>(&bytes)
            .with_context(|| format!("invalid project MCP config {}", project.path.display()))?;
        let mut servers = BTreeMap::new();
        // Unrelated bad user peers stay isolated; project policies still need a valid base.
        let mut user_diagnostics = Vec::new();
        parse_servers(
            read_servers(&self.user)?,
            &mut servers,
            &mut user_diagnostics,
            ConfigScope::User,
        );
        let mut diagnostics = Vec::new();
        parse_servers(
            raw.servers,
            &mut servers,
            &mut diagnostics,
            ConfigScope::Project,
        );
        ensure!(
            diagnostics.is_empty(),
            "cannot trust invalid project MCP configuration: {}",
            diagnostics.join("; ")
        );
        let mut trust = self.read_trust()?;
        trust.projects.insert(project_key(project), digest(&bytes));
        write_json(&self.trust, &trust)
    }

    pub fn untrust_project(&self) -> Result<()> {
        let project = self
            .project
            .as_ref()
            .context("no project MCP configuration")?;
        let mut trust = self.read_trust()?;
        ensure!(
            trust.projects.remove(&project_key(project)).is_some(),
            "project MCP configuration is not trusted"
        );
        write_json(&self.trust, &trust)
    }

    fn project_is_trusted(&self, project: &ProjectConfig, digest: &str) -> Result<bool> {
        let trust = self.read_trust()?;
        Ok(trust
            .projects
            .get(&project_key(project))
            .is_some_and(|value| value == digest))
    }

    fn read_trust(&self) -> Result<TrustFile> {
        match read_config(&self.trust) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid MCP trust store {}", self.trust.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TrustFile::default()),
            Err(error) => Err(error)
                .with_context(|| format!("cannot read MCP trust store {}", self.trust.display())),
        }
    }

    pub fn add(&self, name: &str, server: McpServer) -> Result<()> {
        self.add_at(&self.user, name, server, false)
    }

    pub fn add_project(&self, name: &str, server: McpServer) -> Result<()> {
        let project = self
            .project
            .as_ref()
            .context("no project MCP configuration")?;
        self.add_at(&project.path, name, server, true)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        self.remove_at(&self.user, name, false)
    }

    pub fn remove_project(&self, name: &str) -> Result<()> {
        let project = self
            .project
            .as_ref()
            .context("no project MCP configuration")?;
        self.remove_at(&project.path, name, true)
    }

    /// Persist startup enablement without changing any active connection.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        self.set_enabled_at(&self.user, name, enabled, false)
    }

    /// Editing project enablement invalidates its existing digest trust.
    pub fn set_project_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let project = self
            .project
            .as_ref()
            .context("no project MCP configuration")?;
        self.set_enabled_at(&project.path, name, enabled, true)
    }

    fn set_enabled_at(&self, path: &Path, name: &str, enabled: bool, shared: bool) -> Result<()> {
        let _lock = writer_lock(path, shared)?;
        let mut servers = read_servers(path)?;
        let raw = match servers.get(name) {
            Some(raw) => raw.clone(),
            None if shared => RawValue::from_string("{}".into())?,
            None => bail!("MCP server {name} was not found"),
        };
        let mut fields = entry_fields(&raw)?;
        let is_override = shared && !fields.contains_key("command") && !fields.contains_key("url");
        let server = if is_override {
            let base = read_servers(&self.user)?
                .remove(name)
                .map(|raw| parse_server(name, &raw))
                .transpose()?;
            parse_project_server(name, &raw, base.as_ref())?
        } else {
            parse_server(name, &raw)?
        };
        // An inherited value is not an explicit project policy. Save true too,
        // so a later user-level change cannot silently remove that override.
        if (!is_override || fields.contains_key("enabled")) && server.enabled() == enabled {
            return Ok(());
        }
        fields.insert("enabled".into(), serde_json::value::to_raw_value(&enabled)?);
        servers.insert(name.to_owned(), serde_json::value::to_raw_value(&fields)?);
        if shared {
            write_shared_json(path, &RawServers { servers })
        } else {
            write_json(path, &RawServers { servers })
        }
    }

    fn add_at(&self, path: &Path, name: &str, server: McpServer, shared: bool) -> Result<()> {
        validate_name(name)?;
        validate_server(&server)?;
        let _lock = writer_lock(path, shared)?;
        let mut servers = read_servers(path)?;
        ensure!(
            !servers.contains_key(name),
            "MCP server {name} already exists"
        );
        servers.insert(name.to_owned(), serde_json::value::to_raw_value(&server)?);
        if shared {
            write_shared_json(path, &RawServers { servers })
        } else {
            write_json(path, &RawServers { servers })
        }
    }

    fn remove_at(&self, path: &Path, name: &str, shared: bool) -> Result<()> {
        let _lock = writer_lock(path, shared)?;
        let mut servers = read_servers(path)?;
        ensure!(
            servers.remove(name).is_some(),
            "MCP server {name} was not found"
        );
        if shared {
            write_shared_json(path, &RawServers { servers })
        } else {
            write_json(path, &RawServers { servers })
        }
    }
}

// Configuration is a file, not a pipe/device transport. Use the same verified
// descriptor for inspection and reading, including project and trust sources.
fn read_config(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    crate::file_io::open_regular(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_servers(path: &Path) -> Result<BTreeMap<String, Box<RawValue>>> {
    match read_config(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<RawServers>(&bytes)
            .with_context(|| format!("invalid MCP config {}", path.display()))?
            .servers),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => {
            Err(error).with_context(|| format!("cannot read MCP config {}", path.display()))
        }
    }
}

enum ConfigScope {
    User,
    Project,
}

fn entry_fields(raw: &RawValue) -> Result<BTreeMap<String, Box<RawValue>>> {
    serde_json::from_str(raw.get()).map_err(|_| anyhow::anyhow!("MCP entry must be an object"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectPolicy {
    #[serde(default, deserialize_with = "present_policy")]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "present_policy")]
    exposure: Option<McpExposure>,
    #[serde(default, deserialize_with = "present_policy")]
    tool_exposure: Option<IndexMap<String, McpExposure>>,
}

// Missing means inherit; an explicitly supplied null is not a policy value.
fn present_policy<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

fn parse_project_server(name: &str, raw: &RawValue, base: Option<&McpServer>) -> Result<McpServer> {
    let fields = entry_fields(raw)?;
    if fields.contains_key("command") || fields.contains_key("url") {
        return parse_server(name, raw);
    }
    let patch: ProjectPolicy = serde_json::from_str(raw.get())
        .with_context(|| format!("invalid MCP project policy {name}"))?;
    let mut server = base
        .with_context(|| format!("MCP project override {name} needs a valid user server"))?
        .clone();
    let (enabled, exposure, tool_exposure) = match &mut server {
        McpServer::Stdio(server) => (
            &mut server.enabled,
            &mut server.exposure,
            &mut server.tool_exposure,
        ),
        McpServer::Http(server) => (
            &mut server.enabled,
            &mut server.exposure,
            &mut server.tool_exposure,
        ),
    };
    if let Some(value) = patch.enabled {
        *enabled = value;
    }
    if let Some(value) = patch.exposure {
        *exposure = value;
    }
    if let Some(value) = patch.tool_exposure {
        *tool_exposure = value;
    }
    validate_server(&server)?;
    Ok(server)
}

fn parse_servers(
    raw: BTreeMap<String, Box<RawValue>>,
    servers: &mut BTreeMap<String, McpServer>,
    diagnostics: &mut Vec<String>,
    scope: ConfigScope,
) {
    for (name, value) in raw {
        let parsed = match scope {
            ConfigScope::User => parse_server(&name, &value),
            ConfigScope::Project => parse_project_server(&name, &value, servers.get(&name)),
        };
        match parsed {
            Ok(server) => {
                servers.insert(name, server);
            }
            Err(error) => diagnostics.push(match scope {
                ConfigScope::User => format!("{error:#}"),
                ConfigScope::Project => format!("project: {error:#}"),
            }),
        }
    }
}

fn writer_lock(path: &Path, shared: bool) -> Result<File> {
    let root = path.parent().context("MCP config has no parent")?;
    let directory_mode = if shared { 0o755 } else { 0o700 };
    DirBuilder::new()
        .recursive(true)
        .mode(directory_mode)
        .create(root)?;
    // The separate lock inode survives atomic replacement of mcp.json.
    let lock = File::from(rustix::fs::open(
        root.join("mcp.lock"),
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW,
        if shared {
            rustix::fs::Mode::RUSR
                | rustix::fs::Mode::WUSR
                | rustix::fs::Mode::RGRP
                | rustix::fs::Mode::WGRP
                | rustix::fs::Mode::ROTH
                | rustix::fs::Mode::WOTH
        } else {
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR
        },
    )?);
    ensure!(lock.metadata()?.is_file(), "MCP lock is not a regular file");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(lock)
}

fn project_root(cwd: &Path) -> Result<PathBuf> {
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("cannot resolve working directory {}", cwd.display()))?;
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return Ok(ancestor.to_owned());
        }
    }
    Ok(cwd)
}

fn project_key(project: &ProjectConfig) -> String {
    project.root.to_string_lossy().into_owned()
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn parse_server(name: &str, value: &RawValue) -> Result<McpServer> {
    (|| -> Result<McpServer> {
        validate_name(name)?;
        // Inspect transport keys without sorting the nested selection rules.
        // Syntax was checked at the file boundary; keep non-object errors value-free.
        let object: BTreeMap<String, &RawValue> = serde_json::from_str(value.get())
            .map_err(|_| anyhow::anyhow!("MCP server must be an object"))?;
        let server = match (object.contains_key("command"), object.contains_key("url")) {
            (true, false) => McpServer::Stdio(serde_json::from_str(value.get())?),
            (false, true) => McpServer::Http(serde_json::from_str(value.get())?),
            (true, true) => bail!("MCP server cannot combine command and URL"),
            (false, false) => bail!("MCP server requires command or URL"),
        };
        validate_server(&server)?;
        Ok(server)
    })()
    .with_context(|| format!("invalid MCP server {name:?}"))
}

pub(super) fn validate_server(server: &McpServer) -> Result<()> {
    ensure!(
        server.tool_exposure().keys().all(|name| !name.is_empty()),
        "MCP tool exposure name is empty"
    );
    match server {
        McpServer::Stdio(server) => {
            ensure!(!server.command.trim().is_empty(), "MCP command is empty");
            if let Some(cwd) = &server.cwd {
                ensure!(
                    !cwd.as_os_str().is_empty() && !cwd.as_os_str().as_encoded_bytes().contains(&0),
                    "invalid MCP working directory"
                );
            }
            for (name, value) in &server.env {
                ensure!(
                    valid_env_name(name),
                    "invalid MCP environment variable name"
                );
                ensure!(
                    !value.contains('\0'),
                    "invalid MCP environment variable value"
                );
            }
        }
        McpServer::Http(server) => {
            http_config::validate(server)?;
            let url = Url::parse(&server.url).context("invalid MCP server URL")?;
            ensure!(
                matches!(url.scheme(), "http" | "https"),
                "MCP server URL must use HTTP or HTTPS"
            );
            ensure!(
                url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
                "MCP server URL cannot contain userinfo or fragment"
            );
            if let Some(name) = &server.bearer_token_env {
                ensure!(
                    valid_env_name(name),
                    "invalid MCP bearer token environment variable name"
                );
            }
        }
    }
    Ok(())
}

pub(super) fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        bail!("MCP name {name:?} must use only ASCII letters, numbers, '_' or '-'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        fs,
        sync::{Arc, Barrier},
    };

    fn parse_server(name: &str, value: Value) -> Result<McpServer> {
        super::parse_server(name, &serde_json::value::to_raw_value(&value)?)
    }

    #[test]
    fn exposure_defaults_to_direct_and_round_trips_explicit_modes() {
        let direct: McpStdioServer = serde_json::from_value(serde_json::json!({
            "command": "python3"
        }))
        .unwrap();
        assert_eq!(direct.exposure, McpExposure::Direct);
        let deferred: McpStdioServer = serde_json::from_value(serde_json::json!({
            "command": "python3",
            "exposure": "deferred"
        }))
        .unwrap();
        assert_eq!(deferred.exposure, McpExposure::Deferred);
        let hidden: McpHttpServer = serde_json::from_value(serde_json::json!({
            "url": "https://example.com/mcp",
            "exposure": "hidden"
        }))
        .unwrap();
        assert_eq!(hidden.exposure, McpExposure::Hidden);
        let encoded = serde_json::to_value(McpStdioServer {
            enabled: true,
            cwd: None,
            env: Default::default(),
            command: "python3".into(),
            args: Vec::new(),
            exposure: McpExposure::Direct,
            tool_exposure: Default::default(),
        })
        .unwrap();
        assert!(encoded.get("exposure").is_none());
    }

    #[test]
    fn tool_exposure_round_trips_both_transports_and_rejects_invalid_entries() {
        for transport in [
            json!({"command": "python3", "args": []}),
            json!({"url": "https://example.com/mcp"}),
        ] {
            let mut value = transport.clone();
            value["exposure"] = json!("hidden");
            value["tool_exposure"] = json!({
                "read.document": "direct",
                "search": "deferred",
                "remove": "hidden"
            });
            let server = parse_server("demo", value.clone()).unwrap();
            assert_eq!(serde_json::to_value(server).unwrap(), value);
            for invalid in [
                json!({"": "direct"}),
                json!({"read": "invalid"}),
                json!([]),
                json!(null),
            ] {
                let mut value = transport.clone();
                value["tool_exposure"] = invalid;
                assert!(parse_server("demo", value).is_err());
            }
        }
    }

    #[test]
    fn enablement_edits_preserve_settings_order_peers_and_digest_trust() {
        let root = std::env::temp_dir().join(format!("ion-mcp-enabled-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".git")).unwrap();
        let config = McpConfig::with_project(&root.join("user"), &root).unwrap();
        let manifest = r#"{"servers":{
            "local":{"command":"python3","args":["worker.py"],"cwd":"data","env":{"TOKEN":"${MISSING}"},"exposure":"deferred","tool_exposure":{"z*":"hidden","*":"direct"}},
            "remote":{"url":"https://example.com/mcp","headers":{"X-Key":"${MISSING}"},"exposure":"hidden","tool_exposure":{"z*":"hidden","*":"direct"}}
        }}"#;
        for project in [false, true] {
            let path = if project {
                config.project_path().unwrap()
            } else {
                &config.user
            };
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, manifest).unwrap();
            if project {
                // A disabled project definition must still shadow the enabled user copy.
                fs::write(&config.user, manifest).unwrap();
            }
            if !project {
                let mut raw = read_servers(path).unwrap();
                raw.insert(
                    "invalid".into(),
                    serde_json::value::to_raw_value(&json!({"command":7})).unwrap(),
                );
                write_json(path, &RawServers { servers: raw }).unwrap();
            }
            let before = fs::read(path).unwrap();
            let expected: Value = serde_json::from_slice(&before).unwrap();
            for name in ["local", "remote"] {
                if project {
                    config.trust_project().unwrap();
                }
                assert!(config.list().unwrap().servers[name].enabled());
                for enabled in [false, true] {
                    if project {
                        config.set_project_enabled(name, enabled).unwrap();
                        assert!(
                            config
                                .list()
                                .unwrap()
                                .diagnostics
                                .iter()
                                .any(|line| line.contains("not trusted"))
                        );
                        config.trust_project().unwrap();
                    } else {
                        config.set_enabled(name, enabled).unwrap();
                    }
                    let listing = config.list().unwrap();
                    let server = &listing.servers[name];
                    assert_eq!(server.enabled(), enabled);
                    assert_eq!(
                        server
                            .tool_exposure()
                            .keys()
                            .map(String::as_str)
                            .collect::<Vec<_>>(),
                        ["z*", "*"]
                    );
                    let mut saved = serde_json::to_value(server).unwrap();
                    saved.as_object_mut().unwrap().remove("enabled");
                    assert_eq!(saved, expected["servers"][name]);
                    let bytes = fs::read(path).unwrap();
                    if project {
                        config.set_project_enabled(name, enabled).unwrap();
                        assert!(config.list().unwrap().diagnostics.is_empty());
                    } else {
                        config.set_enabled(name, enabled).unwrap();
                        assert_eq!(
                            read_servers(path).unwrap()["invalid"].get(),
                            r#"{"command":7}"#
                        );
                    }
                    assert_eq!(
                        fs::read(path).unwrap(),
                        bytes,
                        "no-op toggle rewrote the manifest"
                    );
                }
            }
            let before_error = fs::read(path).unwrap();
            let absent = if project {
                config.set_project_enabled("absent", false)
            } else {
                config.set_enabled("invalid", false)
            };
            assert!(absent.is_err());
            assert_eq!(fs::read(path).unwrap(), before_error);
            fs::remove_file(path).unwrap();
        }
        for mut transport in [
            json!({"command":"python3"}),
            json!({"url":"https://example.com/mcp"}),
        ] {
            for invalid in [json!(null), json!(0), json!("false")] {
                transport["enabled"] = invalid;
                assert!(parse_server("invalid", transport.clone()).is_err());
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_policy_overrides_inherit_only_valid_user_launch_settings() {
        let root = std::env::temp_dir().join(format!("ion-mcp-policy-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".ion")).unwrap();
        let config = McpConfig::with_project(&root.join("user"), &root).unwrap();
        fs::create_dir_all(config.user.parent().unwrap()).unwrap();
        fs::write(&config.user, r#"{"servers":{
            "local":{"command":"python3","env":{"TOKEN":"user-secret"},"enabled":false,"tool_exposure":{"z*":"hidden","*":"direct"}},
            "remote":{"url":"https://example.com/mcp","headers":{"X-Key":"user-secret"}},
            "invalid":{"command":7}
        }}"#).unwrap();
        let path = config.project_path().unwrap();
        fs::write(path, r#"{"servers":{
            "local":{"enabled":true,"exposure":"deferred","tool_exposure":{"b*":"direct","*":"hidden"}},
            "remote":{"enabled":false}
        }}"#).unwrap();
        config.trust_project().unwrap();
        let listing = config.list().unwrap();
        assert!(listing.servers["local"].enabled());
        assert!(!listing.servers["remote"].enabled());
        assert_eq!(listing.servers["local"].exposure(), McpExposure::Deferred);
        assert_eq!(
            listing.servers["local"]
                .tool_exposure()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["b*", "*"]
        );
        config.set_project_enabled("local", false).unwrap();
        config.trust_project().unwrap();
        assert!(!config.list().unwrap().servers["local"].enabled());
        config.set_project_enabled("local", true).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["servers"]["local"]["enabled"], true);
        assert!(saved["servers"]["local"].get("command").is_none());
        assert!(!fs::read_to_string(path).unwrap().contains("user-secret"));
        for invalid in [
            r#"{"enabled":null}"#,
            r#"{"exposure":null}"#,
            r#"{"tool_exposure":null}"#,
            r#"{"args":[]}"#,
            r#"{"tool_exposure":{"":"direct"}}"#,
            r#"{"enabled":true,"enabled":false}"#,
        ] {
            fs::write(path, format!(r#"{{"servers":{{"local":{invalid}}}}}"#)).unwrap();
            assert!(config.trust_project().is_err(), "{invalid}");
        }
        fs::write(path, r#"{"servers":{"absent":{"enabled":false}}}"#).unwrap();
        assert!(config.trust_project().is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordered_tool_rules_survive_loading_and_unrelated_user_and_project_edits() {
        let root = std::env::temp_dir().join(format!("ion-mcp-order-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".git")).unwrap();
        let config = McpConfig::with_project(&root.join("user"), &root).unwrap();
        for project in [false, true] {
            let path = if project {
                config.project_path().unwrap()
            } else {
                &config.user
            };
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, r#"{"servers":{
                "local":{"command":"python3","tool_exposure":{"z*":"deferred","*":"hidden","zero":"direct"}},
                "remote":{"url":"https://example.com/mcp","tool_exposure":{"z*":"deferred","*":"hidden","zero":"direct"}}
            }}"#).unwrap();
            for step in 0..3 {
                if project {
                    config.trust_project().unwrap();
                }
                let listed = config.list().unwrap();
                assert!(listed.diagnostics.is_empty(), "{:?}", listed.diagnostics);
                for name in ["local", "remote"] {
                    let keys = listed.servers[name]
                        .tool_exposure()
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    assert_eq!(
                        keys,
                        ["z*", "*", "zero"],
                        "{name}: project={project}, step={step}"
                    );
                }
                if step == 0 {
                    let server = McpServer::Stdio(McpStdioServer {
                        enabled: true,
                        command: "python3".into(),
                        args: Vec::new(),
                        cwd: None,
                        env: Default::default(),
                        exposure: McpExposure::Direct,
                        tool_exposure: Default::default(),
                    });
                    if project {
                        config.add_project("other", server).unwrap();
                    } else {
                        config.add("other", server).unwrap();
                    }
                } else if step == 1 {
                    if project {
                        config.remove_project("other").unwrap();
                    } else {
                        config.remove("other").unwrap();
                    }
                }
            }
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stdio_launch_options_round_trip_and_validate_native_fields() {
        let value = json!({
            "command": "python3", "args": [], "cwd": "server data",
            "env": {"MODE": "local", "TOKEN": "${MCP_TOKEN}", "EMPTY": ""}
        });
        let server = parse_server("demo", value.clone()).unwrap();
        assert_eq!(serde_json::to_value(server).unwrap(), value);
        for invalid in [
            json!({"cwd": ""}),
            json!({"cwd": "bad\0path"}),
            json!({"env": {"": "value"}}),
            json!({"env": {"1KEY": "value"}}),
            json!({"env": {"KEY": "bad\0value"}}),
            json!({"env": {"KEY": 1}}),
            json!({"env": []}),
        ] {
            let mut value = json!({"command": "python3"});
            value
                .as_object_mut()
                .unwrap()
                .extend(invalid.as_object().unwrap().clone());
            assert!(parse_server("demo", value).is_err());
        }
        assert!(
            parse_server(
                "remote",
                json!({
                    "url": "https://example.com/mcp", "env": {"KEY": "value"}
                })
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_value_map_diagnostics_do_not_disclose_values() {
        for (field, base) in [
            ("env", json!({"command": "python3"})),
            ("headers", json!({"url": "https://example.test/mcp"})),
        ] {
            for value in [
                json!({"PIN": 731982654}),
                json!([731982654]),
                json!(731982654),
            ] {
                let mut definition = base.clone();
                definition[field] = value;
                let error = parse_server("demo", definition)
                    .err()
                    .expect("non-string maps are rejected");
                let diagnostic = format!("{error:#}");
                assert!(diagnostic.contains("values must map names to strings"));
                assert!(!diagnostic.contains("731982654"));
            }
        }
    }

    #[test]
    fn headers_round_trip_without_resolving_credentials_and_are_http_only() {
        let value = json!({"url": "https://example.test/mcp", "headers": {"X-Key": "${UNSET}"}});
        let server = parse_server("remote", value.clone()).unwrap();
        assert_eq!(serde_json::to_value(server).unwrap(), value);
        assert!(
            parse_server(
                "local",
                json!({"command": "python3", "headers": {"X-Key": "value"}})
            )
            .is_err()
        );
        assert!(
            parse_server(
                "remote",
                json!({"url": "https://example.test/mcp", "headers": {"Host": "other.test"}})
            )
            .is_err()
        );
    }

    #[test]
    fn remote_config_requires_http_url_and_named_ambient_token() {
        let root = std::env::temp_dir().join(format!("ion-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        let remote = |url: &str, bearer_token_env: Option<&str>| {
            McpServer::Http(McpHttpServer {
                enabled: true,
                url: url.into(),
                bearer_token_env: bearer_token_env.map(str::to_owned),
                headers: Default::default(),
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            })
        };
        assert!(
            config
                .add("bad-scheme", remote("file:///tmp/mcp", None))
                .is_err()
        );
        assert!(
            config
                .add("bad-env", remote("https://example.com/mcp", Some("1KEY")))
                .is_err()
        );
        config
            .add("remote", remote("https://example.com/mcp", Some("MCP_KEY")))
            .unwrap();
        assert!(
            matches!(&config.list().unwrap().servers["remote"], McpServer::Http(server) if server.bearer_token_env.as_deref() == Some("MCP_KEY"))
        );
        assert!(
            fs::read_to_string(&config.user)
                .unwrap()
                .contains("MCP_KEY")
        );
        config.remove("remote").unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_config_is_inert_until_matching_trust_and_overrides_user_entries() {
        let root = std::env::temp_dir().join(format!("ion-mcp-project-{}", uuid::Uuid::now_v7()));
        let project_root = root.join("project");
        fs::create_dir_all(project_root.join(".git")).unwrap();
        let config = McpConfig::with_project(&root.join("config"), &project_root).unwrap();
        let stdio = |command: &str| {
            McpServer::Stdio(McpStdioServer {
                enabled: true,
                cwd: None,
                env: Default::default(),
                command: command.into(),
                args: Vec::new(),
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            })
        };

        McpConfig::new(&root.join("config"))
            .add("shared", stdio("user-command"))
            .unwrap();
        config
            .add_project("shared", stdio("project-command"))
            .unwrap();
        config
            .add_project("project-only", stdio("project-only"))
            .unwrap();

        let untrusted = config.list().unwrap();
        assert_eq!(untrusted.servers.len(), 1);
        assert!(
            matches!(&untrusted.servers["shared"], McpServer::Stdio(server) if server.command == "user-command")
        );
        assert!(
            untrusted
                .diagnostics
                .iter()
                .any(|message| message.contains("not trusted")),
            "{:?}",
            untrusted.diagnostics
        );

        config.trust_project().unwrap();
        let trusted = config.list().unwrap();
        assert_eq!(trusted.servers.len(), 2);
        assert!(
            matches!(&trusted.servers["shared"], McpServer::Stdio(server) if server.command == "project-command")
        );
        assert!(trusted.servers.contains_key("project-only"));

        config
            .add_project("project-two", stdio("project-two"))
            .unwrap();
        let changed = config.list().unwrap();
        assert_eq!(changed.servers.len(), 1);
        assert!(
            changed
                .diagnostics
                .iter()
                .any(|message| message.contains("not trusted")),
            "{:?}",
            changed.diagnostics
        );

        config.trust_project().unwrap();
        assert_eq!(config.list().unwrap().servers.len(), 3);
        config.untrust_project().unwrap();
        assert_eq!(config.list().unwrap().servers.len(), 1);

        fs::write(
            &config.project.as_ref().unwrap().path,
            br#"{"servers":{"bad":{"command":7}}}"#,
        )
        .unwrap();
        assert!(config.trust_project().is_err());
        assert_eq!(config.list().unwrap().servers.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_server_entry_does_not_hide_valid_startup_servers_or_get_discarded() {
        let root = std::env::temp_dir().join(format!("ion-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        fs::write(
            &config.user,
            br#"{"servers":{"good":{"command":"echo","args":["ready"]},"bad":{"command":7}}}"#,
        )
        .unwrap();

        let (servers, diagnostics) = config.load_startup().unwrap();
        assert_eq!(servers.len(), 1);
        assert!(matches!(&servers["good"], McpServer::Stdio(server) if server.command == "echo"));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("invalid MCP server \"bad\""));
        assert!(diagnostics[0].contains("expected a string"));
        let listing = config.list().unwrap();
        assert_eq!(listing.servers.len(), 1);
        assert!(listing.servers.contains_key("good"));
        assert_eq!(listing.diagnostics.len(), 1);

        config.remove("good").unwrap();
        config
            .add(
                "new",
                McpServer::Stdio(McpStdioServer {
                    enabled: true,
                    cwd: None,
                    env: Default::default(),
                    command: "echo".into(),
                    args: Vec::new(),
                    exposure: McpExposure::Direct,
                    tool_exposure: Default::default(),
                }),
            )
            .unwrap();
        let saved = fs::read_to_string(&config.user).unwrap();
        assert!(saved.contains("\"bad\""));
        assert!(saved.contains("\"new\""));
        assert!(!saved.contains("\"good\""));

        // Invalid entries are still addressable by name, so the user can
        // repair the file without hand-editing unrelated JSON.
        config.remove("bad").unwrap();
        let listing = config.list().unwrap();
        assert_eq!(listing.servers.len(), 1);
        assert!(listing.servers.contains_key("new"));
        assert!(listing.diagnostics.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_configuration_mutations_preserve_acknowledged_updates() {
        let root = std::env::temp_dir().join(format!("ion-mcp-config-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        let server = || {
            McpServer::Stdio(McpStdioServer {
                enabled: true,
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: Vec::new(),
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            })
        };
        for index in 0..8 {
            config.add(&format!("old-{index}"), server()).unwrap();
        }
        let start = Arc::new(Barrier::new(16));
        let workers = (0..16)
            .map(|index| {
                let root = root.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    let config = McpConfig::new(&root);
                    start.wait();
                    if index < 8 {
                        config.add(&format!("new-{index}"), server())
                    } else {
                        config.remove(&format!("old-{}", index - 8))
                    }
                })
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        let saved = config.list().unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        assert!(saved.diagnostics.is_empty());
        assert_eq!(
            saved.servers.keys().cloned().collect::<Vec<_>>(),
            (0..8)
                .map(|index| format!("new-{index}"))
                .collect::<Vec<_>>(),
            "acknowledged add/remove updates were lost or resurrected"
        );
    }
}
