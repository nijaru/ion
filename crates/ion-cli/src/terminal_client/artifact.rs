use std::{
    fs::{self, OpenOptions},
    num::NonZeroUsize,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ion_app::SessionControl;
use ion_core::{
    ArtifactRef, Session, SessionError, TranscriptActivity, TranscriptItem, UserShellOutcome,
};

use super::{
    Action, EditorDraft, Frontend, command::file_path, editing, session_io::ReadOutput, tools,
};
use crate::{
    keybindings::{self, KeyAction, KeyBindings},
    transcript_detail::CaptureChoice,
};

pub(super) struct SavePrompt {
    choices: Vec<CaptureChoice>,
    selected: usize,
    path: String,
    cursor: usize,
}

impl SavePrompt {
    pub fn new(choices: &[CaptureChoice], selected: usize) -> Self {
        Self {
            choices: choices.to_vec(),
            selected,
            path: String::new(),
            cursor: 0,
        }
    }

    pub fn insert(&mut self, value: &str) {
        // A destination is one line; don't silently turn multiline paste into a path.
        if !value.chars().any(char::is_control) && self.path.len() + value.len() <= 16 * 1024 {
            self.path.insert_str(self.cursor, value);
            self.cursor += value.len();
        }
    }

    pub fn key(&mut self, key: ion_terminal::KeyEvent, keys: &KeyBindings) -> Action {
        match keys.action(keybindings::TEXT, key) {
            Some(KeyAction::Submit) if !self.path.trim().is_empty() => Action::SaveCapture {
                artifact: self.choices[self.selected].artifact.clone(),
                path: self.path.clone(),
            },
            Some(KeyAction::SelectUp) => {
                self.selected = self.selected.saturating_sub(1);
                Action::None
            }
            Some(KeyAction::SelectDown) => {
                self.selected = (self.selected + 1).min(self.choices.len() - 1);
                Action::None
            }
            Some(action) => {
                match editing::apply(&self.path, self.cursor, action) {
                    Some(editing::Edit::Move(cursor)) => self.cursor = cursor,
                    Some(editing::Edit::Delete { range, .. }) => {
                        self.cursor = range.start;
                        self.path.replace_range(range, "");
                    }
                    None => {}
                }
                Action::None
            }
            None => {
                if let Some(ch) = keybindings::printable(key) {
                    self.insert(ch.encode_utf8(&mut [0; 4]));
                }
                Action::None
            }
        }
    }

    pub fn title(&self) -> String {
        format!(
            "Save capture {}/{} · {}",
            self.selected + 1,
            self.choices.len(),
            self.choices[self.selected].label
        )
    }

    pub fn input(&self, width: usize) -> crate::terminal_input::InputWindow {
        crate::terminal_input::input_window(&self.path, self.cursor, width, 1)
    }
}

pub(super) fn save_tail(args: &str) -> Option<(&str, &str)> {
    let (number, rest) = args.split_once(char::is_whitespace)?;
    let mut rest = rest.trim_start().splitn(2, char::is_whitespace);
    (rest.next()? == "save").then(|| (number, rest.next().unwrap_or_default().trim_start()))
}

fn tool_number(text: &str) -> Result<NonZeroUsize> {
    text.parse()
        .context("tool numbers are one-based (for example 3 or 3.2)")
}

fn artifact_at<'a>(
    mut tools: impl Iterator<Item = &'a TranscriptActivity>,
    number: &str,
    index: usize,
) -> Result<&'a ArtifactRef> {
    let mut parts = number.split('.');
    let root = tool_number(parts.next().unwrap_or_default())?;
    let mut activity = tools.nth(root.get() - 1);
    for part in parts {
        let part = tool_number(part)?;
        activity = activity.and_then(|parent| parent.children.get(part.get() - 1));
    }
    let activity = activity.with_context(|| format!("Tool {number} not found; use /tools"))?;
    activity
        .result
        .as_ref()
        .and_then(|result| result.artifacts.get(index))
        .with_context(|| format!("Tool {number} has no recorded artifact {index}"))
}

fn shell_artifact_at<'a>(
    items: impl IntoIterator<Item = &'a TranscriptItem>,
    entry: &str,
    index: usize,
) -> Result<&'a ArtifactRef> {
    let entry = entry
        .parse::<std::num::NonZeroU64>()
        .context("shell entry numbers are one-based (for example shell:3)")?;
    let shell = items
        .into_iter()
        .find_map(|item| match item {
            TranscriptItem::UserShell(shell) if shell.source.entry == entry.get() => Some(shell),
            _ => None,
        })
        .with_context(|| format!("Shell entry {entry} not found; use /tool all"))?;
    match &shell.outcome {
        Some(UserShellOutcome::Observed { output }) => output.artifacts.get(index),
        Some(UserShellOutcome::Unknown) | None => None,
    }
    .with_context(|| format!("Shell entry {entry} has no recorded artifact {index}"))
}

pub(super) fn start_save(
    runtime: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    number: &str,
    tail: &str,
) -> Result<()> {
    let (index, path) = tail
        .split_once(char::is_whitespace)
        .context("use /tool N save INDEX PATH or /tool shell:ENTRY save INDEX PATH")?;
    let index = index.parse().context("artifact index must be zero-based")?;
    let items = ui.conversation.items();
    let artifact = if let Some(entry) = number.strip_prefix("shell:") {
        shell_artifact_at(items, entry, index)?
    } else {
        artifact_at(tools(items), number, index)?
    }
    .clone();
    start_export(runtime, ui, artifact, path.trim_start())
}

pub(super) fn start_export(
    runtime: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    artifact: ArtifactRef,
    path: &str,
) -> Result<()> {
    anyhow::ensure!(
        !ui.session_io.blocks_input(),
        "Wait for Session history to refresh before saving an artifact"
    );
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = file_path(path, &ui.cwd, home.as_deref())?;
    let io = runtime.session().io()?;
    ui.session_io
        .start_file_write(runtime.session(), async move {
            Ok(io
                .run(move |session| {
                    let result = save_new(session, &artifact, &path).map(|()| artifact.complete);
                    Ok(ReadOutput::ArtifactExport { path, result })
                })
                .await?)
        })
}

fn save_new(session: &Session, artifact: &ArtifactRef, path: &Path) -> Result<(), SessionError> {
    let parent = path.parent().ok_or(SessionError::InvalidPath)?;
    let staged = parent.join(format!(".ion-artifact-{}.tmp", uuid::Uuid::now_v7()));
    let file = OpenOptions::new()
        .mode(0o600)
        .write(true)
        .create_new(true)
        .open(&staged)?;
    // Publish only after integrity verification. A hard link installs the new
    // name atomically without replacing an existing file or symlink.
    let result = (|| {
        session.stream_artifact(artifact, &file)?;
        file.sync_all()?;
        fs::hard_link(&staged, path)?;
        Ok(())
    })();
    drop(file);
    let _ = fs::remove_file(&staged);
    result
}

#[test]
fn capture_save_publishes_only_verified_bytes_without_clobbering() {
    let root = std::env::temp_dir().join(format!("ion-artifact-test-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let session = std::sync::Arc::new(Session::create(root.join("session.sqlite"), None).unwrap());
    let bytes = b"\0\xff\x1b[0m\r\n";
    let receipt = session.artifact_sink().capture("stdout", bytes).unwrap();
    let dest = root.join("capture.bin");
    fs::write(&dest, b"existing").unwrap();
    assert!(save_new(&session, &receipt, &dest).is_err());
    assert_eq!(fs::read(&dest).unwrap(), b"existing");
    let dest = root.join("new.bin");
    let mut wrong = receipt.clone();
    wrong.bytes += 1;
    assert!(save_new(&session, &wrong, &dest).is_err());
    assert!(!dest.exists());
    save_new(&session, &receipt, &dest).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), bytes);
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ion-artifact-")
    }));
    drop(session);
    fs::remove_dir_all(root).unwrap();
}
