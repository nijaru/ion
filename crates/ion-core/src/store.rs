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

const SCHEMA_VERSION: i64 = 11;

/// Schema gating (DESIGN.md §11.1). Ion is v0 with no compatibility
/// guarantees: a fresh database gets the current schema, and a database
/// written by any other version — older dev build or newer Ion — is
/// refused, never migrated and never silently reinterpreted (§26.3).
fn apply_migrations(connection: &mut Connection) -> Result<(), StoreError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version == 0
        && connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count == 0)?
    {
        connection.execute_batch(SCHEMA)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        return Ok(());
    }
    Err(StoreError::Sqlite(format!(
        "database schema version {version} does not match this Ion ({SCHEMA_VERSION}); \
         Ion v0 keeps no compatibility across builds — move the database aside"
    )))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    operation_id TEXT NOT NULL,
    step INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    recorded_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    cwd TEXT NOT NULL,
    title TEXT NOT NULL,
    parent_session_id TEXT,
    initial_model_ref TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entries (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    seq INTEGER NOT NULL,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);

CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    kind TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    accepted_seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS operation_state (
    operation_id TEXT NOT NULL REFERENCES operations(id),
    state_seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (operation_id)
);

CREATE TABLE IF NOT EXISTS inbox_items (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    status TEXT NOT NULL,
    accepted_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS effects (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL REFERENCES operations(id),
    kind TEXT NOT NULL,
    recovery_class TEXT NOT NULL,
    status TEXT NOT NULL,
    effective_input TEXT NOT NULL,
    settlement TEXT,
    created_at INTEGER NOT NULL,
    settled_at INTEGER,
    attempt INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS capability_snapshots (
    id TEXT PRIMARY KEY,
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS context_manifests (
    id TEXT PRIMARY KEY,
    capability_snapshot_id TEXT NOT NULL REFERENCES capability_snapshots(id),
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS model_steps (
    effect_id TEXT PRIMARY KEY REFERENCES effects(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    step INTEGER NOT NULL,
    model_ref TEXT NOT NULL,
    context_window INTEGER,
    capability_snapshot_id TEXT NOT NULL REFERENCES capability_snapshots(id),
    context_manifest_id TEXT NOT NULL REFERENCES context_manifests(id),
    capabilities TEXT NOT NULL,
    context_fingerprint TEXT NOT NULL,
    cache_expectation TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS assistant_frames (
    effect_id TEXT PRIMARY KEY REFERENCES effects(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    step INTEGER NOT NULL,
    frame_seq INTEGER NOT NULL,
    text TEXT NOT NULL,
    thinking TEXT NOT NULL,
    updated_at INTEGER NOT NULL
)
";

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
}

/// Handle to the store thread. Cheap to clone.
#[derive(Clone)]
pub struct SessionStore {
    tx: mpsc::Sender<StoreCommand>,
    /// Durable sibling directory for large tool-output artifacts. In-memory
    /// stores intentionally have no artifact root; their tool results still
    /// obey the bounded model-result limit.
    artifact_root: Option<Arc<Path>>,
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
    /// Open (creating or migrating) the database at `path` and start the
    /// store thread.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let connection = Connection::open(&path)?;
        let artifact_root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("artifacts");
        Self::start(connection, Some(Arc::from(artifact_root)))
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        Self::start(connection, None)
    }

    fn start(
        mut connection: Connection,
        artifact_root: Option<Arc<Path>>,
    ) -> Result<Self, StoreError> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        apply_migrations(&mut connection)?;
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

fn handle_command(
    connection: &mut Connection,
    command: StoreCommand,
    fail_next_write: &AtomicBool,
) {
    match command {
        StoreCommand::CreateSession { record, reply } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| create_session(connection, &record).map_err(StoreError::from)),
            );
        }
        StoreCommand::BeginOperation {
            session_id,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                begin_operation(
                    connection,
                    session_id,
                    operation_id,
                    &root_inbox,
                    &checkpoint,
                    &entry,
                )
                .map_err(StoreError::from)
            }));
        }
        StoreCommand::Commit { request, reply } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| commit(connection, &request).map_err(StoreError::from)),
            );
        }
        StoreCommand::AppendEntry {
            session_id,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                append_entry(connection, session_id, &entry).map_err(StoreError::from)
            }));
        }
        StoreCommand::Load { session_id, reply } => {
            let _ = reply.send(load(connection, session_id));
        }
        StoreCommand::Usage { session_id, reply } => {
            let _ = reply.send(usage_rows(connection, session_id));
        }
        StoreCommand::LatestSession { reply } => {
            let _ = reply.send(latest_session(connection));
        }
        StoreCommand::Shutdown { reply } => {
            let _ = reply.send(Ok(()));
        }
        StoreCommand::UpsertAssistantFrame { frame, reply } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                upsert_assistant_frame(connection, &frame).map_err(StoreError::from)
            }));
        }
    }
}

fn upsert_assistant_frame(
    connection: &mut Connection,
    frame: &AssistantFrame,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO assistant_frames (
            effect_id, session_id, operation_id, step, frame_seq, text, thinking, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(effect_id) DO UPDATE SET
            frame_seq = excluded.frame_seq,
            text = excluded.text,
            thinking = excluded.thinking,
            updated_at = excluded.updated_at
         WHERE excluded.frame_seq > assistant_frames.frame_seq",
        rusqlite::params![
            frame.effect_id.as_uuid().to_string(),
            frame.session_id.as_uuid().to_string(),
            frame.operation_id.as_uuid().to_string(),
            frame.step as i64,
            frame.frame_seq as i64,
            frame.text,
            frame.thinking,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn latest_session(connection: &mut Connection) -> Result<Option<SessionId>, StoreError> {
    let row: Option<String> = connection
        .query_row(
            "SELECT id FROM sessions ORDER BY updated_at DESC, created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    row.map(|id| {
        Uuid::parse_str(&id)
            .map(SessionId::from_uuid)
            .map_err(|_| StoreError::Sqlite("session id is not a uuid".into()))
    })
    .transpose()
}

fn usage_rows(
    connection: &mut Connection,
    session_id: SessionId,
) -> Result<Vec<UsageRow>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT operation_id, step, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens
         FROM usage WHERE session_id = ?1 ORDER BY id",
    )?;
    let rows = statement
        .query_map([session_id.as_uuid().to_string()], |row| {
            Ok(UsageRow {
                operation_id: OperationId::from_uuid(
                    Uuid::parse_str(&row.get::<_, String>(0)?).map_err(|_| {
                        rusqlite::Error::InvalidColumnType(0, "operation_id".into(), Type::Text)
                    })?,
                ),
                step: row.get::<_, i64>(1)? as u64,
                input_tokens: row.get::<_, i64>(2)? as u64,
                output_tokens: row.get::<_, i64>(3)? as u64,
                cache_read_tokens: row.get::<_, i64>(4)? as u64,
                cache_write_tokens: row.get::<_, i64>(5)? as u64,
            })
        })?
        .collect::<Result<Vec<_>, rusqlite::Error>>()?;
    Ok(rows)
}

fn check_injected(flag: &AtomicBool) -> Result<(), StoreError> {
    if flag.swap(false, Ordering::SeqCst) {
        Err(StoreError::Injected)
    } else {
        Ok(())
    }
}

fn create_session(connection: &Connection, record: &SessionRecord) -> Result<(), rusqlite::Error> {
    let now = now_ms();
    connection.execute(
        "INSERT INTO sessions (id, created_at, updated_at, cwd, title, parent_session_id, initial_model_ref)
         VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            record.id.as_uuid().to_string(),
            now,
            record.cwd,
            record.title,
            record.parent_session_id.map(|id| id.as_uuid().to_string()),
            record.initial_model_ref,
        ],
    )?;
    Ok(())
}

fn append_entry(
    connection: &mut Connection,
    session_id: SessionId,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    insert_entry(&tx, session_id, entry)?;
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now_ms()],
    )?;
    tx.commit()
}

fn begin_operation(
    connection: &mut Connection,
    session_id: SessionId,
    operation_id: OperationId,
    root_inbox: &InboxRecord,
    checkpoint: &CheckpointRecord,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let accepted_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(accepted_seq), 0) + 1 FROM operations WHERE session_id = ?1",
        [session_id.as_uuid().to_string()],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
         VALUES (?1, ?2, 'run', ?3, ?4)",
        rusqlite::params![
            operation_id.as_uuid().to_string(),
            session_id.as_uuid().to_string(),
            now_ms(),
            accepted_seq,
        ],
    )?;
    insert_inbox(&tx, session_id, operation_id, root_inbox)?;
    insert_capability_snapshot(&tx, &checkpoint.capability_snapshot)?;
    insert_checkpoint(&tx, operation_id, checkpoint)?;
    insert_entry(&tx, session_id, entry)?;
    tx.commit()?;
    Ok(())
}

fn commit(connection: &mut Connection, request: &CommitRequest) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    insert_capability_snapshot(&tx, &request.checkpoint.capability_snapshot)?;
    for manifest in &request.context_manifests {
        insert_context_manifest(&tx, manifest)?;
    }
    insert_checkpoint(&tx, request.operation_id, &request.checkpoint)?;
    for entry in &request.entries {
        insert_entry(&tx, request.session_id, entry)?;
    }
    for effect in &request.open_effects {
        tx.execute(
            "INSERT INTO effects (id, operation_id, kind, recovery_class, status, effective_input, created_at, attempt)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
            rusqlite::params![
                effect.id.as_uuid().to_string(),
                request.operation_id.as_uuid().to_string(),
                effect.kind,
                serde_json::to_string(&effect.recovery_class)
                    .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?,
                effect.effective_input.to_string(),
                now_ms(),
                effect.attempt as i64,
            ],
        )?;
        if effect.kind == "model_step" {
            let model = effect.effective_input.get("model").ok_or_else(|| {
                rusqlite::Error::InvalidParameterName("model step missing model".to_owned())
            })?;
            let capability_snapshot_id = effect
                .effective_input
                .get("capability_snapshot_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing capability snapshot id".to_owned(),
                    )
                })?;
            if capability_snapshot_id != request.checkpoint.capability_snapshot.id {
                return Err(rusqlite::Error::InvalidParameterName(
                    "context manifest capability snapshot mismatch".to_owned(),
                ));
            }
            let context_manifest_id = effect
                .effective_input
                .get("context_manifest_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing context manifest id".to_owned(),
                    )
                })?;
            let manifest_exists: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM context_manifests
                    WHERE id = ?1 AND capability_snapshot_id = ?2
                )",
                rusqlite::params![context_manifest_id, capability_snapshot_id],
                |row| row.get(0),
            )?;
            if !manifest_exists {
                return Err(rusqlite::Error::InvalidParameterName(
                    "model step context manifest was not published".to_owned(),
                ));
            }
            let model_ref = model
                .get("model_ref")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName("model step missing model_ref".to_owned())
                })?;
            let capabilities: crate::provider::ModelCapabilities =
                serde_json::from_value(model.get("capabilities").cloned().ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing provider capabilities".to_owned(),
                    )
                })?)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
            let prefix_fingerprint = effect
                .effective_input
                .get("prefix_fingerprint")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing context fingerprint".to_owned(),
                    )
                })?;
            let cache_expectation = effect
                .effective_input
                .get("cache_expectation")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing cache expectation".to_owned(),
                    )
                })?;
            if !matches!(
                cache_expectation,
                "unsupported" | "cold_start" | "prefix_reuse_expected" | "prefix_changed"
            ) {
                return Err(rusqlite::Error::InvalidParameterName(
                    "model step has an unknown cache expectation".to_owned(),
                ));
            }
            let manifest_payload: String = tx.query_row(
                "SELECT payload FROM context_manifests WHERE id = ?1",
                [context_manifest_id],
                |row| row.get(0),
            )?;
            let manifest: crate::context::ContextManifest = serde_json::from_str(&manifest_payload)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
            if manifest.stable_prefix_fingerprint(model_ref) != prefix_fingerprint {
                return Err(rusqlite::Error::InvalidParameterName(
                    "model step context fingerprint mismatch".to_owned(),
                ));
            }
            let capabilities_payload = serde_json::to_string(&capabilities)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
            let step = effect
                .effective_input
                .get("step")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName("model step missing step".to_owned())
                })?;
            tx.execute(
                "INSERT INTO model_steps (
                    effect_id, operation_id, step, model_ref, context_window,
                    capability_snapshot_id, context_manifest_id, capabilities,
                    context_fingerprint, cache_expectation, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    effect.id.as_uuid().to_string(),
                    request.operation_id.as_uuid().to_string(),
                    step as i64,
                    model_ref,
                    model
                        .get("context_window")
                        .and_then(serde_json::Value::as_u64)
                        .map(|v| v as i64),
                    capability_snapshot_id,
                    context_manifest_id,
                    capabilities_payload,
                    prefix_fingerprint,
                    cache_expectation,
                    now_ms(),
                ],
            )?;
        }
    }
    for usage in &request.usage {
        tx.execute(
            "INSERT INTO usage (session_id, operation_id, step, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                request.session_id.as_uuid().to_string(),
                request.operation_id.as_uuid().to_string(),
                usage.step as i64,
                usage.input_tokens as i64,
                usage.output_tokens as i64,
                usage.cache_read_tokens as i64,
                usage.cache_write_tokens as i64,
                now_ms(),
            ],
        )?;
    }
    for effect_id in &request.indeterminate_effects {
        let affected = tx.execute(
            "UPDATE effects SET status = 'indeterminate', settled_at = ?2
             WHERE id = ?1 AND operation_id = ?3 AND status = 'pending'",
            rusqlite::params![
                effect_id.as_uuid().to_string(),
                now_ms(),
                request.operation_id.as_uuid().to_string(),
            ],
        )?;
        if affected != 1 {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "indeterminate settlement matched no pending effect {effect_id}"
            )));
        }
    }
    for settled in &request.settled_effects {
        let affected = tx.execute(
            "UPDATE effects SET status = 'settled', settlement = ?2, settled_at = ?3
             WHERE id = ?1 AND operation_id = ?4 AND status = 'pending'",
            rusqlite::params![
                settled.id.as_uuid().to_string(),
                settled.settlement.to_string(),
                now_ms(),
                request.operation_id.as_uuid().to_string(),
            ],
        )?;
        if affected != 1 {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "settlement matched no pending effect {}",
                settled.id
            )));
        }
    }
    for item in &request.inbox {
        insert_inbox(&tx, request.session_id, request.operation_id, item)?;
    }
    for id in &request.inbox_applied {
        tx.execute(
            "UPDATE inbox_items SET status = 'applied' WHERE id = ?1",
            rusqlite::params![id.as_uuid().to_string()],
        )?;
    }
    for effect_id in &request.assistant_frames_delete {
        tx.execute(
            "DELETE FROM assistant_frames WHERE effect_id = ?1",
            rusqlite::params![effect_id.as_uuid().to_string()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn insert_capability_snapshot(
    connection: &rusqlite::Transaction<'_>,
    snapshot: &crate::context::CapabilitySnapshot,
) -> Result<(), rusqlite::Error> {
    if !snapshot.is_consistent() {
        return Err(rusqlite::Error::InvalidParameterName(
            "capability snapshot id does not match its payload".to_owned(),
        ));
    }
    let payload = serde_json::to_string(snapshot)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO capability_snapshots (id, payload) VALUES (?1, ?2)
         ON CONFLICT(id) DO NOTHING",
        rusqlite::params![snapshot.id, payload],
    )?;
    let stored: String = connection.query_row(
        "SELECT payload FROM capability_snapshots WHERE id = ?1",
        [&snapshot.id],
        |row| row.get(0),
    )?;
    if stored != payload {
        return Err(rusqlite::Error::InvalidParameterName(
            "capability snapshot hash collision or mismatched payload".to_owned(),
        ));
    }
    Ok(())
}

fn insert_context_manifest(
    connection: &rusqlite::Transaction<'_>,
    manifest: &crate::context::ContextManifest,
) -> Result<(), rusqlite::Error> {
    if !manifest.is_consistent() {
        return Err(rusqlite::Error::InvalidParameterName(
            "context manifest id does not match its payload".to_owned(),
        ));
    }
    let payload = serde_json::to_string(manifest)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO context_manifests (id, capability_snapshot_id, payload)
         VALUES (?1, ?2, ?3) ON CONFLICT(id) DO NOTHING",
        rusqlite::params![manifest.id, manifest.capability_snapshot_id, payload],
    )?;
    let stored: String = connection.query_row(
        "SELECT payload FROM context_manifests WHERE id = ?1",
        [&manifest.id],
        |row| row.get(0),
    )?;
    if stored != payload {
        return Err(rusqlite::Error::InvalidParameterName(
            "context manifest hash collision or mismatched payload".to_owned(),
        ));
    }
    Ok(())
}

fn insert_inbox(
    connection: &Connection,
    session_id: SessionId,
    operation_id: OperationId,
    item: &InboxRecord,
) -> Result<(), rusqlite::Error> {
    let status = match item.status {
        InboxStatus::Pending => "pending",
        InboxStatus::Applied => "applied",
    };
    connection.execute(
        "INSERT INTO inbox_items (id, session_id, operation_id, kind, text, status, accepted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            item.id.as_uuid().to_string(),
            session_id.as_uuid().to_string(),
            operation_id.as_uuid().to_string(),
            serde_json::to_string(&item.kind)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?,
            item.text,
            status,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn insert_checkpoint(
    connection: &Connection,
    operation_id: OperationId,
    checkpoint: &CheckpointRecord,
) -> Result<(), rusqlite::Error> {
    if checkpoint.payload.capability_snapshot_id != checkpoint.capability_snapshot.id {
        return Err(rusqlite::Error::InvalidParameterName(
            "checkpoint capability snapshot mismatch".to_owned(),
        ));
    }
    let payload = serde_json::to_string(&checkpoint.payload)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO operation_state (operation_id, state_seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(operation_id) DO UPDATE SET
             state_seq = excluded.state_seq,
             kind = excluded.kind,
             payload = excluded.payload,
             created_at = excluded.created_at",
        rusqlite::params![
            operation_id.as_uuid().to_string(),
            checkpoint.state_seq as i64,
            checkpoint.payload.state.kind(),
            payload,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn insert_entry(
    connection: &Connection,
    session_id: SessionId,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let payload = serde_json::to_string(&entry.entry)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO entries (session_id, seq, id, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            entry.seq as i64,
            Uuid::now_v7().to_string(),
            entry_kind(&entry.entry),
            payload,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn load(connection: &Connection, session_id: SessionId) -> Result<LoadedSession, StoreError> {
    fn decode<T: serde::de::DeserializeOwned>(
        what: &'static str,
        raw: String,
    ) -> Result<T, StoreError> {
        serde_json::from_str(&raw)
            .map_err(|err| StoreError::Sqlite(format!("corrupt {what}: {err}")))
    }

    let id = session_id.as_uuid().to_string();
    let session = connection
        .query_row(
            "SELECT cwd, title, parent_session_id, initial_model_ref FROM sessions WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok(SessionRecord {
                    id: session_id,
                    cwd: row.get(0)?,
                    title: row.get(1)?,
                    parent_session_id: row
                        .get::<_, Option<String>>(2)?
                        .and_then(|text| SessionId::parse(&text)),
                    initial_model_ref: row.get(3)?,
                })
            },
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound(session_id),
            other => StoreError::from(other),
        })?;

    let mut statement = connection
        .prepare("SELECT seq, payload FROM entries WHERE session_id = ?1 ORDER BY seq")?;
    let mut entries = Vec::new();
    let mut rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = rows.next()? {
        let seq: i64 = row.get(0)?;
        let seq = u64::try_from(seq)
            .map_err(|_| StoreError::Sqlite(format!("corrupt entry seq {seq}")))?;
        let payload: String = row.get(1)?;
        entries.push((seq, decode("entry", payload)?));
    }

    let mut statement = connection.prepare(
        "SELECT o.id, o.accepted_seq, s.state_seq, s.payload FROM operations o
         JOIN operation_state s ON s.operation_id = o.id
         WHERE o.session_id = ?1
         ORDER BY o.accepted_seq",
    )?;
    let mut operations = Vec::new();
    let mut op_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = op_rows.next()? {
        let op_id: String = row.get(0)?;
        let accepted_seq: i64 = row.get(1)?;
        let state_seq: i64 = row.get(2)?;
        let payload: String = row.get(3)?;
        let accepted_seq = u64::try_from(accepted_seq).map_err(|_| {
            StoreError::Sqlite(format!("corrupt operation accepted seq {accepted_seq}"))
        })?;
        let checkpoint: CheckpointPayload = decode("checkpoint", payload)?;
        let snapshot_payload: String = connection.query_row(
            "SELECT payload FROM capability_snapshots WHERE id = ?1",
            [&checkpoint.capability_snapshot_id],
            |row| row.get(0),
        )?;
        let capability_snapshot: crate::context::CapabilitySnapshot =
            decode("capability snapshot", snapshot_payload)?;
        if capability_snapshot.id != checkpoint.capability_snapshot_id {
            return Err(StoreError::Sqlite(
                "checkpoint capability snapshot id mismatch".to_owned(),
            ));
        }
        let uuid = Uuid::parse_str(&op_id)
            .map_err(|err| StoreError::Sqlite(format!("corrupt operation id: {err}")))?;
        operations.push(LoadedOperation {
            id: OperationId::from_uuid(uuid),
            accepted_seq,
            latest: (state_seq as u64, checkpoint),
            capability_snapshot,
        });
    }

    let mut statement = connection.prepare(
        "SELECT id, kind, text FROM inbox_items
         WHERE session_id = ?1 AND status = 'pending' ORDER BY accepted_at",
    )?;
    let mut pending_inbox = Vec::new();
    let mut inbox_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = inbox_rows.next()? {
        let item_id: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let text: String = row.get(2)?;
        let uuid = Uuid::parse_str(&item_id)
            .map_err(|err| StoreError::Sqlite(format!("corrupt inbox id: {err}")))?;
        pending_inbox.push(InboxRecord {
            id: InboxId::from_uuid(uuid),
            kind: decode("inbox kind", kind)?,
            text,
            status: InboxStatus::Pending,
        });
    }

    let mut statement = connection.prepare(
        "SELECT effect_id, operation_id, step, frame_seq, text, thinking
         FROM assistant_frames WHERE session_id = ?1 ORDER BY frame_seq",
    )?;
    let mut assistant_frames = Vec::new();
    let mut frame_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = frame_rows.next()? {
        let effect_id: String = row.get(0)?;
        let operation_id: String = row.get(1)?;
        let effect_id = Uuid::parse_str(&effect_id)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
        let operation_id = Uuid::parse_str(&operation_id)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
        assistant_frames.push(AssistantFrame {
            effect_id: EffectId::from_uuid(effect_id),
            session_id,
            operation_id: OperationId::from_uuid(operation_id),
            step: row.get::<_, i64>(2)? as u64,
            frame_seq: row.get::<_, i64>(3)? as u64,
            text: row.get(4)?,
            thinking: row.get(5)?,
        });
    }

    Ok(LoadedSession {
        session,
        entries,
        operations,
        pending_inbox,
        assistant_frames,
    })
}

fn entry_kind(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::UserMessage { .. } => "user_message",
        SessionEntry::ModelChanged { .. } => "model_changed",
        SessionEntry::AssistantMessage { .. } => "assistant_message",
        SessionEntry::ToolCall { .. } => "tool_call",
        SessionEntry::ToolResult { .. } => "tool_result",
        SessionEntry::Compaction { .. } => "compaction",
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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
