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
use ion_core::{CodingSession, SessionEntry};
use sha2::{Digest, Sha256};

pub struct SessionCatalog {
    root: PathBuf,
    cwd: PathBuf,
    key: String,
}

pub struct SessionSummary {
    pub id: String,
    pub path: PathBuf,
    pub name: Option<String>,
    pub preview: Option<String>,
    pub model: Option<ModelRef>,
    pub turns: usize,
    pub updated: SystemTime,
}

impl SessionCatalog {
    pub fn new(root: PathBuf, cwd: PathBuf) -> Self {
        let key = format!("{:x}", Sha256::digest(cwd.as_os_str().as_encoded_bytes()));
        Self { root, cwd, key }
    }

    pub fn new_path(&self) -> Result<PathBuf> {
        let directory = self.root.join(&self.key);
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
        let mut sessions = self
            .paths()?
            .into_iter()
            .filter_map(|path| self.summary(path))
            .collect::<Vec<_>>();
        sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
        Ok(sessions)
    }

    fn paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        let directory = self.root.join(&self.key);
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "sqlite")
                        && entry.file_type()?.is_file()
                    {
                        paths.push(path);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("cannot list sessions"),
        }
        // The former one-session-per-directory path remains discoverable so
        // users do not lose their current work when new sessions become default.
        let legacy = self.root.join(format!("{}.sqlite", self.key));
        if legacy.is_file() {
            paths.push(legacy);
        }
        Ok(paths)
    }

    fn summary(&self, path: PathBuf) -> Option<SessionSummary> {
        // Discovery must remain usable if one file is damaged or only
        // partially initialized. Opening that path explicitly still reports
        // its real error.
        let view = CodingSession::inspect(&path).ok()?;
        if view.cwd != self.cwd {
            return None;
        }
        let updated = modified(&path).ok()?;
        let preview = view.entries.iter().find_map(|entry| match entry {
            SessionEntry::TurnStarted { prompt, .. } => Some(
                prompt
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect(),
            ),
            _ => None,
        });
        let turns = view
            .entries
            .iter()
            .filter(|entry| matches!(entry, SessionEntry::TurnStarted { .. }))
            .count();
        if turns == 0 {
            return None;
        }
        let id = path.file_stem()?.to_string_lossy().into_owned();
        Some(SessionSummary {
            id,
            path,
            name: view.name,
            preview,
            model: view.last_model,
            turns,
            updated,
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
            .find_map(|(path, _)| self.summary(path))
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
                path.file_stem()
                    .is_some_and(|stem| stem.to_string_lossy().starts_with(id))
            })
            .filter_map(|path| self.summary(path));
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
    use ion_core::{CodingAgent, LocalTools};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn empty_sessions_do_not_displace_recent_work() {
        let root = std::env::temp_dir().join(format!("ion-catalog-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let catalog = SessionCatalog::new(root.join("state"), workspace.clone());
        assert!(catalog.list().unwrap().is_empty());
        let first = catalog.new_path().unwrap();
        let second = catalog.new_path().unwrap();
        assert_ne!(first, second);
        let session = CodingSession::create(&first, &workspace).unwrap();
        session.set_name(Some("First task")).unwrap();
        let model = ModelRef {
            provider: "script".into(),
            model: "script".into(),
        };
        let agent = CodingAgent::new(
            Arc::new(ScriptedModelService::new([Script::Stream(vec![
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None,
                    },
                    usage: Usage::unknown(),
                    termination: ResponseTermination::Completed,
                    returned_model: None,
                }),
            ])])),
            Arc::new(LocalTools::new(&workspace).unwrap()),
        );
        agent
            .submit(
                &session,
                model,
                "first prompt".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        drop(session);
        let session = CodingSession::create(&second, &workspace).unwrap();
        session.set_name(Some("Second task")).unwrap();
        drop(session);
        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        let id = &listed[0].id;
        assert_eq!(catalog.by_id(id).unwrap(), listed[0].path);
        assert_eq!(catalog.latest().unwrap(), listed[0].path);
        assert!(
            listed
                .iter()
                .any(|summary| summary.name.as_deref() == Some("First task"))
        );
        assert_eq!(listed[0].name.as_deref(), Some("First task"));
        fs::write(first.with_file_name("damaged.sqlite"), b"partial database").unwrap();
        assert_eq!(catalog.list().unwrap().len(), 1);
        assert_eq!(catalog.latest().unwrap(), listed[0].path);
        fs::remove_dir_all(root).unwrap();
    }
}
