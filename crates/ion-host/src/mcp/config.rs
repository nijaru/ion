//! MCP configuration validation and serialized atomic publication.
use super::http_config;
use crate::model_setup::{write_json, write_shared_json};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
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
    pub fn exposure(&self) -> McpExposure {
        match self {
            Self::Stdio(server) => server.exposure,
            Self::Http(server) => server.exposure,
        }
    }

    pub fn tool_exposure(&self) -> &BTreeMap<String, McpExposure> {
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
    /// Exact remote tool names, before Ion's alias normalization.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_exposure: BTreeMap<String, McpExposure>,
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
    /// Exact remote tool names, before Ion's alias normalization.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_exposure: BTreeMap<String, McpExposure>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServers {
    servers: BTreeMap<String, Value>,
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
            "user",
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
                                "project",
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
        let mut diagnostics = Vec::new();
        parse_servers(raw.servers, &mut servers, &mut diagnostics, "project");
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

    fn add_at(&self, path: &Path, name: &str, server: McpServer, shared: bool) -> Result<()> {
        validate_name(name)?;
        validate_server(&server)?;
        let _lock = writer_lock(path, shared)?;
        let mut servers = read_servers(path)?;
        ensure!(
            !servers.contains_key(name),
            "MCP server {name} already exists"
        );
        servers.insert(name.to_owned(), serde_json::to_value(server)?);
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

fn read_servers(path: &Path) -> Result<BTreeMap<String, Value>> {
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

fn parse_servers(
    raw: BTreeMap<String, Value>,
    servers: &mut BTreeMap<String, McpServer>,
    diagnostics: &mut Vec<String>,
    source: &str,
) {
    for (name, value) in raw {
        match parse_server(&name, value) {
            Ok(server) => {
                servers.insert(name, server);
            }
            Err(error) if source == "user" => diagnostics.push(format!("{error:#}")),
            Err(error) => diagnostics.push(format!("{source}: {error:#}")),
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

fn parse_server(name: &str, value: Value) -> Result<McpServer> {
    (|| -> Result<McpServer> {
        validate_name(name)?;
        let object = value.as_object().context("MCP server must be an object")?;
        let server = match (object.contains_key("command"), object.contains_key("url")) {
            (true, false) => McpServer::Stdio(serde_json::from_value(value)?),
            (false, true) => McpServer::Http(serde_json::from_value(value)?),
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
