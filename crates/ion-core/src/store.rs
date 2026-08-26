//! Durable session store: SQLite on one dedicated blocking thread
//! (DESIGN.md §11).
//!
//! One database per Ion data root; WAL, foreign keys, busy timeout, and
//! `synchronous=FULL` for accepted-intent durability. The store thread
//! owns the write connection; callers get typed replies. Every
//! transition commits atomically: entries, the total operation-state
//! checkpoint, opened/settled effects, and inbox status all agree or
//! the transaction fails (§10.2, §26.2).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use rusqlite::Connection;
use rusqlite::types::Type;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::ids::{EffectId, InboxId, OperationId, SessionId};
use crate::session::{InboxKind, OperationState, SessionEntry};
use crate::tool::RecoveryClass;

const STORE_CAPACITY: usize = 64;

mod schema;
mod sql;
use schema::{SchemaPlan, classify, create_fresh};
use sql::handle_command;

/// One durable session row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub cwd: String,
    pub title: String,
    /// Host-selected launch default, persisted before the first effect.
    /// Later changes are append-only [`SessionEntry::ModelChanged`] rows.
    pub initial_model_ref: String,
    /// Present for bounded child sessions (§20.3): lineage is durable
    /// before the child runs.
    pub parent_session_id: Option<SessionId>,
}

/// One entry to append; `seq` is storage-assigned by the runtime's
/// per-session counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRecord {
    pub seq: u64,
    pub entry: SessionEntry,
}

/// A total operation-state checkpoint (DESIGN.md §10.1). Carries
/// everything needed to rebuild the live machine on reopen: the frozen
/// capability snapshot, the operation prompt, and the pending effect
/// intent, if any.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointPayload {
    pub state: OperationState,
    pub cancel_requested: bool,
    pub prompt: String,
    pub capability_snapshot_id: String,
    pub open_effect: Option<EffectRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRecord {
    pub state_seq: u64,
    pub payload: CheckpointPayload,
    pub capability_snapshot: crate::context::CapabilitySnapshot,
}

/// An effect intent opened before repeat-sensitive execution
/// (DESIGN.md §12.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectRecord {
    pub id: EffectId,
    pub kind: String,
    pub recovery_class: RecoveryClass,
    pub effective_input: serde_json::Value,
    /// 1 for a fresh effect; recovery replays increment it (§15.4).
    pub attempt: u64,
}

/// Bounded auxiliary output for an in-flight assistant effect. A frame never
/// proves provider completion or becomes an assistant semantic entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantFrame {
    pub effect_id: EffectId,
    pub session_id: SessionId,
    pub operation_id: OperationId,
    pub step: u64,
    pub frame_seq: u64,
    pub text: String,
    pub thinking: String,
}

/// Bounded auxiliary output for an in-flight tool effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProgressCheckpoint {
    pub effect_id: EffectId,
    pub session_id: SessionId,
    pub operation_id: OperationId,
    pub call_id: u64,
    pub output: String,
}

/// One settled effect: the typed outcome is stored with the effect row
/// so recovery can classify the crash window (DESIGN.md §12.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledEffect {
    pub id: EffectId,
    pub settlement: serde_json::Value,
}

/// One durable inbox item (DESIGN.md §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxRecord {
    pub id: InboxId,
    pub kind: InboxKind,
    pub text: String,
    pub status: InboxStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InboxStatus {
    Pending,
    Applied,
}

/// Everything one transition durably changes, committed as one
/// transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRequest {
    pub session_id: SessionId,
    pub operation_id: OperationId,
    pub checkpoint: CheckpointRecord,
    pub entries: Vec<EntryRecord>,
    pub open_effects: Vec<EffectRecord>,
    pub settled_effects: Vec<SettledEffect>,
    /// Unresolved NeverReplay effects marked indeterminate after process
    /// loss (DESIGN.md §12.2).
    pub indeterminate_effects: Vec<EffectId>,
    /// Inbox items inserted by this transition (status as recorded).
    pub inbox: Vec<InboxRecord>,
    /// Previously pending inbox items this transition applied.
    pub inbox_applied: Vec<InboxId>,
    /// Token usage rows persisted atomically with this transition
    /// (DESIGN.md §27.2).
    pub usage: Vec<UsageRecord>,
    /// Context manifests published in the same transaction as their model
    /// effect intent. The effect stores only their content-addressed IDs.
    pub context_manifests: Vec<crate::context::ContextManifest>,
    /// Auxiliary assistant frames removed with settled model effects.
    pub assistant_frames_delete: Vec<EffectId>,
    /// Auxiliary tool progress removed with settled tool effects.
    pub tool_progress_delete: Vec<EffectId>,
}

/// One persisted token-usage row (DESIGN.md §27.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub step: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// One usage row as read back for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    pub operation_id: OperationId,
    pub step: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// A session loaded from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSession {
    pub session: SessionRecord,
    pub entries: Vec<(u64, SessionEntry)>,
    pub operations: Vec<LoadedOperation>,
    pub pending_inbox: Vec<InboxRecord>,
    pub assistant_frames: Vec<AssistantFrame>,
    pub tool_progress: Vec<ToolProgressCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedOperation {
    pub id: OperationId,
    pub accepted_seq: u64,
    pub latest: (u64, CheckpointPayload),
    pub capability_snapshot: crate::context::CapabilitySnapshot,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(String),
    #[error("injected store failure")]
    Injected,
    #[error("session {0} not found")]
    NotFound(SessionId),
    #[error("store is closed")]
    Closed,
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err.to_string())
    }
}

enum StoreCommand {
    CreateSession {
        record: SessionRecord,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    BeginOperation {
        session_id: SessionId,
        operation_id: OperationId,
        root_inbox: InboxRecord,
        checkpoint: CheckpointRecord,
        entry: EntryRecord,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    Commit {
        request: CommitRequest,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    AppendEntry {
        session_id: SessionId,
        entry: EntryRecord,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    Load {
        session_id: SessionId,
        reply: oneshot::Sender<Result<LoadedSession, StoreError>>,
    },
    LatestSession {
        reply: oneshot::Sender<Result<Option<SessionId>, StoreError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    Usage {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Vec<UsageRow>, StoreError>>,
    },
    UpsertAssistantFrame {
        frame: AssistantFrame,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    UpsertToolProgress {
        progress: ToolProgressCheckpoint,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
}

/// Handle to the store thread. Cheap to clone.
#[derive(Clone)]
pub struct SessionStore {
    tx: mpsc::Sender<StoreCommand>,
    /// Durable sibling directory for large tool-output artifacts. In-memory
    /// stores intentionally have no artifact root; their tool results still
    /// obey the bounded model-result limit.
    artifact_root: Option<Arc<Path>>,
    /// One-time notice for frontends (e.g. archived old-schema store).
    startup_notice: Option<String>,
    /// Test hook: fail the next mutating command (DESIGN.md §30.5).
    fail_next_write: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStore").finish_non_exhaustive()
    }
}

impl SessionStore {
    /// Open (creating or archiving-forward) the database at `path` and
    /// start the store thread. An older dev build's database is
    /// archived untouched beside the original; the human-readable
    /// notice is available via [`Self::startup_notice`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut connection = Connection::open(&path)?;
        let mut notice = None;
        match classify(&connection)? {
            SchemaPlan::Current => {}
            SchemaPlan::Fresh => create_fresh(&mut connection)?,
            SchemaPlan::ArchiveOlder(old_version) => {
                drop(connection);
                let backup = archive_database_files(&path, old_version)?;
                notice = Some(format!(
                    "session store was from an older Ion build (schema v{old_version}); \
                     archived untouched to {} and started fresh",
                    backup.display()
                ));
                connection = Connection::open(&path)?;
                create_fresh(&mut connection)?;
            }
        }
        let artifact_root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("artifacts");
        Self::start_with(connection, Some(Arc::from(artifact_root)), notice)
    }

    /// One-time notice about startup-side state changes (e.g. an
    /// archived old-schema database), for frontends to surface.
    pub fn startup_notice(&self) -> Option<&str> {
        self.startup_notice.as_deref()
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let mut connection = Connection::open_in_memory()?;
        create_fresh(&mut connection)?;
        Self::start_with(connection, None, None)
    }

    fn start_with(
        mut connection: Connection,
        artifact_root: Option<Arc<Path>>,
        startup_notice: Option<String>,
    ) -> Result<Self, StoreError> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let (tx, mut rx) = mpsc::channel(STORE_CAPACITY);
        let fail_next_write = Arc::new(AtomicBool::new(false));
        let fail_flag = Arc::clone(&fail_next_write);
        let join = std::thread::Builder::new()
            .name("ion-store".to_owned())
            .spawn(move || {
                while let Some(command) = rx.blocking_recv() {
                    let shutdown = matches!(&command, StoreCommand::Shutdown { .. });
                    handle_command(&mut connection, command, &fail_flag);
                    if shutdown {
                        break;
                    }
                }
            })
            .map_err(|err| StoreError::Sqlite(err.to_string()))?;
        Ok(Self {
            tx,
            artifact_root,
            startup_notice,
            fail_next_write,
            join: Arc::new(Mutex::new(Some(join))),
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The root owned by this store for atomically published tool artifacts.
    #[must_use]
    pub(crate) fn artifact_root(&self) -> Option<PathBuf> {
        self.artifact_root.as_deref().map(Path::to_path_buf)
    }

    pub async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::CreateSession { record, reply })
            .await
    }

    /// Durably accept an operation: the operation row, its root inbox
    /// item, the initial total state, and the user entry commit as one
    /// transaction before the caller is acknowledged (DESIGN.md §9.1).
    pub async fn begin_operation(
        &self,
        session_id: SessionId,
        operation_id: OperationId,
        root_inbox: InboxRecord,
        checkpoint: CheckpointRecord,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::BeginOperation {
            session_id,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            reply,
        })
        .await
    }

    pub async fn commit(&self, request: CommitRequest) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::Commit { request, reply })
            .await
    }

    /// Append semantic session configuration while idle or while an
    /// immutable effect is in flight. The session runtime assigns seq.
    pub async fn append_entry(
        &self,
        session_id: SessionId,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::AppendEntry {
            session_id,
            entry,
            reply,
        })
        .await
    }

    pub async fn load(&self, session_id: SessionId) -> Result<LoadedSession, StoreError> {
        self.request(|reply| StoreCommand::Load { session_id, reply })
            .await
    }

    /// Token usage rows recorded for one session (DESIGN.md §27.2).
    pub async fn usage(&self, session_id: SessionId) -> Result<Vec<UsageRow>, StoreError> {
        self.request(|reply| StoreCommand::Usage { session_id, reply })
            .await
    }

    /// The most recently updated session, for `--resume`.
    pub async fn latest_session(&self) -> Result<Option<SessionId>, StoreError> {
        self.request(|reply| StoreCommand::LatestSession { reply })
            .await
    }

    /// Persist the newest bounded frame for an in-flight assistant effect.
    pub async fn upsert_assistant_frame(&self, frame: AssistantFrame) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::UpsertAssistantFrame { frame, reply })
            .await
    }

    pub async fn upsert_tool_progress(
        &self,
        progress: ToolProgressCheckpoint,
    ) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::UpsertToolProgress { progress, reply })
            .await
    }

    /// Stop the dedicated writer thread after all previously queued commands
    /// finish, then join it. Hosts call this after every runtime using the
    /// store has joined.
    pub async fn close(&self) -> Result<(), StoreError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(StoreCommand::Shutdown { reply })
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)??;
        let join = self.join.lock().expect("store join poisoned").take();
        if let Some(join) = join {
            tokio::task::spawn_blocking(move || {
                join.join()
                    .map_err(|_| StoreError::Sqlite("store thread panicked".to_owned()))
            })
            .await
            .map_err(|_| StoreError::Closed)??;
        }
        Ok(())
    }

    /// Test hook (DESIGN.md §30.5): the next mutating command fails
    /// visibly and nothing is written.
    pub fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::SeqCst);
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, StoreError>>) -> StoreCommand,
    ) -> Result<T, StoreError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(StoreError::Closed);
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(build(reply))
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }
}

/// Rename the database and its WAL/SHM siblings to timestamped
/// `.v{version}.{unix_ts}.bak` copies of the same names. The bytes are
/// never opened, migrated, or reinterpreted; the archive is a plain
/// rename so it is atomic per file and reversible by hand. Returns the
/// main backup path.
fn archive_database_files(path: &Path, old_version: i64) -> Result<PathBuf, StoreError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sessions.db");
    let backup_name = format!("{file_name}.v{old_version}.{stamp}.bak");
    let backup = path.with_file_name(&backup_name);
    for (source, target) in [
        (path.to_path_buf(), backup.clone()),
        (
            path.with_file_name(format!("{file_name}-wal")),
            path.with_file_name(format!("{backup_name}-wal")),
        ),
        (
            path.with_file_name(format!("{file_name}-shm")),
            path.with_file_name(format!("{backup_name}-shm")),
        ),
    ] {
        if source.exists() {
            std::fs::rename(&source, &target).map_err(|err| {
                StoreError::Sqlite(format!(
                    "could not archive {} to {}: {err}; move the old session \
                     database aside manually and retry",
                    source.display(),
                    target.display()
                ))
            })?;
        }
    }
    Ok(backup)
}

/// Default database path under the Ion data root
/// (`$XDG_DATA_HOME` or `$HOME/.local/share`, then `ion/sessions.db`).
#[must_use]
pub fn default_db_path() -> PathBuf {
    let root = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|_| PathBuf::from("."));
    root.join("ion").join("sessions.db")
}
