//! Host-side discovery for local Sessions. Conversation facts stay in SQLite.

use std::{
    fs::{self, DirBuilder},
    io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use ion_ai::ModelRef;
use ion_core::{Session, SessionInputPreview};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct SessionCatalog {
    root: PathBuf,
    cwd: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: String,
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
    pub name: Option<String>,
    pub preview: Option<String>,
    pub model: Option<ModelRef>,
    pub turns: usize,
    pub updated: SystemTime,
}

impl SessionCatalog {
    pub fn new(root: PathBuf, cwd: Option<PathBuf>) -> Self {
        Self { root, cwd }
    }

    /// Concrete workspace directory; inspecting it creates no files.
    pub(crate) fn directory(&self) -> PathBuf {
        let key = self.cwd.as_ref().map_or_else(
            || "workspace-free".to_owned(),
            |cwd| format!("{:x}", Sha256::digest(cwd.as_os_str().as_encoded_bytes())),
        );
        self.root.join(key)
    }

    pub(crate) fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn new_path(&self) -> Result<PathBuf> {
        let directory = self.directory();
        if !directory.exists() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&directory)
                .with_context(|| format!("cannot create {}", directory.display()))?;
        }
        for _ in 0..16 {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes)
                .map_err(|error| anyhow::anyhow!("cannot generate session ID: {error}"))?;
            let id = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = directory.join(format!("{id}.sqlite"));
            if !path.exists() {
                return Ok(path);
            }
        }
        bail!("could not allocate a unique session ID")
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        self.scan(&CancellationToken::new())
            .map(|sessions| sessions.expect("an uncancelled scan completes"))
    }

    /// Scan while retaining cancellation as a distinct outcome. Checks occur
    /// between directory entries and Session metadata reads; in-flight filesystem
    /// calls remain uninterruptible.
    pub fn scan(&self, stop: &CancellationToken) -> Result<Option<Vec<SessionSummary>>> {
        let Some(paths) = self.paths_until(Some(stop))? else {
            return Ok(None);
        };
        self.scan_paths(paths, false, stop)
    }

    /// Read coding Sessions across workspace namespaces, excluding workspace-free
    /// applications and nested managed children. This never changes the catalog's
    /// namespace or grants authority to execute in a discovered directory.
    pub fn scan_all(&self, stop: &CancellationToken) -> Result<Option<Vec<SessionSummary>>> {
        if stop.is_cancelled() {
            return Ok(None);
        }
        let mut paths = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return self.scan_paths(Vec::new(), true, stop);
            }
            Err(error) => return Err(error).context("cannot list Session workspaces"),
        };
        let mut entries = entries;
        loop {
            if stop.is_cancelled() {
                return Ok(None);
            }
            let Some(entry) = entries.next() else { break };
            let entry = entry?;
            if stop.is_cancelled() {
                return Ok(None);
            }
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(workspace_paths) = paths_in(&entry.path(), Some(stop))? else {
                return Ok(None);
            };
            paths.extend(workspace_paths);
        }
        self.scan_paths(paths, true, stop)
    }

    fn scan_paths(
        &self,
        paths: Vec<PathBuf>,
        all_workspaces: bool,
        stop: &CancellationToken,
    ) -> Result<Option<Vec<SessionSummary>>> {
        let mut sessions = Vec::new();
        for path in paths {
            if stop.is_cancelled() {
                return Ok(None);
            }
            if let Some(summary) = self.summary(path, all_workspaces) {
                sessions.push(summary);
            }
        }
        if stop.is_cancelled() {
            return Ok(None);
        }
        sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
        Ok(Some(sessions))
    }

    fn paths(&self) -> Result<Vec<PathBuf>> {
        self.paths_until(None)
            .map(|paths| paths.expect("an uncancelled directory scan completes"))
    }

    fn paths_until(&self, stop: Option<&CancellationToken>) -> Result<Option<Vec<PathBuf>>> {
        paths_in(&self.directory(), stop)
    }

    fn summary(&self, path: PathBuf, all_workspaces: bool) -> Option<SessionSummary> {
        summary(path).filter(|summary| {
            if all_workspaces {
                summary.cwd.is_some()
            } else {
                summary.cwd == self.cwd
            }
        })
    }

    pub fn latest(&self) -> Result<PathBuf> {
        let mut recent = self
            .paths()?
            .into_iter()
            .filter_map(|path| modified(&path).ok().map(|updated| (path, updated)))
            .collect::<Vec<_>>();
        recent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
        recent
            .into_iter()
            .find_map(|(path, _)| self.summary(path, false))
            .map(|session| session.path)
            .context("no saved session for this directory")
    }

    pub fn by_id(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty() {
            bail!("session ID is empty");
        }
        let mut matches = self
            .paths()?
            .into_iter()
            .filter(|path| {
                Session::inspect_id(path).is_ok_and(|identity| identity.to_string().starts_with(id))
            })
            .filter_map(|path| self.summary(path, false));
        let first = matches.next().context("session ID was not found")?;
        if matches.next().is_some() {
            bail!("session ID is ambiguous; use more characters")
        }
        Ok(first.path)
    }

    pub fn resolve_explicit(&self, value: PathBuf) -> Result<PathBuf> {
        if value.exists() || value.components().count() > 1 || value.extension().is_some() {
            Ok(value)
        } else {
            self.by_id(&value.to_string_lossy())
        }
    }
}

fn paths_in(directory: &Path, stop: Option<&CancellationToken>) -> Result<Option<Vec<PathBuf>>> {
    let cancelled = || stop.is_some_and(CancellationToken::is_cancelled);
    if cancelled() {
        return Ok(None);
    }
    let mut paths = Vec::new();
    match fs::read_dir(directory) {
        Ok(mut entries) => loop {
            if cancelled() {
                return Ok(None);
            }
            // `for` would traverse before checking cancellation. Check first,
            // then surface an observed traversal error before any later stop.
            let Some(entry) = entries.next() else {
                break;
            };
            let entry = entry?;
            if cancelled() {
                return Ok(None);
            }
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "sqlite") && entry.file_type()?.is_file() {
                paths.push(path);
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot list sessions"),
    }
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(paths))
}

fn summary(path: PathBuf) -> Option<SessionSummary> {
    // Discovery must remain usable if one file is damaged or only
    // partially initialized. Opening that path explicitly still reports
    // its real error.
    let view = Session::inspect_metadata(&path).ok()?;
    let updated = modified(&path).ok()?;
    let preview = view.first_input.map(|input| {
        let label = match input {
            SessionInputPreview::Prompt { text } => text.unwrap_or_else(|| "[image]".into()),
            SessionInputPreview::Shell { command, excluded } => {
                format!("{}{command}", if excluded { "!!" } else { "!" })
            }
        };
        label
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect()
    });
    preview.as_ref()?;
    let id = view.id.to_string();
    Some(SessionSummary {
        id,
        path,
        cwd: view.cwd,
        name: view.name,
        preview,
        model: view.last_model,
        turns: view.turns,
        updated,
    })
}

fn modified(path: &Path) -> Result<SystemTime> {
    let database = fs::metadata(path)?.modified()?;
    let wal = path.with_extension("sqlite-wal");
    match fs::metadata(wal) {
        Ok(metadata) => Ok(database.max(metadata.modified()?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(database),
        Err(error) => Err(error).context("cannot inspect session activity"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use ion_ai::{
        Content, Message, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
        ScriptedModelService, Usage,
    };
    use ion_core::{Agent, AgentLimits};

    use crate::LocalTools;
    use tokio_util::sync::CancellationToken;

    fn fixture() -> (PathBuf, SessionCatalog) {
        let root = std::env::temp_dir().join(format!("ion-catalog-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let catalog =
            SessionCatalog::new(root.join("state"), Some(workspace.canonicalize().unwrap()));
        (root, catalog)
    }

    fn agent(catalog: &SessionCatalog, responses: usize) -> Agent {
        Agent::new(
            Arc::new(ScriptedModelService::new((0..responses).map(|_| {
                Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None,
                    },
                    usage: Usage::unknown(),
                    termination: ResponseTermination::Completed,
                    returned_model: None,
                })])
            }))),
            Arc::new(LocalTools::new(catalog.cwd.as_deref().unwrap()).unwrap()),
            ModelRef {
                provider: "script".into(),
                model: "script".into(),
            },
        )
        .with_limits(AgentLimits {
            image_input: true,
            ..AgentLimits::default()
        })
    }

    fn assert_passive_preview(catalog: &SessionCatalog, path: &Path, preview: &str, turns: usize) {
        let before = Session::inspect(path).unwrap().entries;
        for reopen in [false, true] {
            let reopened = reopen.then(|| Session::open(path).unwrap());
            let listed = catalog.list().unwrap();
            let summary = listed.iter().find(|summary| summary.path == path).unwrap();
            assert_eq!(summary.preview.as_deref(), Some(preview));
            assert_eq!(summary.turns, turns);
            assert_eq!(Session::inspect(path).unwrap().entries, before);
            drop(reopened);
        }
    }

    #[tokio::test]
    async fn empty_sessions_do_not_displace_recent_work() {
        let (root, catalog) = fixture();
        assert!(catalog.list().unwrap().is_empty());
        let first = catalog.new_path().unwrap();
        let second = catalog.new_path().unwrap();
        assert_ne!(first, second);
        let session = Arc::new(Session::create(&first, catalog.cwd.as_deref()).unwrap());
        session.set_name(Some("First task")).unwrap();
        agent(&catalog, 1)
            .submit(
                session.clone(),
                "first prompt".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        drop(session);
        let session = Session::create(&second, catalog.cwd.as_deref()).unwrap();
        session.set_name(Some("Second task")).unwrap();
        drop(session);
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        let id = &listed[0].id;
        assert_eq!(catalog.by_id(id).unwrap(), listed[0].path);
        assert_eq!(catalog.latest().unwrap(), listed[0].path);
        assert_eq!(listed[0].name.as_deref(), Some("First task"));
        fs::write(first.with_file_name("damaged.sqlite"), b"partial database").unwrap();
        assert_eq!(catalog.list().unwrap().len(), 1);
        assert_eq!(catalog.latest().unwrap(), listed[0].path);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn first_input_previews_include_images_and_preserve_bounded_text() {
        let (root, catalog) = fixture();
        let image: ion_ai::ImageContent = serde_json::from_value(serde_json::json!({
            "mime_type": "image/png",
            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        })).unwrap();
        let text = format!("\t{}Z\nSECOND_LINE", "🦀é界".repeat(40));
        let expected = format!(
            " {}",
            "🦀é界".repeat(40).chars().take(119).collect::<String>()
        );
        for (content, preview) in [
            (vec![Content::Image(image.clone())], "[image]".to_owned()),
            (
                vec![Content::Text(text.clone()), Content::Image(image)],
                expected.clone(),
            ),
            (vec![Content::Text(text)], expected),
        ] {
            let path = catalog.new_path().unwrap();
            let session = Arc::new(Session::create(&path, catalog.cwd.as_deref()).unwrap());
            let agent = agent(&catalog, 2);
            agent
                .submit_message(
                    session.clone(),
                    Message {
                        role: Role::User,
                        content,
                        provider_replay: None,
                    },
                    String::new(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
            agent
                .submit(
                    session.clone(),
                    "latest prompt".into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
            session.shutdown().await.unwrap();
            drop(session);
            assert_passive_preview(&catalog, &path, &preview, 2);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn shell_previews_sanitize_controls_without_rewriting_native_evidence() {
        let (root, catalog) = fixture();
        let controls = "\u{1b}\u{85}\u{7f}";
        let command = format!(
            "printf '%s' '{controls}' #\t{}Z\n# SECOND_LINE",
            "🦀é界".repeat(40)
        );
        let label = format!(
            "printf '%s' '   ' # {}",
            "🦀é界".repeat(40).chars().take(100).collect::<String>()
        );
        for excluded in [false, true] {
            let path = catalog.new_path().unwrap();
            let session = Arc::new(Session::create(&path, catalog.cwd.as_deref()).unwrap());
            let tools = LocalTools::new(catalog.cwd.as_deref().unwrap()).unwrap();
            let output = session
                .run_user_shell(
                    command.clone(),
                    excluded,
                    CancellationToken::new(),
                    move |command, stop| async move { tools.run_user_shell(&command, stop).await },
                )
                .await
                .unwrap();
            assert!(!output.is_error, "{output:?}");
            assert_eq!(output.value["stdout"], controls);
            let entries = session.view().unwrap().entries;
            assert!(entries.iter().any(|entry| matches!(entry,
                ion_core::SessionEntry::UserShellAdmitted { command: raw, exclude_from_context }
                    if raw == &command && *exclude_from_context == excluded
            )));
            assert!(entries.iter().any(|entry| matches!(entry,
                ion_core::SessionEntry::UserShellSettled {
                    outcome: ion_core::UserShellOutcome::Observed { output: raw, is_error: false }, ..
                } if raw == &output.value
            )));
            agent(&catalog, 1)
                .submit(
                    session.clone(),
                    "latest prompt".into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
            session.shutdown().await.unwrap();
            drop(session);
            let preview = format!("{}{label}", if excluded { "!!" } else { "!" });
            assert_passive_preview(&catalog, &path, &preview, 1);
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn all_workspace_discovery_is_passive_and_excludes_non_root_coding_sessions() {
        let (root, catalog) = fixture();
        let other_cwd = root.join("other");
        fs::create_dir(&other_cwd).unwrap();
        let other =
            SessionCatalog::new(root.join("state"), Some(other_cwd.canonicalize().unwrap()));
        let workspace_free = SessionCatalog::new(root.join("state"), None);
        let child = catalog.directory().join("children/child.sqlite");
        fs::create_dir_all(child.parent().unwrap()).unwrap();
        let mut saved = Vec::new();
        for (path, cwd) in [
            (catalog.new_path().unwrap(), catalog.cwd.as_deref()),
            (other.new_path().unwrap(), other.cwd.as_deref()),
            (workspace_free.new_path().unwrap(), None),
            (child, catalog.cwd.as_deref()),
        ] {
            let session = Arc::new(Session::create(&path, cwd).unwrap());
            agent(&catalog, 1)
                .submit(
                    session.clone(),
                    "saved prompt".into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
            session.shutdown().await.unwrap();
            saved.push((path, session.view().unwrap().entries));
        }
        assert_eq!(catalog.list().unwrap().len(), 1);
        assert_eq!(workspace_free.list().unwrap().len(), 1);
        let all = catalog
            .scan_all(&CancellationToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|item| item.cwd == catalog.cwd));
        assert!(all.iter().any(|item| item.cwd == other.cwd));
        for (path, before) in saved {
            assert_eq!(Session::inspect(&path).unwrap().entries, before);
        }
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(catalog.scan_all(&stop).unwrap().is_none());
        let missing = SessionCatalog::new(root.join("absent"), None);
        assert_eq!(
            missing
                .scan_all(&CancellationToken::new())
                .unwrap()
                .unwrap()
                .len(),
            0
        );
        assert!(missing.scan_all(&stop).unwrap().is_none());
        assert!(!root.join("absent").exists());
        let broken = root.join("broken");
        fs::write(&broken, b"not a directory").unwrap();
        assert!(
            SessionCatalog::new(broken, None)
                .scan_all(&CancellationToken::new())
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn scan_distinguishes_completion_cancellation_and_directory_errors() {
        let (root, catalog) = fixture();
        let path = catalog.new_path().unwrap();
        let session = Arc::new(Session::create(&path, catalog.cwd.as_deref()).unwrap());
        agent(&catalog, 1)
            .submit(
                session.clone(),
                "saved prompt".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        session.shutdown().await.unwrap();
        drop(session);
        fs::write(catalog.directory().join("notes.txt"), b"not a session").unwrap();

        let complete = catalog
            .scan(&CancellationToken::new())
            .unwrap()
            .expect("uncancelled scan completes");
        assert_eq!(complete.len(), 1);
        assert_eq!(complete[0].path, path);
        assert_eq!(catalog.list().unwrap().len(), 1);
        let stop = CancellationToken::new();
        stop.cancel();
        assert!(catalog.scan(&stop).unwrap().is_none());

        let blocked = root.join("state-file");
        fs::write(&blocked, b"not a directory").unwrap();
        let error = SessionCatalog::new(blocked, None)
            .scan(&CancellationToken::new())
            .unwrap_err();
        assert!(format!("{error:#}").contains("cannot list sessions"));
        fs::remove_dir_all(root).unwrap();
    }
}
