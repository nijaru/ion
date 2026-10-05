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

use crate::tool_set::ToolActivity;
use ion_ai::{
    Content, IncompleteReason, Message, ModelContextChange, ModelContextState,
    ModelContextTimeline, ModelExecution, ModelRef, ResponseTermination, Role, ToolCall,
    ToolResult, ToolSpec, Usage,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use tokio_util::sync::CancellationToken;

const FORMAT_VERSION: u32 = 7;
// An 8 MiB raw prompt or streamed response can grow up to sixfold when JSON
// escapes control characters. Keep the storage bound above that encoded size.
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

fn completed_termination() -> ResponseTermination {
    ResponseTermination::Completed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    version: u32,
    provider_session_id: uuid::Uuid,
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
    /// Effective physical model selected for the next coding request. This is
    /// distinct from the logical Session selection and advances provider replay
    /// epochs when the physical model changes.
    EffectiveModelChanged {
        turn: u64,
        model: ModelRef,
    },
    /// The selected provider could not safely reuse older opaque continuation
    /// after a changed request context. Raw assistant facts remain intact.
    ProviderReplayRebased {
        turn: u64,
    },
    /// Provider-neutral, model-visible request context at this point in the
    /// Session. Execution routes remain request-bound host state and are never
    /// persisted here.
    ModelContextChanged {
        turn: u64,
        context: ModelContextSnapshot,
    },
    UserShell {
        command: String,
        output: serde_json::Value,
        is_error: bool,
        exclude_from_context: bool,
    },
    Steering {
        turn: u64,
        input: Message,
    },
    /// Provider usage from a best-effort prompt-cache refresh. This is an
    /// accounting fact only and never enters model context.
    CacheWarm {
        turn: u64,
        execution: ModelExecution,
        #[serde(default = "Usage::unknown")]
        usage: Usage,
    },
    Compacted {
        through_entry: u64,
        summary: String,
        execution: ModelExecution,
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
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelContextSnapshot {
    pub instructions: String,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToolActivity {
    pub call_id: String,
    pub activity: ToolActivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Interrupted,
    Failed(String),
}

#[derive(Debug, Clone, Copy)]
pub enum ForkPoint {
    BeforeTurn(u64),
    AfterTurn(u64),
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
    pub last_effective_model: Option<ModelRef>,
    pub last_context: Option<ModelContextSnapshot>,
    pub compacted_through: Option<u64>,
    pub last_execution: Option<ModelExecution>,
    pub last_usage: Option<Usage>,
}

#[derive(Debug, Clone)]
pub struct TurnSummary {
    pub turn: u64,
    pub input: Message,
    pub model: ModelRef,
    pub end: Option<TurnEndReason>,
}

impl SessionView {
    /// Conversation display, including user commands kept out of model context.
    pub fn display_messages(&self) -> Vec<Message> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::UserShell {
                    command,
                    output,
                    exclude_from_context,
                    ..
                } => Some(shell_message(command, output, *exclude_from_context)),
                other => message_from_entry(other),
            })
            .collect()
    }

    pub fn turns(&self) -> Vec<TurnSummary> {
        let mut turns: Vec<TurnSummary> = Vec::new();
        for entry in &self.entries {
            match entry {
                SessionEntry::TurnStarted { turn, input, model } => turns.push(TurnSummary {
                    turn: *turn,
                    input: input.clone(),
                    model: model.clone(),
                    end: None,
                }),
                SessionEntry::TurnEnded { turn, reason } => {
                    if let Some(last) = turns.last_mut()
                        && last.turn == *turn
                    {
                        last.end = Some(reason.clone());
                    }
                }
                _ => {}
            }
        }
        turns
    }
}

pub(crate) struct CompactionPlan {
    pub through_entry: u64,
    pub messages: Vec<Message>,
    pub chunked: bool,
}

#[derive(Default, Clone)]
struct State {
    active: Option<u64>,
    assistant_seen_in_turn: bool,
    pending: Vec<(String, String)>,
    last_id: u64,
    last_end: Option<(u64, TurnEndReason)>,
    last_model: Option<ModelRef>,
    last_effective_model: Option<ModelRef>,
    last_context: Option<ModelContextSnapshot>,
    // Derived from effective-model transitions/rebases; older opaque replay stays
    // in raw history and is never revived by switching a route back.
    replay_epoch_start: u64,
    sequence: u64,
    compaction: Option<(u64, String)>,
    last_execution: Option<ModelExecution>,
    last_usage: Option<Usage>,
}

impl State {
    fn select_model(&mut self, model: &ModelRef) {
        self.last_model = Some(model.clone());
    }

    fn select_effective_model(&mut self, model: &ModelRef) {
        if self
            .last_effective_model
            .as_ref()
            .is_some_and(|prior| prior != model)
        {
            self.replay_epoch_start = self.sequence + 1;
        }
        self.last_effective_model = Some(model.clone());
    }

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
                self.select_model(model);
            }
            SessionEntry::EffectiveModelChanged { turn, model } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || self.last_effective_model.as_ref() == Some(model)
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.select_effective_model(model);
            }
            SessionEntry::ProviderReplayRebased { turn } => {
                if self.active != Some(*turn)
                    || self.assistant_seen_in_turn
                    || !self.pending.is_empty()
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.replay_epoch_start = self.sequence + 1;
            }
            SessionEntry::ModelContextChanged { turn, context } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || self.last_context.as_ref() == Some(context)
                    || !valid_model_context(context)
                {
                    return Err(SessionError::InvalidHistory);
                }
                self.last_context = Some(context.clone());
            }
            SessionEntry::UserShell {
                command,
                output,
                exclude_from_context,
                ..
            } => {
                if self.active.is_some() || command.trim().is_empty() || command.contains('\0') {
                    return Err(SessionError::InvalidHistory);
                }
                if !exclude_from_context {
                    messages.push(shell_message(command, output, false));
                }
                new_settled.push(self.sequence + 1);
            }
            SessionEntry::CacheWarm {
                turn, execution, ..
            } => {
                if self.active != Some(*turn)
                    || self.last_model.as_ref() != Some(&execution.route.logical)
                    || self.last_effective_model.as_ref() != Some(&execution.route.effective)
                {
                    return Err(SessionError::InvalidHistory);
                }
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
                self.assistant_seen_in_turn = false;
                self.last_id = *turn;
                self.select_model(model);
                messages.push(input.clone());
            }
            SessionEntry::Steering { turn, input } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || !valid_user_message(input)
                {
                    return Err(SessionError::InvalidHistory);
                }
                messages.push(input.clone());
            }
            SessionEntry::Assistant {
                turn,
                message,
                tool_activities,
                execution,
                usage,
                termination,
            } => {
                if self.active != Some(*turn)
                    || !self.pending.is_empty()
                    || message.role != Role::Assistant
                    || self.last_model.as_ref() != Some(&execution.route.logical)
                    || self
                        .last_effective_model
                        .as_ref()
                        .is_some_and(|model| model != &execution.route.effective)
                {
                    return Err(SessionError::InvalidHistory);
                }
                if self.last_effective_model.is_none() {
                    self.last_effective_model = Some(execution.route.effective.clone());
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
                let mut activity_index = 0usize;
                for part in &message.content {
                    match part {
                        Content::ToolCall(ToolCall { id, name, .. })
                            if !id.is_empty() && !name.is_empty() =>
                        {
                            if self.pending.iter().any(|(pending_id, _)| pending_id == id) {
                                return Err(SessionError::InvalidHistory);
                            }
                            let Some(stored) = tool_activities.get(activity_index) else {
                                return Err(SessionError::InvalidHistory);
                            };
                            if stored.call_id != *id {
                                return Err(SessionError::InvalidHistory);
                            }
                            activity_index += 1;
                            self.pending.push((id.clone(), name.clone()));
                        }
                        Content::Text(_) => {}
                        _ => return Err(SessionError::InvalidHistory),
                    }
                }
                if activity_index != tool_activities.len() {
                    return Err(SessionError::InvalidHistory);
                }
                messages.push(message.clone());
                self.assistant_seen_in_turn = true;
                self.last_execution = Some(execution.clone());
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
                self.assistant_seen_in_turn = false;
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

pub struct UserShellPermit<'a> {
    session: &'a Session,
    _gate: AsyncMutexGuard<'a, ()>,
}

impl UserShellPermit<'_> {
    pub fn record(
        self,
        command: String,
        output: serde_json::Value,
        is_error: bool,
        exclude_from_context: bool,
    ) -> Result<(), SessionError> {
        self.session
            .record_user_shell(command, output, is_error, exclude_from_context)
    }
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
        initialize_new(&connection)?;
        let header = Header {
            version: FORMAT_VERSION,
            provider_session_id: uuid::Uuid::now_v7(),
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
        configure_connection(&connection)?;
        let header = read_header(&connection)?;
        let (state, settled, messages) = project(&read_entries(&connection)?)?;
        prepare_writer(&connection)?;
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
        let source = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let entries = read_entries(&source.connection)?;
        drop(source);
        self.copy_entries_to(path.as_ref(), &entries)
    }

    /// Copy the valid prefix at a selected Turn boundary into an independent
    /// Session. The source and target still share the live working directory.
    pub fn fork_to(&self, path: impl AsRef<Path>, point: ForkPoint) -> Result<Self, SessionError> {
        let source = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let mut entries = read_entries(&source.connection)?;
        let boundary = entries
            .iter()
            .position(|entry| match (point, entry) {
                (ForkPoint::BeforeTurn(selected), SessionEntry::TurnStarted { turn, .. }) => {
                    selected == *turn
                }
                (ForkPoint::AfterTurn(selected), SessionEntry::TurnEnded { turn, .. }) => {
                    selected == *turn
                }
                _ => false,
            })
            .ok_or(SessionError::InvalidForkPoint)?;
        let selected_model = match (&point, &entries[boundary]) {
            (ForkPoint::BeforeTurn(_), SessionEntry::TurnStarted { model, .. }) => {
                Some(model.clone())
            }
            _ => None,
        };
        entries.truncate(boundary + usize::from(matches!(point, ForkPoint::AfterTurn(_))));
        if let Some(model) = selected_model {
            entries.push(SessionEntry::ModelSelected { model });
        }
        drop(source);
        self.copy_entries_to(path.as_ref(), &entries)
    }

    fn copy_entries_to(&self, path: &Path, entries: &[SessionEntry]) -> Result<Self, SessionError> {
        let clone = Self::create(path, &self.header.cwd)?;
        let copied = {
            let mut target = clone.store.lock().map_err(|_| SessionError::Poisoned)?;
            append(&mut target, entries)
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
            last_effective_model: state.last_effective_model,
            last_context: state.last_context,
            compacted_through: state.compaction.map(|(through, _)| through),
            last_execution: state.last_execution,
            last_usage: state.last_usage,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn cwd(&self) -> &Path {
        &self.header.cwd
    }
    /// Stable transport affinity identity. Conversation copies get a fresh ID
    /// through `create`, while reopening reads the existing immutable header.
    pub fn provider_session_id(&self) -> uuid::Uuid {
        self.header.provider_session_id
    }
    pub fn messages(&self) -> Result<Vec<Message>, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .messages
            .clone())
    }

    /// Record a user-run command after its observed result is available.
    /// It is never appended while a model Turn owns the Session.
    fn record_user_shell(
        &self,
        command: String,
        output: serde_json::Value,
        is_error: bool,
        exclude_from_context: bool,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::UserShell {
                command,
                output,
                is_error,
                exclude_from_context,
            }],
        )
    }

    /// Acquire exclusive authority for one direct user shell effect. The host
    /// executes the effect, then publishes its observed result through the
    /// returned permit before exclusivity is released.
    pub async fn begin_user_shell(
        &self,
        stop: CancellationToken,
    ) -> Result<UserShellPermit<'_>, SessionError> {
        let gate = tokio::select! {
            gate = self.submit_gate.lock() => gate,
            () = stop.cancelled() => return Err(SessionError::UserShellCancelled),
        };
        if stop.is_cancelled() {
            return Err(SessionError::UserShellCancelled);
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let recovery = interrupted_turn_entries(&store.state);
        if !recovery.is_empty() {
            // Recovery must commit before the host gets permission to run.
            append(&mut store, &recovery)?;
        }
        Ok(UserShellPermit {
            session: self,
            _gate: gate,
        })
    }

    pub fn entry_count(&self) -> Result<u64, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .sequence)
    }

    /// Provider-neutral context projection when no effective target is supplied.
    /// Opaque provider replay is retained only for the direct-model case where
    /// the logical selection and last effective physical model are identical.
    /// Routed callers must use `context_messages_for` with the resolved
    /// effective model before dispatch.
    pub fn context_messages(&self) -> Result<Vec<Message>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let direct = match (
            store.state.last_model.as_ref(),
            store.state.last_effective_model.as_ref(),
        ) {
            (Some(logical), Some(effective)) if logical == effective => Some(effective),
            _ => None,
        };
        context_projection(&store, direct)
    }

    pub fn model_context(&self) -> Result<Option<ModelContextSnapshot>, SessionError> {
        Ok(self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .last_context
            .clone())
    }

    /// Return a full provider-neutral context timeline only while the raw
    /// message projection is still byte-for-byte comparable to Session history.
    /// Compaction or a replay epoch change deliberately falls back to the
    /// latest leading context instead of guessing at provider prefix semantics.
    pub(crate) fn context_timeline_for(
        &self,
        model: &ModelRef,
        current: &ModelContextSnapshot,
    ) -> Result<Option<ModelContextTimeline>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store.state.compaction.is_some()
            || store.state.replay_epoch_start != 0
            || store.state.last_effective_model.as_ref() != Some(model)
        {
            return Ok(None);
        }

        let mut message_count = 0usize;
        let mut initial = None;
        let mut changes = Vec::new();
        let mut last = None;
        for entry in read_entries(&store.connection)? {
            match entry {
                SessionEntry::ModelContextChanged { context, .. } => {
                    let state = model_context_state(&context);
                    if initial.is_none() {
                        initial = Some(state.clone());
                    } else if last.as_ref() != Some(&state) {
                        changes.push(ModelContextChange {
                            after_message: message_count,
                            context: state.clone(),
                        });
                    }
                    last = Some(state);
                }
                other => {
                    if message_from_entry(&other).is_some() {
                        message_count = message_count.saturating_add(1);
                    }
                }
            }
        }

        let current = model_context_state(current);
        let initial = initial.unwrap_or_else(|| current.clone());
        if last.as_ref().is_some_and(|last| last != &current) {
            changes.push(ModelContextChange {
                after_message: message_count,
                context: current.clone(),
            });
        }
        Ok(Some(ModelContextTimeline { initial, changes }))
    }

    /// Project a request for one model without reviving opaque replay from a
    /// previous model epoch. The raw Session keeps every original message.
    pub(crate) fn context_messages_for(
        &self,
        model: &ModelRef,
    ) -> Result<Vec<Message>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        context_projection(&store, Some(model))
    }

    /// Record the effective physical model selected for the next coding request.
    /// Repeated direct requests on the same physical model are elided.
    pub(crate) fn record_effective_model(
        &self,
        turn: u64,
        model: ModelRef,
    ) -> Result<bool, SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        match store.state.last_effective_model.as_ref() {
            None => Ok(false),
            Some(previous) if previous == &model => Ok(false),
            Some(_) => {
                append(
                    &mut store,
                    &[SessionEntry::EffectiveModelChanged { turn, model }],
                )?;
                Ok(true)
            }
        }
    }

    /// Permanently omit prior opaque replay from future model requests after
    /// an adapter detects a changed signed context before dispatch.
    pub(crate) fn rebase_provider_replay(&self, turn: u64) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &[SessionEntry::ProviderReplayRebased { turn }])
    }

    /// Record a model-visible context transition immediately before a request.
    /// Identical consecutive snapshots are elided. The snapshot contains only
    /// provider-neutral prompt/tool declarations; concrete execution routes stay
    /// frozen in the in-memory request-bound ToolCatalog.
    pub(crate) fn record_model_context(
        &self,
        turn: u64,
        context: ModelContextSnapshot,
    ) -> Result<bool, SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store.state.last_context.as_ref() == Some(&context) {
            return Ok(false);
        }
        append(
            &mut store,
            &[SessionEntry::ModelContextChanged { turn, context }],
        )?;
        Ok(true)
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
        // A large final tool result can make the empty suffix after it look
        // like the only fitting tail. Prefer retaining that call/result batch
        // exactly when older settled history is available to summarize.
        let through = if keep_bytes > 0 {
            let recent_entry = entries.iter().rposition(|entry| match entry {
                SessionEntry::UserShell {
                    exclude_from_context,
                    ..
                } => !exclude_from_context,
                SessionEntry::TurnStarted { .. }
                | SessionEntry::Steering { .. }
                | SessionEntry::Assistant { .. }
                | SessionEntry::ToolResult { .. } => true,
                SessionEntry::ModelSelected { .. }
                | SessionEntry::EffectiveModelChanged { .. }
                | SessionEntry::ProviderReplayRebased { .. }
                | SessionEntry::ModelContextChanged { .. }
                | SessionEntry::CacheWarm { .. }
                | SessionEntry::Compacted { .. }
                | SessionEntry::TurnEnded { .. } => false,
            });
            let earlier_cut = recent_entry
                .filter(|&index| matches!(entries[index], SessionEntry::ToolResult { .. }))
                .and_then(|index| {
                    entries[..index].iter().rposition(|entry| {
                        matches!(entry, SessionEntry::Assistant { message, .. }
                            if message.content.iter().any(|part| matches!(part, Content::ToolCall(_))))
                    })
                })
                .and_then(|index| {
                    store
                        .settled
                        .range((previous + 1)..=index as u64)
                        .next_back()
                        .copied()
                        .filter(|&cut| through.is_some_and(|current| current > cut))
                });
            earlier_cut.or(through)
        } else {
            through
        };
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

    pub(crate) fn record_cache_warm(
        &self,
        turn: u64,
        execution: ModelExecution,
        usage: Usage,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::CacheWarm {
                turn,
                execution,
                usage,
            }],
        )
    }

    pub(crate) fn record_compaction(
        &self,
        through_entry: u64,
        summary: String,
        execution: ModelExecution,
        usage: Usage,
    ) -> Result<(), SessionError> {
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(
            &mut store,
            &[SessionEntry::Compacted {
                through_entry,
                summary,
                execution,
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
            last_effective_model: store.state.last_effective_model.clone(),
            last_context: store.state.last_context.clone(),
            compacted_through: store.state.compaction.as_ref().map(|(through, _)| *through),
            last_execution: store.state.last_execution.clone(),
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
        let mut entries = interrupted_turn_entries(&store.state);
        let interrupted = store.state.pending.len();
        let turn = store
            .state
            .last_id
            .checked_add(1)
            .ok_or(SessionError::TurnIdExhausted)?;
        entries.push(SessionEntry::TurnStarted { turn, input, model });
        append(&mut store, &entries)?;
        Ok((turn, interrupted))
    }

    #[cfg(test)]
    pub(crate) fn record_assistant(
        &self,
        turn: u64,
        message: Message,
        usage: Usage,
        continue_turn: bool,
    ) -> Result<bool, SessionError> {
        let activities = default_tool_activities(&message);
        let logical = self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .last_model
            .clone()
            .ok_or(SessionError::InvalidHistory)?;
        let effective = logical.clone();
        self.record_effective_model(turn, effective.clone())?;
        self.record_assistant_with_activities(
            turn,
            message,
            activities,
            ModelExecution {
                route: ion_ai::ModelRoute {
                    logical,
                    effective,
                    reason: ion_ai::ModelRouteReason::UserRequest,
                },
                returned_model: None,
            },
            usage,
            continue_turn,
        )
    }

    pub(crate) fn record_assistant_with_activities(
        &self,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
        continue_turn: bool,
    ) -> Result<bool, SessionError> {
        self.record_assistant_entries(
            turn,
            message,
            tool_activities,
            execution,
            usage,
            continue_turn,
            Vec::new(),
        )
    }

    /// Publish the completed assistant and queued steering in one batch; a
    /// failed transaction must leave both unpublished for the host to recover.
    pub(crate) fn record_assistant_with_steering(
        &self,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
        steering: Vec<Message>,
    ) -> Result<bool, SessionError> {
        let continue_turn = !steering.is_empty();
        self.record_assistant_entries(
            turn,
            message,
            tool_activities,
            execution,
            usage,
            continue_turn,
            steering,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one atomic assistant/steering commit carries its complete durable facts"
    )]
    fn record_assistant_entries(
        &self,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
        continue_turn: bool,
        steering: Vec<Message>,
    ) -> Result<bool, SessionError> {
        let has_calls = message
            .content
            .iter()
            .any(|part| matches!(part, Content::ToolCall(_)));
        validate_tool_activities(&message, &tool_activities)?;
        let mut entries = vec![SessionEntry::Assistant {
            turn,
            message,
            tool_activities,
            execution,
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
                .map(|input| SessionEntry::Steering { turn, input }),
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
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
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
                    images: Vec::new(),
                    is_error: true,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        if results.is_empty() {
            return Err(SessionError::InvalidHistory);
        }
        validate_tool_activities(&message, &tool_activities)?;
        let mut entries = vec![SessionEntry::Assistant {
            turn,
            message,
            tool_activities,
            execution,
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
        inputs: Vec<Message>,
    ) -> Result<(), SessionError> {
        if inputs.is_empty() {
            return Ok(());
        }
        let entries = inputs
            .into_iter()
            .map(|input| SessionEntry::Steering { turn, input })
            .collect::<Vec<_>>();
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &entries)
    }

    #[cfg(test)]
    pub(crate) fn record_tool_result(
        &self,
        turn: u64,
        result: ToolResult,
    ) -> Result<(), SessionError> {
        self.record_tool_result_with_context(turn, result, None)
    }

    /// Publish one observed tool result and, when this closes the assistant's
    /// pending calls, the next model-visible context in the same transaction.
    pub(crate) fn record_tool_result_with_context(
        &self,
        turn: u64,
        result: ToolResult,
        context: Option<ModelContextSnapshot>,
    ) -> Result<(), SessionError> {
        let mut entries = vec![SessionEntry::ToolResult { turn, result }];
        if let Some(context) = context {
            entries.push(SessionEntry::ModelContextChanged { turn, context });
        }
        let mut store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        append(&mut store, &entries)
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

fn interrupted_turn_entries(state: &State) -> Vec<SessionEntry> {
    let Some(turn) = state.active else {
        return Vec::new();
    };
    let mut entries = unknown_results(turn, &state.pending);
    entries.push(SessionEntry::TurnEnded {
        turn,
        reason: TurnEndReason::Interrupted,
    });
    entries
}

fn unknown_results(turn: u64, pending: &[(String, String)]) -> Vec<SessionEntry> {
    pending.iter().map(|(call_id, name)| SessionEntry::ToolResult { turn, result: ToolResult {
        call_id: call_id.clone(), name: name.clone(),
        result: serde_json::json!({"error":"The tool result was not committed. Its external effect is unknown; inspect the working directory before retrying."}),
        images: Vec::new(),
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

fn context_projection(
    store: &Store,
    model: Option<&ModelRef>,
) -> Result<Vec<Message>, SessionError> {
    let through = store
        .state
        .compaction
        .as_ref()
        .map_or(0, |(through, _)| *through);
    if store.state.replay_epoch_start <= through
        && model == store.state.last_effective_model.as_ref()
    {
        return match &store.state.compaction {
            Some((through, summary)) => {
                let entries = read_entries_after(&store.connection, *through)?;
                let mut messages = vec![summary_message(summary)];
                messages.extend(messages_from_entries(&entries));
                Ok(messages)
            }
            None => Ok(store.messages.clone()),
        };
    }
    let mut messages = store
        .state
        .compaction
        .as_ref()
        .map(|(_, summary)| vec![summary_message(summary)])
        .unwrap_or_default();
    let clear_all = model != store.state.last_effective_model.as_ref();
    for (index, entry) in read_entries_after(&store.connection, through)?
        .iter()
        .enumerate()
    {
        if let Some(mut message) = message_from_entry(entry) {
            let sequence = through
                .checked_add(
                    u64::try_from(index)
                        .map_err(|_| SessionError::InvalidHistory)?
                        .checked_add(1)
                        .ok_or(SessionError::InvalidHistory)?,
                )
                .ok_or(SessionError::InvalidHistory)?;
            if clear_all || sequence < store.state.replay_epoch_start {
                message.provider_replay = None;
            }
            messages.push(message);
        }
    }
    Ok(messages)
}

#[cfg(test)]
fn default_tool_activities(message: &Message) -> Vec<StoredToolActivity> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::ToolCall(call) => Some(StoredToolActivity {
                call_id: call.id.clone(),
                activity: ToolActivity::external(call.name.clone()),
            }),
            _ => None,
        })
        .collect()
}

fn validate_tool_activities(
    message: &Message,
    activities: &[StoredToolActivity],
) -> Result<(), SessionError> {
    let call_ids = message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if call_ids.len() != activities.len()
        || call_ids
            .iter()
            .zip(activities)
            .any(|(call_id, activity)| *call_id != activity.call_id)
    {
        return Err(SessionError::InvalidHistory);
    }
    Ok(())
}

fn model_context_state(context: &ModelContextSnapshot) -> ModelContextState {
    ModelContextState {
        instructions: Some(context.instructions.clone()),
        tools: context.tools.clone(),
    }
}

fn valid_model_context(context: &ModelContextSnapshot) -> bool {
    let mut names = BTreeSet::new();
    context
        .tools
        .iter()
        .all(|tool| !tool.name.trim().is_empty() && names.insert(tool.name.as_str()))
}

fn messages_from_entries(entries: &[SessionEntry]) -> Vec<Message> {
    entries.iter().filter_map(message_from_entry).collect()
}

fn message_from_entry(entry: &SessionEntry) -> Option<Message> {
    match entry {
        SessionEntry::UserShell {
            command,
            output,
            exclude_from_context: false,
            ..
        } => Some(shell_message(command, output, false)),
        SessionEntry::TurnStarted { input, .. } => Some(input.clone()),
        SessionEntry::Steering { input, .. } => Some(input.clone()),
        SessionEntry::Assistant { message, .. } => Some(message.clone()),
        SessionEntry::ToolResult { result, .. } => Some(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(result.clone())],
            provider_replay: None,
        }),
        SessionEntry::UserShell { .. }
        | SessionEntry::ModelSelected { .. }
        | SessionEntry::EffectiveModelChanged { .. }
        | SessionEntry::ProviderReplayRebased { .. }
        | SessionEntry::ModelContextChanged { .. }
        | SessionEntry::CacheWarm { .. }
        | SessionEntry::Compacted { .. }
        | SessionEntry::TurnEnded { .. } => None,
    }
}

fn shell_message(command: &str, output: &serde_json::Value, excluded: bool) -> Message {
    let mut body = format!(
        "User ran shell command{}:\n$ {command}",
        if excluded {
            " (not shared with model)"
        } else {
            ""
        }
    );
    if let Some(stdout) = output.get("stdout").and_then(serde_json::Value::as_str) {
        body.push_str("\nstdout:\n");
        body.push_str(stdout);
        if output
            .get("stdout_truncated")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            body.push_str("\n[stdout truncated]");
        }
    }
    if let Some(stderr) = output.get("stderr").and_then(serde_json::Value::as_str)
        && !stderr.is_empty()
    {
        body.push_str("\nstderr:\n");
        body.push_str(stderr);
        if output
            .get("stderr_truncated")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            body.push_str("\n[stderr truncated]");
        }
    }
    if let Some(code) = output.get("exit_code").filter(|value| !value.is_null()) {
        body.push_str(&format!("\nexit code: {code}"));
    }
    if let Some(signal) = output.get("signal").filter(|value| !value.is_null()) {
        body.push_str(&format!("\nsignal: {signal}"));
    }
    if output.get("cancelled").and_then(serde_json::Value::as_bool) == Some(true) {
        body.push_str("\ncommand cancelled");
    }
    if output.get("timed_out").and_then(serde_json::Value::as_bool) == Some(true) {
        body.push_str("\ncommand timed out");
    }
    if let Some(error) = output.get("error").and_then(serde_json::Value::as_str) {
        body.push_str("\nerror: ");
        body.push_str(error);
    }
    Message {
        role: Role::User,
        content: vec![Content::Text(body)],
        provider_replay: None,
    }
}

pub(crate) fn valid_user_message(message: &Message) -> bool {
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
    // Reject obsolete formats before interpreting the current header payload.
    // In particular, never invent an affinity identity when opening old data.
    #[derive(Deserialize)]
    struct Format {
        version: u32,
    }
    let format: Format = serde_json::from_slice(&encoded)?;
    if format.version != FORMAT_VERSION {
        return Err(SessionError::UnsupportedFormat(format.version));
    }
    Ok(serde_json::from_slice(&encoded)?)
}

fn configure_connection(connection: &Connection) -> Result<(), SessionError> {
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn prepare_writer(connection: &Connection) -> Result<(), SessionError> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

fn initialize_new(connection: &Connection) -> Result<(), SessionError> {
    configure_connection(connection)?;
    prepare_writer(connection)?;
    connection.execute_batch("CREATE TABLE session (id INTEGER PRIMARY KEY CHECK(id=1), header BLOB NOT NULL); CREATE TABLE entries (seq INTEGER PRIMARY KEY, body BLOB NOT NULL);")?;
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
    #[error("user shell command cancelled before start")]
    UserShellCancelled,
    #[error("user input must contain text or valid images")]
    InvalidUserInput,
    #[error("session name must be at most 120 bytes without control characters")]
    InvalidName,
    #[error("selected Turn boundary does not exist or has not ended")]
    InvalidForkPoint,
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
    use ion_ai::{ProviderReplay, ToolCall};

    fn test_execution() -> ModelExecution {
        ModelExecution {
            route: ion_ai::ModelRoute::direct(
                ModelRef {
                    provider: "test".into(),
                    model: "test".into(),
                },
                ion_ai::ModelRouteReason::Auxiliary,
            ),
            returned_model: None,
        }
    }

    #[tokio::test]
    async fn user_shell_permit_waits_for_turn_gate_and_commits_before_release() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        {
            let guard = session.submit_gate.lock().await;
            let permit = session.begin_user_shell(CancellationToken::new());
            tokio::pin!(permit);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), permit.as_mut())
                    .await
                    .is_err()
            );
            drop(guard);
            let permit = permit.await.unwrap();
            permit
                .record(
                    "echo done".into(),
                    serde_json::json!({"stdout":"done","exit_code":0}),
                    false,
                    false,
                )
                .unwrap();
        }
        assert!(matches!(
            session.view().unwrap().entries.last(),
            Some(SessionEntry::UserShell { .. })
        ));
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn interrupted_shell_permit_recovers_before_authority_and_rejects_failed_recovery() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let (turn, _) = session
            .begin_turn("interrupted".into(), test_execution().route.logical)
            .unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "unsettled".into(),
                        name: "write".into(),
                        arguments: serde_json::json!({"path":"must-not-replay","content":"old"}),
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        drop(session);
        let session = Session::open(&path).unwrap();
        let before = session.view().unwrap();
        assert_eq!(before.unfinished_turn, Some(turn));
        session.store.lock().unwrap().connection.execute_batch(
            "CREATE TRIGGER reject_recovery BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'recovery unavailable'); END;"
        ).unwrap();
        assert!(matches!(
            session.begin_user_shell(CancellationToken::new()).await,
            Err(SessionError::Sqlite(_))
        ));
        assert_eq!(session.view().unwrap().entries, before.entries);
        session
            .store
            .lock()
            .unwrap()
            .connection
            .execute_batch("DROP TRIGGER reject_recovery;")
            .unwrap();

        let permit = session
            .begin_user_shell(CancellationToken::new())
            .await
            .unwrap();
        let recovered = session.view().unwrap();
        assert_eq!(
            recovered.unfinished_turn, None,
            "recovery must precede permission to run an effect"
        );
        assert!(matches!(
            recovered.entries.last(),
            Some(SessionEntry::TurnEnded {
                reason: TurnEndReason::Interrupted,
                ..
            })
        ));
        assert!(
            matches!(&recovered.entries[recovered.entries.len() - 2], SessionEntry::ToolResult { result, .. } if result.call_id == "unsettled" && result.is_error)
        );
        permit
            .record(
                "printf new".into(),
                serde_json::json!({"stdout":"new","exit_code":0}),
                false,
                false,
            )
            .unwrap();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(
            matches!(reopened.view().unwrap().entries.last(), Some(SessionEntry::UserShell { output, .. }) if output["stdout"] == "new")
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opening_invalid_database_does_not_initialize_session_schema() {
        let (root, path) = fixture();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE unrelated (value INTEGER);")
            .unwrap();
        drop(connection);

        assert!(Session::open(&path).is_err());

        let connection =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let table_exists = |name: &str| {
            connection
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                    params![name],
                    |_| Ok(()),
                )
                .optional()
                .unwrap()
                .is_some()
        };
        assert!(table_exists("unrelated"));
        assert!(!table_exists("session"));
        assert!(!table_exists("entries"));
        drop(connection);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reopen_rejects_old_format_or_missing_affinity_without_backfill() {
        let (root, path) = fixture();
        drop(Session::create(&path, &root).unwrap());
        let connection = Connection::open(&path).unwrap();
        for version in [6, FORMAT_VERSION] {
            let encoded = serde_json::to_vec(&serde_json::json!({
                "version": version,
                "cwd": root,
                "name": null,
            }))
            .unwrap();
            connection
                .execute(
                    "UPDATE session SET header = ?1 WHERE id=1",
                    params![encoded],
                )
                .unwrap();
            for result in [
                Session::open(&path).map(|_| ()),
                Session::inspect(&path).map(|_| ()),
            ] {
                if version == 6 {
                    assert!(matches!(result, Err(SessionError::UnsupportedFormat(6))));
                } else {
                    assert!(result.is_err());
                }
            }
            let stored: Vec<u8> = connection
                .query_row("SELECT header FROM session WHERE id=1", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(stored, encoded);
        }
        drop(connection);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn user_shell_context_choice_survives_reopen_and_compaction() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        session
            .record_user_shell(
                "pwd".into(),
                serde_json::json!({"stdout":"visible"}),
                false,
                false,
            )
            .unwrap();
        session
            .record_user_shell(
                "secret".into(),
                serde_json::json!({"stdout":"private"}),
                false,
                true,
            )
            .unwrap();
        let view = session.view().unwrap();
        assert_eq!(view.display_messages().len(), 2);
        assert_eq!(view.messages.len(), 1);
        assert!(
            serde_json::to_string(&session.context_messages().unwrap())
                .unwrap()
                .contains("visible")
        );
        assert!(
            !serde_json::to_string(&session.context_messages().unwrap())
                .unwrap()
                .contains("private")
        );
        assert!(matches!(
            session.record_user_shell("\0".into(), serde_json::Value::Null, true, false),
            Err(SessionError::InvalidHistory)
        ));
        assert_eq!(session.view().unwrap().entries.len(), 2);
        drop(session);
        let reopened = Session::open(&path).unwrap();
        let view = reopened.view().unwrap();
        assert_eq!(view.display_messages().len(), 2);
        assert_eq!(view.messages.len(), 1);
        assert!(
            !serde_json::to_string(&reopened.context_messages().unwrap())
                .unwrap()
                .contains("private")
        );
        reopened
            .record_compaction(
                reopened.entry_count().unwrap(),
                "visible command was run".into(),
                test_execution(),
                Usage::unknown(),
            )
            .unwrap();
        assert!(
            !serde_json::to_string(&reopened.context_messages().unwrap())
                .unwrap()
                .contains("private")
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_point_forks_preserve_valid_prefix_and_source_history() {
        let (root, path) = fixture();
        let source = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "model".into(),
        };
        for prompt in ["first", "second"] {
            let (turn, _) = source.begin_turn(prompt.into(), model.clone()).unwrap();
            source
                .record_assistant(
                    turn,
                    Message {
                        role: Role::Assistant,
                        content: vec![Content::Text(format!("answer {turn}"))],
                        provider_replay: None,
                    },
                    Usage::unknown(),
                    false,
                )
                .unwrap();
            if turn == 1 {
                source
                    .record_compaction(
                        source.entry_count().unwrap(),
                        "summary".into(),
                        test_execution(),
                        Usage::unknown(),
                    )
                    .unwrap();
            }
        }
        let (active, _) = source.begin_turn("third".into(), model.clone()).unwrap();
        assert!(matches!(
            source.fork_to(root.join("invalid.sqlite"), ForkPoint::AfterTurn(active)),
            Err(SessionError::InvalidForkPoint)
        ));
        let before = source
            .fork_to(root.join("before.sqlite"), ForkPoint::BeforeTurn(2))
            .unwrap();
        let after = source
            .fork_to(root.join("after.sqlite"), ForkPoint::AfterTurn(2))
            .unwrap();
        assert_eq!(source.view().unwrap().unfinished_turn, Some(3));
        assert_eq!(before.view().unwrap().last_model, Some(model.clone()));
        assert_eq!(before.context_messages().unwrap().len(), 1); // summary only
        assert_eq!(
            after.view().unwrap().last_end.as_ref().map(|item| item.0),
            Some(2)
        );
        assert_eq!(after.context_messages().unwrap().len(), 3); // summary, second input/answer
        assert!(
            !after
                .view()
                .unwrap()
                .entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::TurnStarted { turn: 3, .. }))
        );
        let source_id = source.provider_session_id();
        let before_id = before.provider_session_id();
        let after_id = after.provider_session_id();
        assert_ne!(source_id, before_id);
        assert_ne!(source_id, after_id);
        assert_ne!(before_id, after_id);
        drop((source, before, after));
        assert_eq!(
            Session::open(&path).unwrap().provider_session_id(),
            source_id
        );
        assert_eq!(
            Session::open(root.join("before.sqlite"))
                .unwrap()
                .provider_session_id(),
            before_id
        );
        assert_eq!(
            Session::open(root.join("after.sqlite"))
                .unwrap()
                .provider_session_id(),
            after_id
        );
        fs::remove_dir_all(root).unwrap();
    }

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
    fn image_input_survives_reopen_and_malformed_image_cannot_be_constructed() {
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
        let before = session.entry_count().unwrap();
        assert!(
            serde_json::from_value::<ion_ai::ImageContent>(serde_json::json!({
                "mime_type":"image/png", "data":"broken"
            }))
            .is_err()
        );
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
                            tool_activities: Vec::new(),
                            execution: ModelExecution {
                                route: ion_ai::ModelRoute::direct(
                                    ModelRef {
                                        provider: "test".into(),
                                        model: "test".into(),
                                    },
                                    ion_ai::ModelRouteReason::UserRequest,
                                ),
                                returned_model: None,
                            },
                            usage: Usage::unknown(),
                            termination: ResponseTermination::Completed,
                        },
                        SessionEntry::Steering {
                            turn,
                            input: Message {
                                role: Role::User,
                                content: vec![Content::Text(String::new())],
                                provider_replay: None,
                            },
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
                .record_compaction(1, "invalid".into(), test_execution(), Usage::unknown())
                .is_err()
        );
        session
            .record_compaction(
                plan.through_entry,
                "first task done".into(),
                test_execution(),
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
    fn model_switch_does_not_revive_earlier_opaque_replay() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let gemini = ModelRef {
            provider: "openrouter".into(),
            model: "google/gemini".into(),
        };
        let other = ModelRef {
            provider: "openrouter".into(),
            model: "other-model".into(),
        };
        let (first, _) = session.begin_turn("first".into(), gemini.clone()).unwrap();
        session
            .record_assistant(
                first,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("first answer".into())],
                    provider_replay: Some(ProviderReplay::new(
                        "openrouter",
                        "openrouter_reasoning_details",
                        serde_json::json!([{"type":"reasoning.encrypted","data":"opaque"}]),
                    )),
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        session.select_model(other.clone()).unwrap();
        let (second, _) = session.begin_turn("second".into(), other.clone()).unwrap();
        let second_context = session.context_messages_for(&other).unwrap();
        assert!(second_context[1].provider_replay.is_none());
        assert_eq!(session.context_messages().unwrap(), second_context);
        assert_eq!(
            second_context[1].content,
            session.messages().unwrap()[1].content
        );
        session
            .record_assistant(
                second,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("second answer".into())],
                    provider_replay: Some(ProviderReplay::new(
                        "openrouter",
                        "openrouter_plain_reasoning",
                        serde_json::json!("reasoning"),
                    )),
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        session
            .record_compaction(
                3,
                "first turn done".into(),
                test_execution(),
                Usage::unknown(),
            )
            .unwrap();
        let (third, _) = session.begin_turn("third".into(), gemini.clone()).unwrap();
        let third_context = session.context_messages_for(&gemini).unwrap();
        assert!(
            third_context
                .iter()
                .all(|message| message.provider_replay.is_none())
        );
        session
            .record_assistant(
                third,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "latest".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path":"file.txt"}),
                        raw_arguments: None,
                    })],
                    provider_replay: Some(ProviderReplay::new(
                        "openrouter",
                        "openrouter_reasoning_details",
                        serde_json::json!([{"type":"reasoning.text","text":"now"}]),
                    )),
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let current = session.context_messages_for(&gemini).unwrap();
        assert!(
            current[..current.len() - 1]
                .iter()
                .all(|message| message.provider_replay.is_none())
        );
        assert!(current.last().unwrap().provider_replay.is_some());
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.context_messages_for(&gemini).unwrap(), current);
        assert_eq!(
            reopened
                .messages()
                .unwrap()
                .iter()
                .filter(|message| message.provider_replay.is_some())
                .count(),
            3
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn effective_model_route_changes_do_not_revive_old_replay_under_one_logical_model() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let logical = ModelRef {
            provider: "virtual".into(),
            model: "coding".into(),
        };
        let physical_a = ModelRef {
            provider: "anthropic".into(),
            model: "claude-a".into(),
        };
        let physical_b = ModelRef {
            provider: "openrouter".into(),
            model: "provider/model-b".into(),
        };
        let execution = |effective: ModelRef, returned_model: &str| ModelExecution {
            route: ion_ai::ModelRoute {
                logical: logical.clone(),
                effective,
                reason: ion_ai::ModelRouteReason::UserRequest,
            },
            returned_model: Some(returned_model.into()),
        };

        let (first, _) = session.begin_turn("first".into(), logical.clone()).unwrap();
        assert!(
            !session
                .record_effective_model(first, physical_a.clone())
                .unwrap()
        );
        session
            .record_assistant_with_activities(
                first,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("first answer".into())],
                    provider_replay: Some(ProviderReplay::new(
                        "anthropic",
                        "anthropic_content_blocks",
                        serde_json::json!({"opaque":"a"}),
                    )),
                },
                Vec::new(),
                execution(physical_a.clone(), "claude-a-20261004"),
                Usage::known(10, 2),
                false,
            )
            .unwrap();

        let (second, _) = session
            .begin_turn("second".into(), logical.clone())
            .unwrap();
        assert!(
            session
                .record_effective_model(second, physical_b.clone())
                .unwrap()
        );
        assert!(
            session
                .context_messages_for(&physical_b)
                .unwrap()
                .iter()
                .all(|message| message.provider_replay.is_none())
        );
        session
            .record_assistant_with_activities(
                second,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("second answer".into())],
                    provider_replay: Some(ProviderReplay::new(
                        "openrouter",
                        "openrouter_plain_reasoning",
                        serde_json::json!("b"),
                    )),
                },
                Vec::new(),
                execution(physical_b.clone(), "provider/model-b-20261004"),
                Usage::known(12, 3),
                false,
            )
            .unwrap();

        let (third, _) = session.begin_turn("third".into(), logical.clone()).unwrap();
        assert!(
            session
                .record_effective_model(third, physical_a.clone())
                .unwrap()
        );
        let third_context = session.context_messages_for(&physical_a).unwrap();
        assert!(
            third_context
                .iter()
                .all(|message| message.provider_replay.is_none())
        );
        session
            .record_assistant_with_activities(
                third,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("third answer".into())],
                    provider_replay: None,
                },
                Vec::new(),
                execution(physical_a.clone(), "claude-a-20261004"),
                Usage::known(14, 4),
                false,
            )
            .unwrap();

        let view = session.view().unwrap();
        assert_eq!(view.last_model, Some(logical.clone()));
        assert_eq!(view.last_effective_model, Some(physical_a.clone()));
        assert_eq!(
            view.last_execution
                .as_ref()
                .map(|execution| &execution.route.effective),
            Some(&physical_a)
        );
        assert_eq!(
            view.messages
                .iter()
                .filter(|message| message.provider_replay.is_some())
                .count(),
            2
        );
        drop(session);

        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.view().unwrap().last_model, Some(logical));
        assert_eq!(
            reopened.view().unwrap().last_effective_model,
            Some(physical_a.clone())
        );
        assert!(
            reopened
                .context_messages_for(&physical_a)
                .unwrap()
                .iter()
                .all(|message| message.provider_replay.is_none())
        );
        assert_eq!(
            reopened
                .messages()
                .unwrap()
                .iter()
                .filter(|message| message.provider_replay.is_some())
                .count(),
            2
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provider_rebase_is_durable_and_keeps_raw_assistant_history() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "anthropic".into(),
            model: "claude-test".into(),
        };
        let (first, _) = session.begin_turn("first".into(), model.clone()).unwrap();
        session
            .record_assistant(
                first,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("first answer".into())],
                    provider_replay: Some(
                        ProviderReplay::new(
                            "anthropic",
                            "anthropic_content_blocks",
                            serde_json::json!({"blocks":[{"type":"thinking",
                                "thinking":"","signature":"signed"},{"type":"text",
                                "text":"first answer"}],"prefix_sha256":"a".repeat(64)}),
                        )
                        .with_prefix_binding(true),
                    ),
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let (second, _) = session.begin_turn("second".into(), model.clone()).unwrap();
        assert!(
            session.context_messages_for(&model).unwrap()[1]
                .provider_replay
                .is_some()
        );
        session.rebase_provider_replay(second).unwrap();
        assert!(
            session.context_messages_for(&model).unwrap()[1]
                .provider_replay
                .is_none()
        );
        assert_eq!(
            session.messages().unwrap()[1]
                .provider_replay
                .as_ref()
                .unwrap()
                .kind,
            "anthropic_content_blocks"
        );
        session
            .record_assistant(
                second,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("second answer".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        assert!(matches!(
            session.rebase_provider_replay(second),
            Err(SessionError::InvalidHistory)
        ));
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(
            reopened.context_messages_for(&model).unwrap()[1]
                .provider_replay
                .is_none()
        );
        assert!(reopened.messages().unwrap()[1].provider_replay.is_some());
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
                    images: Vec::new(),
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
                    result: serde_json::json!({"content":"b".repeat(8000)}),
                    images: Vec::new(),
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
        assert_eq!(
            session
                .compaction_plan(1000, usize::MAX)
                .unwrap()
                .unwrap()
                .through_entry,
            4
        );
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compaction_keeps_oversized_recent_tool_batch_when_older_cut_exists() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let (old_turn, _) = session
            .begin_turn("old task".into(), model.clone())
            .unwrap();
        session
            .record_assistant(
                old_turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("old answer".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let (current_turn, _) = session.begin_turn("read big file".into(), model).unwrap();
        session
            .record_assistant(
                current_turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "call-1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path":"big.txt"}),
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
                current_turn,
                ToolResult {
                    call_id: "call-1".into(),
                    name: "read".into(),
                    result: serde_json::json!({"content":"x".repeat(8000)}),
                    images: Vec::new(),
                    is_error: false,
                },
            )
            .unwrap();
        let plan = session.compaction_plan(1000, usize::MAX).unwrap().unwrap();
        assert_eq!(plan.through_entry, 3);
        assert_eq!(plan.messages.len(), 2);
        session
            .record_compaction(
                plan.through_entry,
                "old task done".into(),
                test_execution(),
                Usage::unknown(),
            )
            .unwrap();
        let context = session.context_messages().unwrap();
        assert_eq!(context.len(), 4);
        assert!(matches!(context[2].content[0], Content::ToolCall(_)));
        assert!(matches!(context[3].content[0], Content::ToolResult(_)));
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.context_messages().unwrap(), context);
        drop(reopened);
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
            .record_steerings(
                turn,
                vec![Message {
                    role: Role::User,
                    content: vec![Content::Text("follow-up steering".into())],
                    provider_replay: None,
                }],
            )
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
                    images: Vec::new(),
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
    fn model_context_changes_are_durable_and_elide_repeats() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let (turn, _) = session.begin_turn("hello".into(), model).unwrap();
        let first = ModelContextSnapshot {
            instructions: "first instructions".into(),
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }],
        };
        assert!(session.record_model_context(turn, first.clone()).unwrap());
        assert!(!session.record_model_context(turn, first).unwrap());

        let changed = ModelContextSnapshot {
            instructions: "changed instructions".into(),
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }],
        };
        assert!(session.record_model_context(turn, changed.clone()).unwrap());
        let view = session.view().unwrap();
        assert_eq!(view.last_context, Some(changed.clone()));
        assert_eq!(
            view.entries
                .iter()
                .filter(|entry| matches!(entry, SessionEntry::ModelContextChanged { .. }))
                .count(),
            2
        );
        drop(session);

        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.view().unwrap().last_context, Some(changed));
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clone_and_fork_follow_model_context_history() {
        let (root, path) = fixture();
        let session = Session::create(&path, &root).unwrap();
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let context = |instructions: &str| ModelContextSnapshot {
            instructions: instructions.into(),
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "Read".into(),
                input_schema: serde_json::json!({"type":"object"}),
            }],
        };

        let (first, _) = session.begin_turn("first".into(), model.clone()).unwrap();
        session
            .record_model_context(first, context("first context"))
            .unwrap();
        session
            .record_assistant(
                first,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("first done".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();

        let (second, _) = session.begin_turn("second".into(), model).unwrap();
        session
            .record_model_context(second, context("second context"))
            .unwrap();
        session
            .record_assistant(
                second,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("second done".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();

        let clone = session.clone_to(root.join("clone-context.sqlite")).unwrap();
        assert_eq!(
            clone.model_context().unwrap().unwrap().instructions,
            "second context"
        );

        let before = session
            .fork_to(
                root.join("before-context.sqlite"),
                ForkPoint::BeforeTurn(second),
            )
            .unwrap();
        assert_eq!(
            before.model_context().unwrap().unwrap().instructions,
            "first context"
        );

        let after = session
            .fork_to(
                root.join("after-context.sqlite"),
                ForkPoint::AfterTurn(second),
            )
            .unwrap();
        assert_eq!(
            after.model_context().unwrap().unwrap().instructions,
            "second context"
        );

        drop(after);
        drop(before);
        drop(clone);
        drop(session);
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
            .record_compaction(
                3,
                "first is done".into(),
                test_execution(),
                Usage::unknown(),
            )
            .unwrap();
        let (second, _) = source.begin_turn("second".into(), model.clone()).unwrap();
        let original = source.view().unwrap();
        let cloned = source.clone_to(&clone_path).unwrap();
        let source_id = source.provider_session_id();
        let clone_id = cloned.provider_session_id();
        assert_ne!(source_id, clone_id);
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
            Session::open(&path).unwrap().provider_session_id(),
            source_id
        );
        assert_eq!(
            Session::open(&clone_path).unwrap().provider_session_id(),
            clone_id
        );
        assert_eq!(
            Session::inspect(&clone_path).unwrap().last_end,
            Some((second, TurnEndReason::Interrupted))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
