//! Explicit executable user commands: strict inert manifests, project trust,
//! literal argv mapping, and source revalidation before dispatch.
//!
//! Discovering or opening a repository never launches a configured program.
//! Only an explicit `SessionControl::command` invocation maps one trusted entry
//! onto the existing direct-user-shell route.

use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{command_args::quote, file_io::read_bounded, model_setup::write_json};

const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_DESCRIPTION_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOrigin {
    User,
    Project,
}

impl CommandOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

impl std::fmt::Display for CommandOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
struct CommandSource {
    path: PathBuf,
    digest: String,
    project: Option<ProjectSource>,
}

#[derive(Debug, Clone)]
struct ProjectSource {
    root: PathBuf,
    manifest: PathBuf,
    trust: PathBuf,
}

/// One validated, inert command definition. Its fixed argv is data, not a
/// second shell or interpolation language.
#[derive(Debug, Clone)]
pub struct UserCommand {
    pub name: String,
    pub description: String,
    pub program: String,
    pub args: Vec<String>,
    pub share_with_model: bool,
    pub origin: CommandOrigin,
    source: CommandSource,
}

impl UserCommand {
    fn verify_source(&self) -> Result<()> {
        let bytes = read_bounded(&self.source.path, MAX_MANIFEST_BYTES).with_context(|| {
            format!(
                "cannot read command source {}; the loaded definition is no longer current",
                self.source.path.display()
            )
        })?;
        ensure!(
            digest(&bytes) == self.source.digest,
            "command source {} changed since it was loaded; run /reload after reviewing the change",
            self.source.path.display()
        );
        if let Some(project) = &self.source.project {
            let root = project_key(&project.root)?;
            let manifest = manifest_identity(&project.manifest)?;
            ensure!(
                trusted_project(&project.trust, &root, &manifest, &self.source.digest)
                    .with_context(|| format!(
                        "cannot verify command trust {}",
                        project.trust.display()
                    ))?,
                "project commands manifest {} is no longer trusted for its current contents; run `ion commands trust` and /reload",
                project.manifest.display()
            );
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct MappedCommand {
    pub command: String,
    pub share_with_model: bool,
}

/// The frozen catalog published with one Session binding. Entries retain their
/// manifest identity and digest so dispatch can reject stale authority.
#[derive(Debug, Clone, Default)]
pub struct CommandCatalog {
    commands: BTreeMap<String, UserCommand>,
    diagnostics: Vec<String>,
}

impl CommandCatalog {
    /// Load inert user commands plus a trusted project layer. No configured
    /// program is started by discovery, trust or listing.
    pub fn load(cwd: &Path, config_root: &Path) -> Result<Self> {
        CommandConfig::with_project(config_root, cwd)?.list()
    }

    pub fn iter(&self) -> impl Iterator<Item = &UserCommand> {
        self.commands.values()
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub(crate) fn resolve(&self, name: &str, args: &[String]) -> Result<MappedCommand> {
        let command = self.commands.get(name).ok_or_else(|| {
            anyhow!("unknown user command /command:{name}; type /commands to list enabled commands")
        })?;
        ensure!(
            !args.iter().any(|arg| arg.contains('\0')),
            "command arguments cannot contain NUL"
        );
        command.verify_source()?;
        let mut mapped = quote(&command.program);
        for value in command.args.iter().chain(args) {
            mapped.push(' ');
            mapped.push_str(&quote(value));
        }
        Ok(MappedCommand {
            command: mapped,
            share_with_model: command.share_with_model,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    #[serde(default)]
    projects: BTreeMap<String, TrustEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustEntry {
    manifest: String,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    commands: BTreeMap<String, RawCommand>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    description: String,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    share_with_model: bool,
}

struct ProjectConfig {
    root: PathBuf,
    manifest: PathBuf,
}

/// Configuration paths for user and project command manifests. Project entries
/// use `commands-trust.json`, never MCP's trust store or grants.
pub struct CommandConfig {
    user: PathBuf,
    project: Option<ProjectConfig>,
    trust: PathBuf,
}

impl CommandConfig {
    pub fn new(config_root: &Path) -> Self {
        Self {
            user: config_root.join("commands.json"),
            project: None,
            trust: config_root.join("commands-trust.json"),
        }
    }

    /// Project manifest at the nearest Git ancestor (or canonical `cwd`), the
    /// same root policy as MCP. Project entries remain inert until trusted.
    pub fn with_project(config_root: &Path, cwd: &Path) -> Result<Self> {
        let root = project_root(cwd)?;
        Ok(Self {
            user: config_root.join("commands.json"),
            project: Some(ProjectConfig {
                manifest: root.join(".ion/commands.json"),
                root,
            }),
            trust: config_root.join("commands-trust.json"),
        })
    }

    pub fn project_path(&self) -> Option<&Path> {
        self.project
            .as_ref()
            .map(|project| project.manifest.as_path())
    }

    pub fn list(&self) -> Result<CommandCatalog> {
        let mut commands = BTreeMap::new();
        let mut diagnostics = Vec::new();
        match read_manifest(&self.user) {
            Ok(Some((bytes, path))) => {
                let digest = digest(&bytes);
                parse_entries(
                    &bytes,
                    &path,
                    CommandOrigin::User,
                    digest,
                    None,
                    &mut commands,
                    &mut diagnostics,
                );
            }
            Ok(None) => {}
            Err(error) => diagnostics.push(format!(
                "cannot read user commands manifest {}: {error:#}",
                self.user.display()
            )),
        }
        if let Some(project) = &self.project {
            match read_manifest(&project.manifest) {
                Ok(Some((bytes, path))) => {
                    let digest = digest(&bytes);
                    match project_trust_state(self, project, &digest) {
                        Ok(true) => parse_entries(
                            &bytes,
                            &path,
                            CommandOrigin::Project,
                            digest,
                            Some(ProjectSource {
                                root: project.root.clone(),
                                manifest: project.manifest.clone(),
                                trust: self.trust.clone(),
                            }),
                            &mut commands,
                            &mut diagnostics,
                        ),
                        Ok(false) => diagnostics.push(format!(
                            "project commands manifest {} is not trusted for its current contents; run `ion commands trust`",
                            project.manifest.display()
                        )),
                        Err(error) => diagnostics.push(format!(
                            "cannot read commands trust store {}: {error:#}",
                            self.trust.display()
                        )),
                    }
                }
                Ok(None) => {}
                Err(error) => diagnostics.push(format!(
                    "cannot read project commands manifest {}: {error:#}",
                    project.manifest.display()
                )),
            }
        }
        Ok(CommandCatalog {
            commands,
            diagnostics,
        })
    }

    /// Trust the exact current project manifest after validating the whole
    /// document. Editing any byte invalidates the grant.
    pub fn trust_project(&self) -> Result<PathBuf> {
        let project = self
            .project
            .as_ref()
            .context("no project commands manifest")?;
        let (bytes, path) = read_manifest(&project.manifest)?.with_context(|| {
            format!(
                "cannot read project commands manifest {}",
                project.manifest.display()
            )
        })?;
        let mut commands = BTreeMap::new();
        let mut diagnostics = Vec::new();
        parse_entries(
            &bytes,
            &path,
            CommandOrigin::Project,
            digest(&bytes),
            None,
            &mut commands,
            &mut diagnostics,
        );
        ensure!(
            diagnostics.is_empty(),
            "cannot trust invalid project commands manifest: {}",
            diagnostics.join("; ")
        );
        let root = project_key(&project.root)?;
        let manifest = manifest_identity(&project.manifest)?;
        let _lock = self.writer_lock()?;
        let mut trust = self.read_trust()?;
        trust.projects.insert(
            root,
            TrustEntry {
                manifest,
                digest: digest(&bytes),
            },
        );
        self.write_trust(&trust)?;
        Ok(project.manifest.clone())
    }

    pub fn untrust_project(&self) -> Result<()> {
        let project = self
            .project
            .as_ref()
            .context("no project commands manifest")?;
        let root = project_key(&project.root)?;
        let _lock = self.writer_lock()?;
        let mut trust = self.read_trust()?;
        ensure!(
            trust.projects.remove(&root).is_some(),
            "project commands manifest is not trusted"
        );
        self.write_trust(&trust)
    }

    /// The trust reader is capped at the manifest budget. Validate the exact
    /// atomic-write encoding too, so accumulated canonical roots cannot publish
    /// a store that this same owner would refuse to read or modify later.
    fn write_trust(&self, trust: &TrustFile) -> Result<()> {
        let encoded =
            serde_json::to_vec_pretty(trust).context("cannot encode commands trust store")?;
        ensure!(
            encoded.len() <= MAX_MANIFEST_BYTES,
            "commands trust store would exceed the {MAX_MANIFEST_BYTES} byte limit; untrust unused projects first"
        );
        write_json(&self.trust, trust)
    }

    fn read_trust(&self) -> Result<TrustFile> {
        match read_bounded(&self.trust, MAX_MANIFEST_BYTES) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid commands trust store {}", self.trust.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TrustFile::default()),
            Err(error) => Err(error).with_context(|| {
                format!("cannot read commands trust store {}", self.trust.display())
            }),
        }
    }

    fn writer_lock(&self) -> Result<File> {
        let parent = self
            .trust
            .parent()
            .context("commands trust has no parent")?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let lock = File::from(rustix::fs::open(
            parent.join("commands.lock"),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )?);
        ensure!(
            lock.metadata()?.is_file(),
            "commands lock is not a regular file"
        );
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(lock)
    }
}

fn read_manifest(path: &Path) -> Result<Option<(Vec<u8>, PathBuf)>> {
    match read_bounded(path, MAX_MANIFEST_BYTES) {
        Ok(bytes) => Ok(Some((bytes, path.to_owned()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("1 MiB regular-file read failed"),
    }
}

fn parse_entries(
    bytes: &[u8],
    path: &Path,
    origin: CommandOrigin,
    source_digest: String,
    project: Option<ProjectSource>,
    commands: &mut BTreeMap<String, UserCommand>,
    diagnostics: &mut Vec<String>,
) {
    let manifest: Manifest = match serde_json::from_slice(bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            diagnostics.push(format!(
                "invalid {} commands manifest {}: {error}",
                origin,
                path.display()
            ));
            return;
        }
    };
    for (name, raw) in manifest.commands {
        match validate_command(&name, &raw) {
            Ok(()) => {
                commands.insert(
                    name.clone(),
                    UserCommand {
                        name,
                        description: raw.description,
                        program: raw.program,
                        args: raw.args,
                        share_with_model: raw.share_with_model,
                        origin,
                        source: CommandSource {
                            path: path.to_owned(),
                            digest: source_digest.clone(),
                            project: project.clone(),
                        },
                    },
                );
            }
            Err(error) => diagnostics.push(format!(
                "invalid {} command in {}: {error:#}",
                origin,
                path.display()
            )),
        }
    }
}

fn validate_command(name: &str, raw: &RawCommand) -> Result<()> {
    ensure!(
        valid_name(name),
        "command name must be 1-64 ASCII letters, digits, '-' or '_'"
    );
    ensure!(
        !raw.description.trim().is_empty() && raw.description.len() <= MAX_DESCRIPTION_BYTES,
        "description must contain 1-{MAX_DESCRIPTION_BYTES} bytes"
    );
    ensure!(
        !raw.description.chars().any(char::is_control),
        "description must not contain control characters"
    );
    ensure!(
        !raw.program.trim().is_empty() && !raw.program.contains('\0'),
        "program must be nonempty and contain no NUL"
    );
    ensure!(
        !raw.args.iter().any(|arg| arg.contains('\0')),
        "arguments must not contain NUL"
    );
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
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

fn project_key(root: &Path) -> Result<String> {
    root.to_str()
        .map(str::to_owned)
        .context("project root is not representable as UTF-8 in commands-trust.json")
}

fn manifest_identity(manifest: &Path) -> Result<String> {
    fs::canonicalize(manifest)
        .with_context(|| format!("cannot resolve commands manifest {}", manifest.display()))?
        .to_str()
        .map(str::to_owned)
        .context("commands manifest path is not representable as UTF-8")
}

fn project_trust_state(
    config: &CommandConfig,
    project: &ProjectConfig,
    digest: &str,
) -> Result<bool> {
    let root = project_key(&project.root)?;
    let manifest = manifest_identity(&project.manifest)?;
    trusted_project(&config.trust, &root, &manifest, digest)
}

fn trusted_project(trust: &Path, root: &str, manifest: &str, digest: &str) -> Result<bool> {
    let file = match read_bounded(trust, MAX_MANIFEST_BYTES) {
        Ok(bytes) => serde_json::from_slice::<TrustFile>(&bytes)
            .with_context(|| format!("invalid commands trust store {}", trust.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot read commands trust store {}", trust.display()));
        }
    };
    Ok(file
        .projects
        .get(root)
        .is_some_and(|entry| entry.manifest == manifest && entry.digest == digest))
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Temp {
        root: PathBuf,
        config: PathBuf,
    }

    impl Temp {
        fn new(label: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("ion-commands-{label}-{}", uuid::Uuid::now_v7()));
            let config = root.join("config");
            fs::create_dir_all(&config).unwrap();
            Self { root, config }
        }
        fn project(&self, name: &str) -> PathBuf {
            let root = self.root.join(name);
            fs::create_dir_all(root.join(".git")).unwrap();
            root
        }
        fn write_project(&self, root: &Path, body: &str) {
            fs::create_dir_all(root.join(".ion")).unwrap();
            fs::write(root.join(".ion/commands.json"), body).unwrap();
        }
        fn write_user(&self, body: &str) {
            fs::write(self.config.join("commands.json"), body).unwrap();
        }
        fn config_for(&self, cwd: &Path) -> CommandConfig {
            CommandConfig::with_project(&self.config, cwd).unwrap()
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn manifest(description: &str, program: &str, share: bool) -> String {
        serde_json::json!({
            "commands": {
                "check": {
                    "description": description,
                    "program": program,
                    "args": ["fixed"],
                    "share_with_model": share
                }
            }
        })
        .to_string()
    }

    #[test]
    fn strict_manifest_validation_and_origin_precedence() {
        let temp = Temp::new("manifest");
        let root = temp.project("work");
        temp.write_user(&manifest("user check", "user-program", false));
        temp.write_project(&root, &manifest("project check", "project-program", true));
        let config = temp.config_for(&root);
        let untrusted = config.list().unwrap();
        assert_eq!(untrusted.iter().next().unwrap().origin, CommandOrigin::User);
        assert_eq!(untrusted.iter().next().unwrap().description, "user check");
        assert!(
            untrusted
                .diagnostics()
                .iter()
                .any(|message| message.contains("not trusted"))
        );

        config.trust_project().unwrap();
        let trusted = config.list().unwrap();
        assert_eq!(
            trusted.iter().next().unwrap().origin,
            CommandOrigin::Project
        );
        assert_eq!(trusted.iter().next().unwrap().description, "project check");
        assert!(trusted.diagnostics().is_empty());

        for invalid in [
            r#"{"commands":{"check":{"description":"d","program":"p","args":[],"share_with_model":false,"extra":1}}}"#,
            r#"{"commands":{"check":{"description":"d","program":"p","args":[]}}}"#,
            r#"{"commands":{"bad name":{"description":"d","program":"p","args":[],"share_with_model":false}}}"#,
            r#"{"commands":{"check":{"description":"d\u0000","program":"p","args":[],"share_with_model":false}}}"#,
            r#"{"commands":{"check":{"description":"d","program":"p","args":["x\u0000"],"share_with_model":false}}}"#,
        ] {
            fs::write(root.join(".ion/commands.json"), invalid).unwrap();
            assert!(config.trust_project().is_err(), "{invalid}");
        }
    }

    #[test]
    fn trust_is_root_scoped_and_independent_of_mcp() {
        let temp = Temp::new("roots");
        let first = temp.project("first");
        let second = temp.project("second");
        let body = manifest("check", "program", false);
        temp.write_project(&first, &body);
        temp.write_project(&second, &body);
        let first_config = temp.config_for(&first);
        first_config.trust_project().unwrap();
        assert!(first_config.list().unwrap().iter().next().is_some());
        assert!(
            temp.config_for(&second)
                .list()
                .unwrap()
                .iter()
                .next()
                .is_none()
        );
        // An MCP trust store entry with the same root grants nothing here.
        let root_key = first.to_str().unwrap();
        fs::write(
            temp.config.join("mcp-trust.json"),
            serde_json::json!({"projects": {root_key: "whatever"}}).to_string(),
        )
        .unwrap();
        first_config.untrust_project().unwrap();
        assert!(first_config.list().unwrap().iter().next().is_none());
        let trust: TrustFile =
            serde_json::from_slice(&fs::read(temp.config.join("commands-trust.json")).unwrap())
                .unwrap();
        assert!(trust.projects.is_empty());
    }

    #[test]
    fn stale_or_revoked_source_is_rejected_before_shell_mapping() {
        let temp = Temp::new("stale");
        let root = temp.project("work");
        temp.write_project(&root, &manifest("check", "program", false));
        let config = temp.config_for(&root);
        config.trust_project().unwrap();
        let catalog = config.list().unwrap();
        assert!(catalog.resolve("check", &[]).is_ok());

        temp.write_project(&root, &manifest("edited", "program", false));
        let error = catalog.resolve("check", &[]).unwrap_err().to_string();
        assert!(error.contains("changed since it was loaded"));

        let refreshed = config.list().unwrap();
        assert!(refreshed.iter().next().is_none());
        config.untrust_project().unwrap();
        let untrusted = temp.config_for(&root).list().unwrap();
        assert!(untrusted.iter().next().is_none());
    }

    #[test]
    fn manifest_size_and_nul_program_are_bounded_before_parse() {
        let temp = Temp::new("bounded");
        let root = temp.project("work");
        let oversized = format!(
            "{{\"commands\":{{\"check\":{{\"description\":\"{}\",\"program\":\"p\",\"args\":[],\"share_with_model\":false}}}}}}",
            "x".repeat(MAX_MANIFEST_BYTES)
        );
        temp.write_project(&root, &oversized);
        let config = temp.config_for(&root);
        let listing = config.list().unwrap();
        assert!(listing.iter().next().is_none());
        assert!(listing.diagnostics().iter().any(|d| d.contains("1 MiB")));
        assert!(config.trust_project().is_err());

        temp.write_project(
            &root,
            r#"{"commands":{"check":{"description":"d","program":"p\u0000","args":[],"share_with_model":false}}}"#,
        );
        assert!(config.trust_project().is_err());
    }

    #[test]
    fn explicit_invocation_quotes_argv_and_never_runs_discovery() {
        let temp = Temp::new("quote");
        let root = temp.project("work");
        temp.write_project(&root, &manifest("check", "program", false));
        let config = temp.config_for(&root);
        config.trust_project().unwrap();
        let catalog = config.list().unwrap();
        let mapped = catalog
            .resolve(
                "check",
                &["a b".into(), "$(touch nope)".into(), "x'y".into()],
            )
            .unwrap();
        assert_eq!(
            mapped.command,
            "'program' 'fixed' 'a b' '$(touch nope)' 'x'\\''y'"
        );
        assert!(!mapped.share_with_model);
        assert!(!temp.root.join("nope").exists());
        assert!(catalog.resolve("missing", &[]).is_err());
    }

    #[test]
    fn oversized_trust_encoding_is_refused_without_replacing_the_old_store() {
        let temp = Temp::new("trust-bound");
        let config = CommandConfig::new(&temp.config);
        let original = br#"{"projects":{}}"#;
        fs::write(temp.config.join("commands-trust.json"), original).unwrap();
        let mut trust = TrustFile::default();
        trust.projects.insert(
            "r".repeat(MAX_MANIFEST_BYTES),
            TrustEntry {
                manifest: "manifest".into(),
                digest: "digest".into(),
            },
        );
        assert!(config.write_trust(&trust).is_err());
        assert_eq!(
            fs::read(temp.config.join("commands-trust.json")).unwrap(),
            original
        );
    }

    #[test]
    fn first_invocation_is_the_only_launch_point() {
        let temp = Temp::new("inert");
        let root = temp.project("work");
        let marker = temp.root.join("launched");
        let program = temp.root.join("program.sh");
        fs::write(
            &program,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        temp.write_project(&root, &manifest("check", program.to_str().unwrap(), false));
        let config = temp.config_for(&root);
        assert!(!marker.exists(), "discovery must not launch the program");
        config.trust_project().unwrap();
        assert!(!marker.exists(), "trust must not launch the program");
        let catalog = config.list().unwrap();
        assert!(!marker.exists(), "listing must not launch the program");
        catalog.resolve("check", &[]).unwrap();
        assert!(!marker.exists(), "argv mapping must not launch the program");
    }
}
