//! Process-local quarantine, not a journal, execution queue or replay authority.
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use ion_app::{SessionControl, UnresolvedInput, UserInput, attachments::AttachmentSource};
use ion_core::{InputReceipt, InputStatus};
use serde::Serialize;

use super::{EditorDraft, Frontend, session_io::ReadOutput};

#[cfg(test)]
mod tests;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Serialize)]
pub(super) struct Origin {
    pub(super) session: uuid::Uuid,
    pub(super) path: PathBuf,
}

impl Origin {
    pub(super) fn capture(session: &ion_core::Session) -> Self {
        Self {
            session: session.id(),
            path: session.path().to_owned(),
        }
    }
}

enum Source {
    // Keep the entire wrapper: its private byte lease belongs to this custody.
    Input(UnresolvedInput<EditorDraft>),
    Discarded(InputReceipt),
    Command(EditorDraft),
}

struct Record {
    id: u64,
    origin: Origin,
    cause: String,
    source: Source,
}

#[derive(Default)]
pub(super) struct Recovery {
    records: Vec<Record>,
}

#[derive(Serialize)]
struct Snapshot {
    format: &'static str,
    id: u64,
    origin: Origin,
    cause: String,
    status: serde_json::Value,
    source: Option<SnapshotSource>,
}

#[derive(Serialize)]
struct SnapshotSource {
    editor: EditorDraft,
    input: Option<UserInput>,
    input_id: Option<ion_app::InputId>,
    operation: Option<ion_app::OperationId>,
    delivery: Option<ion_app::Delivery>,
}

impl Record {
    fn status(&self) -> serde_json::Value {
        match &self.source {
            Source::Input(input) => serde_json::json!(input.receipt.status()),
            Source::Discarded(receipt) => serde_json::json!(receipt.status()),
            Source::Command(_) => serde_json::json!({"state": "unknown"}),
        }
    }

    fn snapshot(&self) -> Snapshot {
        // One bounded owned copy lets the I/O worker serialize without sharing
        // editor custody or transferring the original receipt/reservation.
        let copy_editor = |editor: &EditorDraft| EditorDraft {
            literal: editor.literal.clone(),
            cursor: editor.cursor,
        };
        let source = match &self.source {
            Source::Input(input) => Some(SnapshotSource {
                editor: copy_editor(&input.metadata),
                input: Some(input.input.clone()),
                input_id: Some(input.id),
                operation: Some(input.operation),
                delivery: Some(input.delivery),
            }),
            Source::Command(editor) => Some(SnapshotSource {
                editor: copy_editor(editor),
                input: None,
                input_id: None,
                operation: None,
                delivery: None,
            }),
            Source::Discarded(_) => None,
        };
        Snapshot {
            format: "ion-recovery-inspection-v1",
            id: self.id,
            origin: self.origin.clone(),
            cause: self.cause.clone(),
            status: self.status(),
            source,
        }
    }
}

impl Recovery {
    fn insert(&mut self, origin: Origin, cause: String, source: Source) -> u64 {
        let id = NEXT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("process-local recovery ID space exhausted");
        self.records.push(Record {
            id,
            origin,
            cause,
            source,
        });
        id
    }

    pub(super) fn input(
        &mut self,
        input: UnresolvedInput<EditorDraft>,
        origin: Origin,
    ) -> Result<u64> {
        anyhow::ensure!(
            input.session == origin.session,
            "unresolved input origin differs from its Session"
        );
        anyhow::ensure!(
            matches!(input.input, UserInput::Prompt { .. }),
            "native recovery received SDK Message input"
        );
        Ok(self.insert(
            origin,
            "Input commit owner reported an unresolved disposition".into(),
            Source::Input(input),
        ))
    }

    pub(super) fn command(&mut self, source: EditorDraft, origin: Origin, cause: String) -> u64 {
        self.insert(origin, cause, Source::Command(source))
    }

    pub(super) fn blocks(&self, session: uuid::Uuid) -> bool {
        self.records.iter().any(|record| {
            record.origin.session == session
                && match &record.source {
                    Source::Input(input) => {
                        !matches!(input.receipt.status(), InputStatus::NotAccepted)
                    }
                    Source::Discarded(receipt) => {
                        !matches!(receipt.status(), InputStatus::NotAccepted)
                    }
                    Source::Command(_) => false,
                }
        })
    }

    pub(super) fn list(&self) -> String {
        if self.records.is_empty() {
            return "No local recovery records".into();
        }
        let mut rows = vec!["Local recovery · unsaved sources are lost on quit".into()];
        rows.extend(self.records.iter().map(|record| {
            format!(
                "{} · Session {} · {} · {}",
                record.id,
                record.origin.session,
                record.status(),
                match &record.source {
                    Source::Input(input) if input.receipt.status() == InputStatus::NotAccepted =>
                        "not accepted; select original Session to return draft",
                    Source::Input(_) => "input quarantined",
                    Source::Discarded(_) => "source discarded; fence retained",
                    Source::Command(_) => "command effects unknown",
                }
            )
        }));
        rows.join("\n")
    }

    pub(super) fn inspect(&self, id: u64) -> Result<String> {
        let record = self.record(id)?;
        let mut text = format!(
            "Recovery {}\nSession: {}\nPath: {}\nStatus: {}\nCause: {}\n",
            id,
            record.origin.session,
            record.origin.path.display(),
            record.status(),
            record.cause
        );
        let editor = match &record.source {
            Source::Input(input) => {
                if let UserInput::Prompt {
                    text: prompt,
                    attachments,
                } = &input.input
                {
                    text.push_str(&format!(
                        "Prompt (JSON literal): {}\nAttachments: {}\n",
                        serde_json::to_string(prompt)?,
                        attachments.len()
                    ));
                    for attachment in attachments {
                        text.push_str(&format!("  {}\n", attachment_metadata(attachment)));
                    }
                }
                Some(&input.metadata)
            }
            Source::Command(editor) => Some(editor),
            Source::Discarded(_) => None,
        };
        if let Some(editor) = editor {
            text.push_str(&format!(
                "Editor literal (JSON): {}\nCaret (UTF-8 byte offset): {}",
                serde_json::to_string(&editor.literal)?,
                editor.cursor
            ));
        } else {
            text.push_str("Local source discarded; Session fence remains until receipt authority resolves it.");
        }
        Ok(text)
    }

    fn record(&self, id: u64) -> Result<&Record> {
        self.records
            .iter()
            .find(|record| record.id == id)
            .context("unknown local recovery ID")
    }

    fn discard(&mut self, id: u64) -> Result<()> {
        let at = self
            .records
            .iter()
            .position(|record| record.id == id)
            .context("unknown local recovery ID")?;
        let record = &mut self.records[at];
        match &record.source {
            Source::Input(input) => record.source = Source::Discarded(input.receipt.clone()),
            Source::Discarded(_) => {}
            Source::Command(_) => {
                self.records.remove(at);
            }
        }
        Ok(())
    }
}

fn attachment_metadata(source: &AttachmentSource) -> String {
    match source {
        AttachmentSource::File(path) => format!("file {}", path.display()),
        AttachmentSource::ImageFile(path) => format!("image file {}", path.display()),
        AttachmentSource::ImageBytes { bytes, mime_type } => {
            format!("image bytes: {} bytes, {mime_type:?}", bytes.len())
        }
        AttachmentSource::Image(image) => format!(
            "loaded image: {} encoded bytes, {:?}",
            image.content.data().len(),
            image.content.mime_type()
        ),
        AttachmentSource::Text(file) => {
            format!("text {:?}: {} UTF-8 bytes", file.name, file.text.len())
        }
    }
}

impl Frontend {
    pub(super) fn reconcile_recovery(&mut self, current: uuid::Uuid) -> Result<()> {
        let mut at = 0;
        while at < self.recovery.records.len() {
            let status = match &self.recovery.records[at].source {
                Source::Input(input) => input.receipt.status(),
                Source::Discarded(receipt) => receipt.status(),
                Source::Command(_) => {
                    at += 1;
                    continue;
                }
            };
            match status {
                InputStatus::Committed(commit) => {
                    anyhow::ensure!(
                        commit.session == self.recovery.records[at].origin.session,
                        "recovery receipt committed to a different Session"
                    );
                    let record = self.recovery.records.remove(at);
                    self.note(format!(
                        "Recovery {} committed in Session {}; original released without replay",
                        record.id, record.origin.session
                    ));
                    self.session_io.request_refresh();
                }
                InputStatus::NotAccepted => {
                    // Editor custody is Session-scoped. A late rejection may be
                    // authoritative without belonging in the currently bound editor.
                    if self.recovery.records[at].origin.session != current
                        && matches!(self.recovery.records[at].source, Source::Input(_))
                    {
                        at += 1;
                        continue;
                    }
                    let record = self.recovery.records.remove(at);
                    if let Source::Input(input) = record.source {
                        let UserInput::Prompt { attachments, .. } = input.input else {
                            unreachable!("validated native prompt")
                        };
                        self.return_draft(input.metadata, attachments);
                    }
                    self.note(format!("Recovery {} was not accepted; any retained draft returned without submitting", record.id));
                }
                InputStatus::Pending | InputStatus::Committing | InputStatus::Unresolved => at += 1,
            }
        }
        Ok(())
    }
}

pub(super) fn command(
    control: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    args: &str,
) -> Result<()> {
    ui.reconcile_recovery(control.session().id())?;
    let (verb, tail) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    match verb {
        "" => ui.print(ui.recovery.list()),
        "discard" => {
            let id = tail.trim().parse().context("use /recovery discard ID")?;
            ui.recovery.discard(id)?;
            ui.status = format!(
                "Recovery {id}: local source discarded; not proof of rollback or permission to retry"
            );
        }
        "save" => {
            let (id, path) = tail
                .trim_start()
                .split_once(char::is_whitespace)
                .context("use /recovery save ID PATH")?;
            let id = id.parse().context("invalid recovery ID")?;
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let target = super::command::attachment_path(path.trim(), &ui.cwd, home.as_deref())?;
            let snapshot = ui.recovery.record(id)?.snapshot();
            let io = control.session().io()?;
            ui.session_io
                .start_file_write(control.session(), async move {
                    io.run(move |_| {
                        Ok(save_new(&snapshot, &target)
                            .map(|()| ReadOutput::RecoverySaved { id, path: target }))
                    })
                    .await?
                })?;
        }
        id if tail.trim().is_empty() => ui.print(
            ui.recovery.inspect(
                id.parse()
                    .context("use /recovery [ID|save ID PATH|discard ID]")?,
            )?,
        ),
        _ => anyhow::bail!("use /recovery [ID|save ID PATH|discard ID]"),
    }
    Ok(())
}

fn save_new(snapshot: &Snapshot, path: &Path) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(snapshot)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}
