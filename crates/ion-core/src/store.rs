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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::ids::{EffectId, InboxId, OperationId, SessionId};
use crate::session::{InboxKind, OperationState, SessionEntry};
use crate::tool::RecoveryClass;

const STORE_CAPACITY: usize = 64;

const SCHEMA_VERSION: i64 = 2;

/// Ordered, transactional migrations gated by `PRAGMA user_version`
/// (DESIGN.md §11.1). Version 0 (fresh or pre-versioning) applies the
/// initial schema; a database from a newer Ion is refused, never
/// silently reinterpreted (§26.3).
fn apply_migrations(connection: &mut Connection) -> Result<(), StoreError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Sqlite(format!(
            "database schema version {version} is newer than this Ion supports ({SCHEMA_VERSION})"
        )));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = connection.transaction()?;
    if version < 1 {
        // Fresh database: the initial schema is already at the current
        // shape.
        tx.execute_batch(SCHEMA)?;
    }
    if version == 1 {
        // v2: effect attempts, persisted so recovery replays are
        // countable (DESIGN.md §11.3, §15.4).
        tx.execute_batch("ALTER TABLE effects ADD COLUMN attempt INTEGER NOT NULL DEFAULT 1")?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    cwd TEXT NOT NULL,
    title TEXT NOT NULL
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
    accepted_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS operation_states (
    operation_id TEXT NOT NULL REFERENCES operations(id),
    state_seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (operation_id, state_seq)
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
";

/// One durable session row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub cwd: String,
    pub title: String,
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
    pub tools: Vec<crate::tool::ToolSpec>,
    pub open_effect: Option<EffectRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRecord {
    pub state_seq: u64,
    pub payload: CheckpointPayload,
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
}

/// A session loaded from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSession {
    pub session: SessionRecord,
    pub entries: Vec<(u64, SessionEntry)>,
    pub operations: Vec<LoadedOperation>,
    pub pending_inbox: Vec<InboxRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedOperation {
    pub id: OperationId,
    pub latest: (u64, CheckpointPayload),
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
    Load {
        session_id: SessionId,
        reply: oneshot::Sender<Result<LoadedSession, StoreError>>,
    },
}

/// Handle to the store thread. Cheap to clone.
#[derive(Clone)]
pub struct SessionStore {
    tx: mpsc::Sender<StoreCommand>,
    /// Test hook: fail the next mutating command (DESIGN.md §30.5).
    fail_next_write: Arc<AtomicBool>,
    _join: Arc<JoinHandle<()>>,
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
        Self::start(connection)
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        Self::start(connection)
    }

    fn start(mut connection: Connection) -> Result<Self, StoreError> {
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
                    handle_command(&mut connection, command, &fail_flag);
                }
            })
            .map_err(|err| StoreError::Sqlite(err.to_string()))?;
        Ok(Self {
            tx,
            fail_next_write,
            _join: Arc::new(join),
        })
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

    pub async fn load(&self, session_id: SessionId) -> Result<LoadedSession, StoreError> {
        self.request(|reply| StoreCommand::Load { session_id, reply })
            .await
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
        StoreCommand::Load { session_id, reply } => {
            let _ = reply.send(load(connection, session_id));
        }
    }
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
        "INSERT INTO sessions (id, created_at, updated_at, cwd, title) VALUES (?1, ?2, ?2, ?3, ?4)",
        rusqlite::params![
            record.id.as_uuid().to_string(),
            now,
            record.cwd,
            record.title
        ],
    )?;
    Ok(())
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
    tx.execute(
        "INSERT INTO operations (id, session_id, kind, accepted_at) VALUES (?1, ?2, 'run', ?3)",
        rusqlite::params![
            operation_id.as_uuid().to_string(),
            session_id.as_uuid().to_string(),
            now_ms()
        ],
    )?;
    insert_inbox(&tx, session_id, operation_id, root_inbox)?;
    insert_checkpoint(&tx, operation_id, checkpoint)?;
    insert_entry(&tx, session_id, entry)?;
    tx.commit()?;
    Ok(())
}

fn commit(connection: &mut Connection, request: &CommitRequest) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
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
    tx.commit()?;
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
    let payload = serde_json::to_string(&checkpoint.payload)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO operation_states (operation_id, state_seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            operation_id.as_uuid().to_string(),
            checkpoint.state_seq as i64,
            state_kind(&checkpoint.payload.state),
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
            "SELECT cwd, title FROM sessions WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok(SessionRecord {
                    id: session_id,
                    cwd: row.get(0)?,
                    title: row.get(1)?,
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
        "SELECT o.id, s.state_seq, s.payload FROM operations o
         JOIN operation_states s ON s.operation_id = o.id
         WHERE o.session_id = ?1
         AND s.state_seq = (SELECT MAX(state_seq) FROM operation_states WHERE operation_id = o.id)",
    )?;
    let mut operations = Vec::new();
    let mut op_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = op_rows.next()? {
        let op_id: String = row.get(0)?;
        let state_seq: i64 = row.get(1)?;
        let payload: String = row.get(2)?;
        let uuid = Uuid::parse_str(&op_id)
            .map_err(|err| StoreError::Sqlite(format!("corrupt operation id: {err}")))?;
        operations.push(LoadedOperation {
            id: OperationId::from_uuid(uuid),
            latest: (state_seq as u64, decode("checkpoint", payload)?),
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

    Ok(LoadedSession {
        session,
        entries,
        operations,
        pending_inbox,
    })
}

fn state_kind(state: &OperationState) -> &'static str {
    match state {
        OperationState::Accepted => "accepted",
        OperationState::NeedAssistant => "need_assistant",
        OperationState::AssistantEffectPending => "assistant_effect_pending",
        OperationState::ToolsPlanned { .. } => "tools_planned",
        OperationState::ToolEffectPending { .. } => "tool_effect_pending",
        OperationState::NeedContinuation => "need_continuation",
        OperationState::Suspended => "suspended",
        OperationState::Finished(_) => "finished",
    }
}

fn entry_kind(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::UserMessage { .. } => "user_message",
        SessionEntry::AssistantMessage { .. } => "assistant_message",
        SessionEntry::ToolCall { .. } => "tool_call",
        SessionEntry::ToolResult { .. } => "tool_result",
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
