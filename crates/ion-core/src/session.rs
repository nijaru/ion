//! The single durable authority for a local coding conversation.
//! SQLite commits related events atomically before any external effect runs.
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use ion_ai::{
    Content, IncompleteReason, Message, ModelRef, ResponseTermination, Role, ToolCall, ToolResult,
    Usage,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

const FORMAT_VERSION: u32 = 2;
// An 8 MiB raw prompt or streamed response can grow up to sixfold when JSON
// escapes control characters. Keep the storage bound above that encoded size.
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

fn completed_termination() -> ResponseTermination {
    ResponseTermination::Completed
}

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
    Compacted {
        through_entry: u64,
        summary: String,
        #[serde(default = "Usage::unknown")]
        usage: Usage,
    },
    TurnStarted {
        turn: u64,
        input: Message,
        model: ModelRef,
    },
    Assistant {
        turn: u64,
        message: Message,
        #[serde(default = "Usage::unknown")]
        usage: Usage,
        #[serde(default = "completed_termination")]
        termination: ResponseTermination,
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
    pub compacted_through: Option<u64>,
    pub last_usage: Option<Usage>,
}

pub(crate) struct CompactionPlan {
    pub through_entry: u64,
    pub messages: Vec<Message>,
    pub chunked: bool,
}

#[derive(Default, Clone)]
struct State {
    active: Option<u64>,
    pending: Vec<(String, String)>,
    last_id: u64,
    last_end: Option<(u64, TurnEndReason)>,
    last_model: Option<ModelRef>,
    sequence: u64,
    compaction: Option<(u64, String)>,
    last_usage: Option<Usage>,
}

impl State {
    fn apply(
        &mut self,
        entry: &SessionEntry,
        messages: &mut Vec<Message>,
        settled: &BTreeSet<u64>,
        new_settled: &mut Vec<u64>,
    ) -> Result<(), SessionError> {
        match entry {
            SessionEntry::ModelSelected { model } => {
                if self.active.is_some() {
                    return Err(SessionError::InvalidHistory);
                }
                self.last_model = Some(model.clone());
            }
            SessionEntry::Compacted {
                through_entry,
                summary,
                ..
            } => {
                if !self.pending.is_empty()
                    || summary.trim().is_empty()
                    || !(settled.contains(through_entry) || new_settled.contains(through_entry))
                    || self
                        .compaction
                        .as_ref()
                        .is_some_and(|(previous, _)| through_entry <= previous)
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.compaction = Some((*through_entry, summary.clone()));
            }
            SessionEntry::TurnStarted { turn, input, model } => {
                if self.active.is_some()
                    || *turn != self.last_id.saturating_add(1)
                    || !valid_user_message(input)
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.active = Some(*turn);
                self.last_id = *turn;
                self.last_model = Some(model.clone());
                messages.push(input.clone());
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
            SessionEntry::Assistant {
                turn,
                message,
                usage,
                termination,
            } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || message.role != Role::Assistant
                {
                    return Err(SessionError::InvalidHistory);
                }
                if matches!(termination, ResponseTermination::Incomplete(_))
                    && (!matches!(
                        termination,
                        ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
                    ) || !message
                        .content
                        .iter()
                        .any(|part| matches!(part, Content::ToolCall(_))))
                {
                    return Err(SessionError::InvalidHistory);
                }
                for part in &message.content {
                    match part {
                        Content::ToolCall(ToolCall { id, name, .. })
                            if !id.is_empty() && !name.is_empty() =>
                        {
                            if self.pending.iter().any(|(pending_id, _)| pending_id == id) {
                                return Err(SessionError::InvalidHistory);
                            }
                            self.pending.push((id.clone(), name.clone()));
                        }
                        Content::Text(_) => {}
                        _ => return Err(SessionError::InvalidHistory),
                    }
                }
                messages.push(message.clone());
                self.last_usage = Some(*usage);
                if self.pending.is_empty() {
                    new_settled.push(self.sequence + 1);
                }
            }
            SessionEntry::ToolResult { turn, result } => {
                let position = self
                    .pending
                    .iter()
                    .position(|(id, name)| id == &result.call_id && name == &result.name);
                if self.active != Some(*turn) || position.is_none() {
                    return Err(SessionError::InvalidHistory);
                }
                self.pending.remove(position.expect("checked above"));
                messages.push(Message {
                    role: Role::Tool,
                    content: vec![Content::ToolResult(result.clone())],
                    provider_replay: None,
                });
                if self.pending.is_empty() {
                    new_settled.push(self.sequence + 1);
                }
            }
            SessionEntry::TurnEnded { turn, reason } => {
                if self.active != Some(*turn) || !self.pending.is_empty() {
                    return Err(SessionError::InvalidHistory);
                }
                self.active = None;
                self.last_end = Some((*turn, reason.clone()));
                new_settled.push(self.sequence + 1);
            }
        }
        self.sequence += 1;
        Ok(())
    }
}

struct Store {
    connection: Connection,
    state: State,
    settled: BTreeSet<u64>,
    messages: Vec<Message>,
}

/// A writable Session holds a cross-process lock. `submit_gate` also keeps a
/// whole live Turn exclusive within this process, including its async effects.
pub struct Session {
    // Field order closes SQLite before releasing the writer lock.
    store: Mutex<Store>,
    _lock: SessionLock,
    header: Header,
    path: PathBuf,
    pub(crate) submit_gate: AsyncMutex<()>,
}

struct SessionLock(File);

impl Drop for SessionLock {
    fn drop(&mut self) {
        // A child may briefly inherit a duplicate file description. Release
        // this writer's lease explicitly before closing our descriptor.
        let _ = flock(&self.0, FlockOperation::Unlock);
    }
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
                settled: BTreeSet::new(),
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
        let (state, settled, messages) = project(&read_entries(&connection)?)?;
        Ok(Self {
            store: Mutex::new(Store {
                connection,
                state,
                settled,
                messages,
            }),
            _lock: lock,
            header,
            path: path.to_owned(),
            submit_gate: AsyncMutex::new(()),
        })
    }

    /// Copy committed conversation and context into a new Session. Future
    /// entries are independent; the working directory remains shared.
    pub fn clone_to(&self, path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref();
        let source = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entries = read_entries(&source.connection)?;
        let clone = Self::create(path, &self.header.cwd)?;
        let copied = {
            let mut target = clone.store.lock().map_err(|_| SessionError::Poisoned)?;
            append(&mut target, &entries)
        };
        if let Err(error) = copied {
            drop(clone);
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                let _ = fs::remove_file(PathBuf::from(sidecar));
            }
            let _ = fs::remove_file(path.with_extension("lock"));
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(clone)
    }

    /// Read persisted facts without taking write ownership or repairing history.
    pub fn inspect(path: impl AsRef<Path>) -> Result<SessionView, SessionError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let header = read_header(&connection)?;
        let entries = read_entries(&connection)?;
        let (state, _, messages) = project(&entries)?;
        Ok(SessionView {
            cwd: header.cwd,
            name: header.name,
            entries,
            messages,
            unfinished_turn: state.active,
            last_end: state.last_end,
            last_model: state.last_model,
            compacted_through: state.compaction.map(|(through, _)| through),
            last_usage: state.last_usage,
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

    pub fn entry_count(&self) -> Result<u64, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .sequence)
    }

    /// The model-facing projection. Inspect and export still use raw history.
    pub fn context_messages(&self) -> Result<Vec<Message>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        match &store.state.compaction {
            Some((through, summary)) => {
                let entries = read_entries_after(&store.connection, *through)?;
                let mut messages = vec![summary_message(summary)];
                messages.extend(messages_from_entries(&entries));
                Ok(messages)
            }
            None => Ok(store.messages.clone()),
        }
    }

    /// Find the earliest settled cut that fits a useful recent suffix.
    /// The returned prefix includes the previous summary, if any.
    pub(crate) fn compaction_plan(
        &self,
        keep_bytes: usize,
        max_summary_bytes: usize,
    ) -> Result<Option<CompactionPlan>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entries = read_entries(&store.connection)?;
        let previous = store
            .state
            .compaction
            .as_ref()
            .map_or(0, |(through, _)| *through);
        let summary_bytes = store
            .state
            .compaction
            .as_ref()
            .map(|(_, summary)| serde_json::to_vec(&summary_message(summary)))
            .transpose()?
            .map_or(0, |bytes| bytes.len() + 1);
        let suffix_budget = keep_bytes.saturating_sub(summary_bytes);
        let mut prefix_bytes = summary_bytes.saturating_add(2);
        let mut prefix_fit = None;
        for boundary in (previous + 1)..=entries.len() as u64 {
            if let Some(message) = message_from_entry(&entries[boundary as usize - 1]) {
                prefix_bytes = prefix_bytes
                    .saturating_add(serde_json::to_vec(&message)?.len())
                    .saturating_add(1);
            }
            if prefix_bytes <= max_summary_bytes && store.settled.contains(&boundary) {
                prefix_fit = Some(boundary);
            }
        }
        let mut suffix_bytes = 2usize;
        let mut through = None;
        for boundary in ((previous + 1)..=entries.len() as u64).rev() {
            if store.settled.contains(&boundary) && suffix_bytes <= suffix_budget {
                through = Some(boundary);
            }
            if let Some(message) = message_from_entry(&entries[boundary as usize - 1]) {
                suffix_bytes = suffix_bytes.saturating_add(serde_json::to_vec(&message)?.len() + 1);
            }
        }
        let through =
            through.or_else(|| store.settled.range((previous + 1)..).next_back().copied());
        let Some((suffix_target, prefix_fit)) = through.zip(prefix_fit) else {
            return Ok(None);
        };
        let through_entry = suffix_target.min(prefix_fit);
        let mut messages = Vec::new();
        if let Some((_, summary)) = &store.state.compaction {
            messages.push(summary_message(summary));
        }
        messages.extend(messages_from_entries(
            &entries[previous as usize..through_entry as usize],
        ));
        Ok(Some(CompactionPlan {
            through_entry,
            messages,
            chunked: through_entry < suffix_target,
        }))
    }

    pub(crate) fn record_compaction(
        &self,
        through_entry: u64,
        summary: String,
        usage: Usage,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::Compacted {
                through_entry,
                summary,
                usage,
            }],
        )
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
            compacted_through: store.state.compaction.as_ref().map(|(through, _)| *through),
            last_usage: store.state.last_usage,
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
    #[cfg(test)]
    pub(crate) fn begin_turn(
        &self,
        prompt: String,
        model: ModelRef,
    ) -> Result<(u64, usize), SessionError> {
        self.begin_turn_message(
            Message {
                role: Role::User,
                content: vec![Content::Text(prompt)],
                provider_replay: None,
            },
            model,
        )
    }

    pub(crate) fn begin_turn_message(
        &self,
        input: Message,
        model: ModelRef,
    ) -> Result<(u64, usize), SessionError> {
        if !valid_user_message(&input) {
            return Err(SessionError::InvalidUserInput);
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
        entries.push(SessionEntry::TurnStarted { turn, input, model });
        append(&mut store, &entries)?;
        Ok((turn, interrupted))
    }

    pub(crate) fn record_assistant(
        &self,
        turn: u64,
        message: Message,
        usage: Usage,
        continue_turn: bool,
    ) -> Result<bool, SessionError> {
        self.record_assistant_entries(turn, message, usage, continue_turn, Vec::new())
    }

    /// Publish the completed assistant and queued steering in one batch; a
    /// failed transaction must leave both unpublished for the host to recover.
    pub(crate) fn record_assistant_with_steering(
        &self,
        turn: u64,
        message: Message,
        usage: Usage,
        steering: Vec<String>,
    ) -> Result<bool, SessionError> {
        let continue_turn = !steering.is_empty();
        self.record_assistant_entries(turn, message, usage, continue_turn, steering)
    }

    fn record_assistant_entries(
        &self,
        turn: u64,
        message: Message,
        usage: Usage,
        continue_turn: bool,
        steering: Vec<String>,
    ) -> Result<bool, SessionError> {
        let has_calls = message
            .content
            .iter()
            .any(|part| matches!(part, Content::ToolCall(_)));
        let mut entries = vec![SessionEntry::Assistant {
            turn,
            message,
            usage,
            termination: ResponseTermination::Completed,
        }];
        if !has_calls && !continue_turn {
            entries.push(SessionEntry::TurnEnded {
                turn,
                reason: TurnEndReason::Completed,
            });
        }
        entries.extend(
            steering
                .into_iter()
                .map(|prompt| SessionEntry::Steering { turn, prompt }),
        );
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &entries)?;
        Ok(!has_calls && !continue_turn)
    }

    /// Publish a truncated assistant attempt and every unexecuted call's
    /// synthetic result together. A crash cannot leave these calls pending
    /// and falsely classify their effects as unknown on reopen.
    pub(crate) fn record_truncated_assistant(
        &self,
        turn: u64,
        message: Message,
        usage: Usage,
    ) -> Result<Vec<ToolResult>, SessionError> {
        let results = message
            .content
            .iter()
            .filter_map(|part| match part {
                Content::ToolCall(call) => Some(ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    result: serde_json::json!({"error": "Tool call was not executed: the model response hit the output token limit and its arguments may be truncated. Reissue the complete call."}),
                    is_error: true,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        if results.is_empty() {
            return Err(SessionError::InvalidHistory);
        }
        let mut entries = vec![SessionEntry::Assistant {
            turn,
            message,
            usage,
            termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
        }];
        entries.extend(
            results
                .iter()
                .cloned()
                .map(|result| SessionEntry::ToolResult { turn, result }),
        );
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &entries)?;
        Ok(results)
    }

    pub(crate) fn record_steerings(
        &self,
        turn: u64,
        prompts: Vec<String>,
    ) -> Result<(), SessionError> {
        if prompts.is_empty() {
            return Ok(());
        }
        let entries = prompts
            .into_iter()
            .map(|prompt| SessionEntry::Steering { turn, prompt })
            .collect::<Vec<_>>();
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &entries)
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

fn unknown_results(turn: u64, pending: &[(String, String)]) -> Vec<SessionEntry> {
    pending.iter().map(|(call_id, name)| SessionEntry::ToolResult { turn, result: ToolResult {
        call_id: call_id.clone(), name: name.clone(),
        result: serde_json::json!({"error":"The tool result was not committed. Its external effect is unknown; inspect the working directory before retrying."}),
        is_error: true,
    }}).collect()
}

fn append(store: &mut Store, entries: &[SessionEntry]) -> Result<(), SessionError> {
    let mut candidate = store.state.clone();
    let mut new_messages = Vec::new();
    let mut new_settled = Vec::new();
    let encoded = entries
        .iter()
        .map(|entry| {
            candidate.apply(entry, &mut new_messages, &store.settled, &mut new_settled)?;
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
    store.settled.extend(new_settled);
    store.messages.extend(new_messages);
    Ok(())
}

fn summary_message(summary: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(format!(
            "Earlier conversation summary (retain its constraints and progress):\n{summary}"
        ))],
        provider_replay: None,
    }
}

fn messages_from_entries(entries: &[SessionEntry]) -> Vec<Message> {
    entries.iter().filter_map(message_from_entry).collect()
}

fn message_from_entry(entry: &SessionEntry) -> Option<Message> {
    match entry {
        SessionEntry::TurnStarted { input, .. } => Some(input.clone()),
        SessionEntry::Steering { prompt, .. } => Some(Message {
            role: Role::User,
            content: vec![Content::Text(prompt.clone())],
            provider_replay: None,
        }),
        SessionEntry::Assistant { message, .. } => Some(message.clone()),
        SessionEntry::ToolResult { result, .. } => Some(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(result.clone())],
            provider_replay: None,
        }),
        SessionEntry::ModelSelected { .. }
        | SessionEntry::Compacted { .. }
        | SessionEntry::TurnEnded { .. } => None,
    }
}

fn valid_user_message(message: &Message) -> bool {
    message.role == Role::User
        && message.provider_replay.is_none()
        && !message.content.is_empty()
        && message.content.iter().all(|part| match part {
            Content::Text(_) => true,
            Content::Image(image) => image.validate().is_ok(),
            Content::ToolCall(_) | Content::ToolResult(_) => false,
        })
        && message.content.iter().any(|part| match part {
            Content::Text(text) => !text.trim().is_empty(),
            Content::Image(_) => true,
            Content::ToolCall(_) | Content::ToolResult(_) => false,
        })
}

fn project(entries: &[SessionEntry]) -> Result<(State, BTreeSet<u64>, Vec<Message>), SessionError> {
    let mut state = State::default();
    let mut settled = BTreeSet::new();
    let mut messages = Vec::new();
    for entry in entries {
        let mut new_settled = Vec::new();
        state.apply(entry, &mut messages, &settled, &mut new_settled)?;
        settled.extend(new_settled);
    }
    Ok((state, settled, messages))
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

fn read_entries_after(
    connection: &Connection,
    through: u64,
) -> Result<Vec<SessionEntry>, SessionError> {
    let through = i64::try_from(through).map_err(|_| SessionError::InvalidHistory)?;
    let mut statement =
        connection.prepare("SELECT body FROM entries WHERE seq > ?1 ORDER BY seq")?;
    let rows = statement.query_map(params![through], |row| row.get::<_, Vec<u8>>(0))?;
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

fn lock(path: &Path) -> Result<SessionLock, SessionError> {
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
    Ok(SessionLock(file))
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
    #[error("user input must contain text or valid images")]
    InvalidUserInput,
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
    fn raw_input_limit_can_be_persisted_after_json_encoding() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let mut prompt = "line\n".repeat(8 * 1024 * 1024 / 5);
        prompt.push_str("abc");
        assert_eq!(prompt.len(), 8 * 1024 * 1024);
        session
            .begin_turn(
                prompt.clone(),
                ModelRef {
                    provider: "test".into(),
                    model: "test".into(),
                },
            )
            .unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(matches!(
            &reopened.view().unwrap().entries[0],
            SessionEntry::TurnStarted { input, .. }
                if input.content == vec![Content::Text(prompt)]
        ));
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_input_survives_reopen_and_invalid_content_never_commits() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let image: ion_ai::ImageContent = serde_json::from_value(serde_json::json!({
            "mime_type":"image/png",
            "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        }))
        .unwrap();
        let input = Message {
            role: Role::User,
            content: vec![
                Content::Text("inspect".into()),
                Content::Image(image.clone()),
            ],
            provider_replay: None,
        };
        let model = ModelRef {
            provider: "test".into(),
            model: "vision".into(),
        };
        let (turn, _) = session
            .begin_turn_message(input.clone(), model.clone())
            .unwrap();
        session.end_turn(turn, TurnEndReason::Cancelled).unwrap();
        let invalid = Message {
            content: vec![Content::Image(
                serde_json::from_value(serde_json::json!({
                    "mime_type":"image/png", "data":"broken"
                }))
                .unwrap(),
            )],
            ..input.clone()
        };
        let before = session.entry_count().unwrap();
        assert!(matches!(
            session.begin_turn_message(invalid, model),
            Err(SessionError::InvalidUserInput)
        ));
        assert_eq!(session.entry_count().unwrap(), before);
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.context_messages().unwrap()[0], input);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
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
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                },
                Usage::unknown(),
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
    fn releasing_a_session_unlocks_an_inherited_file_description() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        assert!(matches!(
            Session::open(&path),
            Err(SessionError::AlreadyOpen)
        ));
        let inherited = session._lock.0.try_clone().unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        drop(reopened);
        drop(inherited);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_results_preserve_assistant_call_order() {
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
                    content: ["z-call", "a-call"]
                        .into_iter()
                        .map(|id| {
                            Content::ToolCall(ToolCall {
                                id: id.into(),
                                name: "read".into(),
                                arguments: serde_json::json!({"path":"missing"}),
                                raw_arguments: None,
                            })
                        })
                        .collect(),
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.begin_turn("second".into(), model).unwrap().1, 2);
        let ids = reopened
            .view()
            .unwrap()
            .entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::ToolResult { result, .. } => Some(result.call_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, ["z-call", "a-call"]);
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
                    Usage::known(123, 45),
                    false,
                )
                .unwrap()
        );
        let view = session.view().unwrap();
        assert_eq!(view.last_end, Some((turn, TurnEndReason::Completed)));
        assert_eq!(view.last_usage, Some(Usage::known(123, 45)));
        assert_eq!(view.entries.len(), 3);
        drop(session);
        assert_eq!(
            Session::inspect(&path).unwrap().last_usage,
            Some(Usage::known(123, 45))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejected_batch_does_not_publish_projection_or_settled_cut() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let (turn, _) = session
            .begin_turn(
                "first".into(),
                ModelRef {
                    provider: "test".into(),
                    model: "test".into(),
                },
            )
            .unwrap();
        let answer = Message {
            role: Role::Assistant,
            content: vec![Content::Text("done".into())],
            provider_replay: None,
        };
        {
            let mut store = session.store.lock().unwrap();
            let before = store.settled.clone();
            assert!(matches!(
                append(
                    &mut store,
                    &[
                        SessionEntry::Assistant {
                            turn,
                            message: answer.clone(),
                            usage: Usage::unknown(),
                            termination: ResponseTermination::Completed,
                        },
                        SessionEntry::Steering {
                            turn,
                            prompt: String::new(),
                        },
                    ],
                ),
                Err(SessionError::InvalidHistory)
            ));
            assert_eq!(store.settled, before);
            assert_eq!(store.messages.len(), 1);
        }
        session
            .record_assistant(turn, answer, Usage::unknown(), false)
            .unwrap();
        assert_eq!(session.view().unwrap().entries.len(), 3);
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

    #[test]
    fn compaction_changes_only_the_model_projection_and_survives_reopen() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let (first, _) = session
            .begin_turn("first task".into(), model.clone())
            .unwrap();
        session
            .record_assistant(
                first,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("first result".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let plan = session.compaction_plan(0, usize::MAX).unwrap().unwrap();
        assert_eq!(plan.through_entry, 3);
        assert_eq!(plan.messages.len(), 2);
        assert!(
            session
                .record_compaction(1, "invalid".into(), Usage::unknown())
                .is_err()
        );
        session
            .record_compaction(
                plan.through_entry,
                "first task done".into(),
                Usage::unknown(),
            )
            .unwrap();
        let (second, _) = session.begin_turn("next task".into(), model).unwrap();
        session
            .record_assistant(
                second,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("next result".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        assert_eq!(session.messages().unwrap().len(), 4);
        assert_eq!(session.context_messages().unwrap().len(), 3);
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.context_messages().unwrap().len(), 3);
        assert_eq!(reopened.view().unwrap().compacted_through, Some(3));
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compaction_never_cuts_between_parallel_tool_results() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let (turn, _) = session.begin_turn("inspect".into(), model).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: ["one", "two"]
                        .into_iter()
                        .map(|id| {
                            Content::ToolCall(ToolCall {
                                id: id.into(),
                                name: "read".into(),
                                arguments: serde_json::json!({"path": id}),
                                raw_arguments: None,
                            })
                        })
                        .collect(),
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id: "one".into(),
                    name: "read".into(),
                    result: serde_json::json!({"content":"a"}),
                    is_error: false,
                },
            )
            .unwrap();
        assert!(session.compaction_plan(0, usize::MAX).unwrap().is_none());
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id: "two".into(),
                    name: "read".into(),
                    result: serde_json::json!({"content":"b"}),
                    is_error: false,
                },
            )
            .unwrap();
        assert_eq!(
            session
                .compaction_plan(0, usize::MAX)
                .unwrap()
                .unwrap()
                .through_entry,
            4
        );
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_text_response_is_a_compaction_boundary_during_steering() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let (turn, _) = session.begin_turn("first request".into(), model).unwrap();
        assert!(
            !session
                .record_assistant(
                    turn,
                    Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("first response".into())],
                        provider_replay: None,
                    },
                    Usage::unknown(),
                    true,
                )
                .unwrap()
        );
        session
            .record_steerings(turn, vec!["follow-up steering".into()])
            .unwrap();
        assert_eq!(
            session
                .compaction_plan(0, usize::MAX)
                .unwrap()
                .unwrap()
                .through_entry,
            2
        );
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(
            reopened
                .compaction_plan(0, usize::MAX)
                .unwrap()
                .unwrap()
                .through_entry,
            2
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_tool_result_survives_reopen() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let (turn, _) = session
            .begin_turn("read missing file".into(), model)
            .unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "call".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path":"missing"}),
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id: "call".into(),
                    name: "read".into(),
                    result: serde_json::json!({"error":"file missing"}),
                    is_error: true,
                },
            )
            .unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(matches!(
            &reopened.messages().unwrap()[2].content[0],
            Content::ToolResult(ToolResult { is_error: true, .. })
        ));
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cloned_session_preserves_context_but_diverges_independently() {
        let (root, path) = fixture();
        let clone_path = root.join("clone.sqlite");
        let source = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        source.set_name(Some("Source")).unwrap();
        let (first, _) = source.begin_turn("first".into(), model.clone()).unwrap();
        source
            .record_assistant(
                first,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: None,
                },
                Usage::known(10, 2),
                false,
            )
            .unwrap();
        source
            .record_compaction(3, "first is done".into(), Usage::unknown())
            .unwrap();
        let (second, _) = source.begin_turn("second".into(), model.clone()).unwrap();
        let original = source.view().unwrap();
        let cloned = source.clone_to(&clone_path).unwrap();
        assert_eq!(cloned.view().unwrap().entries, original.entries);
        assert_eq!(
            cloned.context_messages().unwrap(),
            source.context_messages().unwrap()
        );
        assert_eq!(cloned.view().unwrap().unfinished_turn, Some(second));
        assert_eq!(cloned.view().unwrap().name, None);
        let (third, interrupted) = cloned.begin_turn("alternate".into(), model).unwrap();
        assert_eq!((third, interrupted), (second + 1, 0));
        assert_eq!(source.view().unwrap().entries, original.entries);
        drop(cloned);
        drop(source);
        assert_eq!(
            Session::inspect(&clone_path).unwrap().last_end,
            Some((second, TurnEndReason::Interrupted))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
