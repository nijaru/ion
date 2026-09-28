//! The single durable authority for a local coding conversation.
//! SQLite commits related events atomically before any external effect runs.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use ion_ai::{Content, Message, ModelRef, Role, ToolCall, ToolResult};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

const FORMAT_VERSION: u32 = 1;
const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    version: u32,
    cwd: PathBuf,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SessionEntry {
    ModelSelected {
        model: ModelRef,
    },
    Steering {
        turn: u64,
        prompt: String,
    },
    TurnStarted {
        turn: u64,
        prompt: String,
        model: ModelRef,
    },
    Assistant {
        turn: u64,
        message: Message,
    },
    ToolResult {
        turn: u64,
        result: ToolResult,
    },
    TurnEnded {
        turn: u64,
        reason: TurnEndReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Interrupted,
    Failed(String),
    StepLimit,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub cwd: PathBuf,
    pub name: Option<String>,
    pub entries: Vec<SessionEntry>,
    pub messages: Vec<Message>,
    pub unfinished_turn: Option<u64>,
    pub last_end: Option<(u64, TurnEndReason)>,
    pub last_model: Option<ModelRef>,
}

#[derive(Default, Clone)]
struct State {
    active: Option<u64>,
    pending: BTreeMap<String, String>,
    last_id: u64,
    last_end: Option<(u64, TurnEndReason)>,
    last_model: Option<ModelRef>,
}

impl State {
    fn apply(
        &mut self,
        entry: &SessionEntry,
        messages: &mut Vec<Message>,
    ) -> Result<(), SessionError> {
        match entry {
            SessionEntry::ModelSelected { model } => {
                if self.active.is_some() {
                    return Err(SessionError::InvalidHistory);
                }
                self.last_model = Some(model.clone());
            }
            SessionEntry::TurnStarted {
                turn,
                prompt,
                model,
            } => {
                if self.active.is_some()
                    || *turn != self.last_id.saturating_add(1)
                    || prompt.trim().is_empty()
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.active = Some(*turn);
                self.last_id = *turn;
                self.last_model = Some(model.clone());
                messages.push(Message {
                    role: Role::User,
                    content: vec![Content::Text(prompt.clone())],
                    provider_replay: None,
                });
            }
            SessionEntry::Steering { turn, prompt } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || prompt.trim().is_empty()
                {
                    return Err(SessionError::InvalidHistory);
                }
                messages.push(Message {
                    role: Role::User,
                    content: vec![Content::Text(prompt.clone())],
                    provider_replay: None,
                });
            }
            SessionEntry::Assistant { turn, message } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || message.role != Role::Assistant
                {
                    return Err(SessionError::InvalidHistory);
                }
                for part in &message.content {
                    match part {
                        Content::ToolCall(ToolCall { id, name, .. })
                            if !id.is_empty() && !name.is_empty() =>
                        {
                            if self.pending.insert(id.clone(), name.clone()).is_some() {
                                return Err(SessionError::InvalidHistory);
                            }
                        }
                        Content::Text(_) => {}
                        _ => return Err(SessionError::InvalidHistory),
                    }
                }
                messages.push(message.clone());
            }
            SessionEntry::ToolResult { turn, result } => {
                if self.active != Some(*turn)
                    || self.pending.remove(&result.call_id).as_deref() != Some(&result.name)
                {
                    return Err(SessionError::InvalidHistory);
                }
                messages.push(Message {
                    role: Role::Tool,
                    content: vec![Content::ToolResult(result.clone())],
                    provider_replay: None,
                });
            }
            SessionEntry::TurnEnded { turn, reason } => {
                if self.active != Some(*turn) || !self.pending.is_empty() {
                    return Err(SessionError::InvalidHistory);
                }
                self.active = None;
                self.last_end = Some((*turn, reason.clone()));
            }
        }
        Ok(())
    }
}

struct Store {
    connection: Connection,
    state: State,
    messages: Vec<Message>,
}

/// A writable Session holds a cross-process lock. `submit_gate` also keeps a
/// whole live Turn exclusive within this process, including its async effects.
pub struct Session {
    store: Mutex<Store>,
    _lock: File,
    header: Header,
    path: PathBuf,
    pub(crate) submit_gate: AsyncMutex<()>,
}

impl Session {
    pub fn create(path: impl AsRef<Path>, cwd: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref();
        let cwd = cwd.as_ref().canonicalize()?;
        if !cwd.is_dir() {
            return Err(SessionError::InvalidWorkingDirectory);
        }
        fs::create_dir_all(path.parent().ok_or(SessionError::InvalidPath)?)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let lock = lock(path)?;
        let mut connection = Connection::open(path)?;
        initialize(&connection)?;
        let header = Header {
            version: FORMAT_VERSION,
            cwd,
            name: None,
        };
        let tx = connection.transaction()?;
        tx.execute(
            "INSERT INTO session(id, header) VALUES (1, ?1)",
            params![serde_json::to_vec(&header)?],
        )?;
        tx.commit()?;
        Ok(Self {
            store: Mutex::new(Store {
                connection,
                state: State::default(),
                messages: Vec::new(),
            }),
            _lock: lock,
            header,
            path: path.to_owned(),
            submit_gate: AsyncMutex::new(()),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(SessionError::NotFound);
        }
        let lock = lock(path)?;
        let connection = Connection::open(path)?;
        initialize(&connection)?;
        let header = read_header(&connection)?;
        let (state, messages) = project(&read_entries(&connection)?)?;
        Ok(Self {
            store: Mutex::new(Store {
                connection,
                state,
                messages,
            }),
            _lock: lock,
            header,
            path: path.to_owned(),
            submit_gate: AsyncMutex::new(()),
        })
    }

    /// Read persisted facts without taking write ownership or repairing history.
    pub fn inspect(path: impl AsRef<Path>) -> Result<SessionView, SessionError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let header = read_header(&connection)?;
        let entries = read_entries(&connection)?;
        let (state, messages) = project(&entries)?;
        Ok(SessionView {
            cwd: header.cwd,
            name: header.name,
            entries,
            messages,
            unfinished_turn: state.active,
            last_end: state.last_end,
            last_model: state.last_model,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn cwd(&self) -> &Path {
        &self.header.cwd
    }
    pub fn messages(&self) -> Result<Vec<Message>, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .messages
            .clone())
    }
    pub fn view(&self) -> Result<SessionView, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entries = read_entries(&store.connection)?;
        let header = read_header(&store.connection)?;
        Ok(SessionView {
            cwd: header.cwd,
            name: header.name,
            entries,
            messages: store.messages.clone(),
            unfinished_turn: store.state.active,
            last_end: store.state.last_end.clone(),
            last_model: store.state.last_model.clone(),
        })
    }

    /// Change only display metadata. Conversation history remains append-only.
    pub fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        if name.is_some_and(|name| name.len() > 120 || name.chars().any(char::is_control)) {
            return Err(SessionError::InvalidName);
        }
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let mut header = read_header(&store.connection)?;
        header.name = name.map(str::to_owned);
        store.connection.execute(
            "UPDATE session SET header = ?1 WHERE id = 1",
            params![serde_json::to_vec(&header)?],
        )?;
        Ok(())
    }

    /// Persist an idle Session's choice even when no new Turn has been sent.
    pub fn select_model(&self, model: ModelRef) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &[SessionEntry::ModelSelected { model }])
    }

    /// Close any interrupted Turn and accept the next input in one transaction.
    /// Caller holds `submit_gate` for the entire resulting Turn.
    pub(crate) fn begin_turn(
        &self,
        prompt: String,
        model: ModelRef,
    ) -> Result<(u64, usize), SessionError> {
        if prompt.trim().is_empty() {
            return Err(SessionError::EmptyPrompt);
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let mut entries = Vec::new();
        let interrupted = store.state.pending.len();
        if let Some(old) = store.state.active {
            entries.extend(unknown_results(old, &store.state.pending));
            entries.push(SessionEntry::TurnEnded {
                turn: old,
                reason: TurnEndReason::Interrupted,
            });
        }
        let turn = store
            .state
            .last_id
            .checked_add(1)
            .ok_or(SessionError::TurnIdExhausted)?;
        entries.push(SessionEntry::TurnStarted {
            turn,
            prompt,
            model,
        });
        append(&mut store, &entries)?;
        Ok((turn, interrupted))
    }

    pub(crate) fn record_assistant(
        &self,
        turn: u64,
        message: Message,
        continue_turn: bool,
    ) -> Result<bool, SessionError> {
        let has_calls = message
            .content
            .iter()
            .any(|part| matches!(part, Content::ToolCall(_)));
        let mut entries = vec![SessionEntry::Assistant { turn, message }];
        if !has_calls && !continue_turn {
            entries.push(SessionEntry::TurnEnded {
                turn,
                reason: TurnEndReason::Completed,
            });
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &entries)?;
        Ok(!has_calls && !continue_turn)
    }

    pub(crate) fn record_steering(&self, turn: u64, prompt: String) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &[SessionEntry::Steering { turn, prompt }])
    }

    pub(crate) fn record_tool_result(
        &self,
        turn: u64,
        result: ToolResult,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &[SessionEntry::ToolResult { turn, result }])
    }

    pub(crate) fn end_turn(&self, turn: u64, reason: TurnEndReason) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store.state.active != Some(turn) {
            return Err(SessionError::InvalidHistory);
        }
        let mut entries = unknown_results(turn, &store.state.pending);
        entries.push(SessionEntry::TurnEnded { turn, reason });
        append(&mut store, &entries)
    }
}

fn unknown_results(turn: u64, pending: &BTreeMap<String, String>) -> Vec<SessionEntry> {
    pending.iter().map(|(call_id, name)| SessionEntry::ToolResult { turn, result: ToolResult {
        call_id: call_id.clone(), name: name.clone(),
        result: serde_json::json!({"error":"The tool result was not committed. Its external effect is unknown; inspect the working directory before retrying."}),
    }}).collect()
}

fn append(store: &mut Store, entries: &[SessionEntry]) -> Result<(), SessionError> {
    let mut candidate = store.state.clone();
    let mut new_messages = Vec::new();
    let encoded = entries
        .iter()
        .map(|entry| {
            candidate.apply(entry, &mut new_messages)?;
            let bytes = serde_json::to_vec(entry)?;
            if bytes.len() > MAX_ENTRY_BYTES {
                return Err(SessionError::EntryTooLarge);
            }
            Ok(bytes)
        })
        .collect::<Result<Vec<_>, SessionError>>()?;
    let tx = store.connection.transaction()?;
    for body in encoded {
        tx.execute("INSERT INTO entries(body) VALUES (?1)", params![body])?;
    }
    tx.commit()?;
    store.state = candidate;
    store.messages.extend(new_messages);
    Ok(())
}

fn project(entries: &[SessionEntry]) -> Result<(State, Vec<Message>), SessionError> {
    let mut state = State::default();
    let mut messages = Vec::new();
    for entry in entries {
        state.apply(entry, &mut messages)?;
    }
    Ok((state, messages))
}

fn read_entries(connection: &Connection) -> Result<Vec<SessionEntry>, SessionError> {
    let mut statement = connection.prepare("SELECT body FROM entries ORDER BY seq")?;
    let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    let mut entries = Vec::new();
    for body in rows {
        entries.push(serde_json::from_slice(&body?)?);
    }
    Ok(entries)
}

fn read_header(connection: &Connection) -> Result<Header, SessionError> {
    let encoded: Vec<u8> = connection
        .query_row("SELECT header FROM session WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(SessionError::InvalidDatabase)?;
    let header: Header = serde_json::from_slice(&encoded)?;
    if header.version != FORMAT_VERSION {
        return Err(SessionError::UnsupportedFormat(header.version));
    }
    Ok(header)
}

fn initialize(connection: &Connection) -> Result<(), SessionError> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS session (id INTEGER PRIMARY KEY CHECK(id=1), header BLOB NOT NULL); CREATE TABLE IF NOT EXISTS entries (seq INTEGER PRIMARY KEY, body BLOB NOT NULL);")?;
    Ok(())
}

fn lock(path: &Path) -> Result<File, SessionError> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"))?;
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK {
            SessionError::AlreadyOpen
        } else {
            SessionError::Io(error.into())
        }
    })?;
    Ok(file)
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session does not exist")]
    NotFound,
    #[error("session is already open for writing")]
    AlreadyOpen,
    #[error("invalid session path")]
    InvalidPath,
    #[error("invalid working directory")]
    InvalidWorkingDirectory,
    #[error("invalid or inconsistent session history")]
    InvalidHistory,
    #[error("invalid session database")]
    InvalidDatabase,
    #[error("unsupported session format version {0}")]
    UnsupportedFormat(u32),
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("session name must be at most 120 bytes without control characters")]
    InvalidName,
    #[error("session entry exceeds storage limit")]
    EntryTooLarge,
    #[error("turn identifier space exhausted")]
    TurnIdExhausted,
    #[error("session mutex was poisoned")]
    Poisoned,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::ToolCall;

    fn fixture() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("ion-session-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        (root.clone(), root.join("session.sqlite"))
    }

    #[test]
    fn interrupted_call_closes_atomically_with_next_input() {
        let (root, path) = fixture();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let session = Session::create(&path, &root).unwrap();
        let (turn, _) = session.begin_turn("first".into(), model.clone()).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "call".into(),
                        name: "write".into(),
                        arguments: serde_json::json!({}),
                    })],
                    provider_replay: None,
                },
                false,
            )
            .unwrap();
        drop(session);
        let before = Session::inspect(&path).unwrap();
        assert_eq!(before.unfinished_turn, Some(turn));
        assert_eq!(before.entries.len(), 2);
        let reopened = Session::open(&path).unwrap();
        let (next, interrupted) = reopened.begin_turn("second".into(), model).unwrap();
        assert_eq!((next, interrupted), (turn + 1, 1));
        let entries = reopened.view().unwrap().entries;
        assert!(matches!(entries[2], SessionEntry::ToolResult { .. }));
        assert!(matches!(
            entries[3],
            SessionEntry::TurnEnded {
                reason: TurnEndReason::Interrupted,
                ..
            }
        ));
        assert!(matches!(entries[4], SessionEntry::TurnStarted { .. }));
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn final_answer_and_end_share_a_commit() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let (turn, _) = session.begin_turn("hello".into(), model).unwrap();
        assert!(
            session
                .record_assistant(
                    turn,
                    Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None
                    },
                    false,
                )
                .unwrap()
        );
        let view = session.view().unwrap();
        assert_eq!(view.last_end, Some((turn, TurnEndReason::Completed)));
        assert_eq!(view.entries.len(), 3);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn name_is_visible_after_reopen_without_changing_history() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        session.set_name(Some("  Investigate parser  ")).unwrap();
        assert_eq!(
            session.view().unwrap().name.as_deref(),
            Some("Investigate parser")
        );
        assert!(session.view().unwrap().entries.is_empty());
        drop(session);
        assert_eq!(
            Session::inspect(&path).unwrap().name.as_deref(),
            Some("Investigate parser")
        );
        let reopened = Session::open(&path).unwrap();
        reopened.set_name(None).unwrap();
        assert!(reopened.view().unwrap().name.is_none());
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn idle_model_choice_survives_reopen() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "new".into(),
        };
        session.select_model(model.clone()).unwrap();
        drop(session);
        assert_eq!(Session::inspect(&path).unwrap().last_model, Some(model));
        fs::remove_dir_all(root).unwrap();
    }
}
