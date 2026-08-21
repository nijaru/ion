//! Process `Runtime`, single-writer `SessionRuntime`, and the durable
//! commit flow (DESIGN.md §4, §8, §9, §10, §11).
//!
//! The process-level `Runtime` owns composition and the session
//! registry; one loaded session has exactly one mutation authority, its
//! `SessionRuntime` task. Transitions are staged on a full clone of the
//! active operation, committed to SQLite as one transaction, and only
//! then installed — a failed commit never mutates authoritative state
//! (§26.2); a store failure that also breaks the failure path fences the
//! session at its last durable checkpoint. Provider/tool I/O stays off
//! the mutation line; only bounded local persistence is awaited (§4.3).

use std::fmt;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

use crate::error::{CommandError, RuntimeError};
use crate::ids::{EffectId, InboxId, OperationId, RuntimeCursor, SessionId};
use crate::provider::{EngineSignal, Provider, ProviderRequest};
use crate::session::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition, project_transcript,
};
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    InboxStatus, LoadedSession, SessionRecord, SessionStore, SettledEffect, StoreError,
};
use crate::tool::{RecoveryClass, ToolCall, ToolRegistry, ToolResult, ToolSpec};

const COMMAND_CAPACITY: usize = 32;
const ENGINE_CAPACITY: usize = 64;
const SUBSCRIBER_CAPACITY: usize = 64;

type EventReceiver = mpsc::Receiver<Result<RuntimeEvent, RuntimeError>>;
type SubscribeReply = Result<(SessionSnapshot, EventReceiver), CommandError>;
type ToolSettlement = (EffectId, ToolResult);

/// Live presentation events (DESIGN.md §21.3). Durable semantic state
/// lives in session entries and operation state, never here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    OperationStarted {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        prompt: String,
    },
    AssistantTextDelta {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        text: String,
    },
    ToolStarted {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        call_id: u64,
        tool: String,
    },
    OperationFinished {
        cursor: RuntimeCursor,
        operation_id: OperationId,
    },
    OperationFailed {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        message: String,
    },
    OperationCancelled {
        cursor: RuntimeCursor,
        operation_id: OperationId,
    },
    SessionClosed {
        cursor: RuntimeCursor,
    },
}

impl RuntimeEvent {
    #[must_use]
    pub const fn cursor(&self) -> RuntimeCursor {
        match self {
            Self::OperationStarted { cursor, .. }
            | Self::AssistantTextDelta { cursor, .. }
            | Self::ToolStarted { cursor, .. }
            | Self::OperationFinished { cursor, .. }
            | Self::OperationFailed { cursor, .. }
            | Self::OperationCancelled { cursor, .. }
            | Self::SessionClosed { cursor } => *cursor,
        }
    }
}

/// Live status of the session's active operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStatus {
    Idle,
    Active {
        operation_id: OperationId,
        prompt: String,
        state: OperationState,
    },
}

/// Snapshot-plus-cursor view of one session (DESIGN.md §21.2). The
/// durable semantic view is the session entry log; live events are
/// never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub cursor: RuntimeCursor,
    pub operation: OperationStatus,
    pub entries: Vec<SessionEntry>,
}

pub struct EventSubscription {
    rx: mpsc::Receiver<Result<RuntimeEvent, RuntimeError>>,
}

impl EventSubscription {
    pub async fn recv(&mut self) -> Result<RuntimeEvent, RuntimeError> {
        match self.rx.recv().await {
            Some(result) => result,
            None => Err(RuntimeError::SubscriptionClosed),
        }
    }
}

enum SessionCommand {
    Submit {
        prompt: String,
        reply: oneshot::Sender<Result<OperationId, CommandError>>,
    },
    Steer {
        text: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    FollowUp {
        text: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    Cancel {
        operation_id: OperationId,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    Subscribe {
        reply: oneshot::Sender<SubscribeReply>,
    },
    Close {
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
}

/// Command sender for the process runtime (DESIGN.md §8.1). Session
/// commands live on [`SessionHandle`]; one-shot callers reach the sole
/// session through [`Runtime::session`].
#[derive(Clone)]
pub struct RuntimeHandle {
    tx: mpsc::Sender<SessionCommand>,
}

impl fmt::Debug for RuntimeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeHandle").finish_non_exhaustive()
    }
}

impl RuntimeHandle {
    /// Close the runtime's sessions and shut down (DESIGN.md §25.2).
    pub async fn shutdown(&self) -> Result<(), CommandError> {
        self.request(|reply| SessionCommand::Close { reply }).await
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, CommandError>>) -> SessionCommand,
    ) -> Result<T, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx.try_send(build(reply)).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
            mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
        })?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }
}

/// Command sender for one loaded session (DESIGN.md §8.1). Success means
/// the transition authority accepted the command durably (P4).
#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::Sender<SessionCommand>,
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionHandle").finish_non_exhaustive()
    }
}

impl SessionHandle {
    /// Accept a prompt durably and open a new operation when idle.
    pub async fn submit(&self, prompt: impl Into<String>) -> Result<OperationId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Submit {
                prompt: prompt.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Join the active operation at its next reasoning boundary
    /// (DESIGN.md §9.2).
    pub async fn steer(&self, text: impl Into<String>) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Steer {
                text: text.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Queue input applied after the current response reaches its
    /// follow-up boundary; the same operation continues until idle
    /// (DESIGN.md §9.3).
    pub async fn follow_up(&self, text: impl Into<String>) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::FollowUp {
                text: text.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Request semantic cancellation of the active operation
    /// (DESIGN.md §9.4). Acknowledgment means the request is durable;
    /// settlement arrives as an event.
    pub async fn cancel(&self, operation_id: OperationId) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Cancel {
                operation_id,
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    pub async fn snapshot(&self) -> Result<SessionSnapshot, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Subscribe { reply })
            .map_err(command_send_error)?;
        let (snapshot, _events) = rx.await.map_err(|_| CommandError::RuntimeDropped)??;
        Ok(snapshot)
    }

    /// Snapshot plus bounded live events (DESIGN.md §21.2). A consumer
    /// that falls behind resynchronizes from a fresh snapshot; past
    /// events are never replayed.
    pub async fn subscribe(&self) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Subscribe { reply })
            .map_err(command_send_error)?;
        let (snapshot, events) = rx.await.map_err(|_| CommandError::RuntimeDropped)??;
        Ok((snapshot, EventSubscription { rx: events }))
    }

    /// Close the session (DESIGN.md §9.5): lifecycle shutdown, never a
    /// user cancellation. The reply arrives after the suspension commit
    /// and task drainage complete. An open operation stays recoverable.
    pub async fn close(&self) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Close { reply })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }
}

fn command_send_error(err: mpsc::error::TrySendError<SessionCommand>) -> CommandError {
    match err {
        mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
        mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
    }
}

/// Process-level runtime: composition and the session registry. v0 keeps
/// exactly one loaded session (DESIGN.md §32 Step 1).
pub struct Runtime {
    handle: RuntimeHandle,
    session: SessionHandle,
    session_id: SessionId,
    join: JoinHandle<()>,
}

impl Runtime {
    /// Compose the runtime with one durable session in the default data
    /// root. Panics if the store cannot be opened; hosts that need
    /// graceful handling use [`Runtime::start_with_store`].
    #[must_use]
    pub fn start(provider: impl Provider, tools: ToolRegistry) -> Self {
        let store = SessionStore::open(crate::store::default_db_path())
            .expect("open the default session store");
        Self::start_with_store(provider, tools, store)
    }

    /// Compose the runtime with one new durable session in `store`.
    #[must_use]
    pub fn start_with_store(
        provider: impl Provider,
        tools: ToolRegistry,
        store: SessionStore,
    ) -> Self {
        Self::spawn_session(provider, tools, store, SessionId::generate(), None)
    }

    /// Reopen a previously persisted session: the transcript and any
    /// open operation are rebuilt from the store (DESIGN.md §32 Step 2,
    /// §9.5). Recovery decisions for a non-terminal operation are Step 3
    /// work; until then an open operation surfaces in the snapshot and
    /// blocks new submits.
    pub async fn open_session(
        provider: impl Provider,
        tools: ToolRegistry,
        store: SessionStore,
        session_id: SessionId,
    ) -> Result<Self, RuntimeError> {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        Ok(Self::spawn_session(
            provider,
            tools,
            store,
            session_id,
            Some(loaded),
        ))
    }

    #[must_use]
    fn spawn_session(
        provider: impl Provider,
        tools: ToolRegistry,
        store: SessionStore,
        session_id: SessionId,
        loaded: Option<LoadedSession>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let handle = RuntimeHandle { tx: tx.clone() };
        let session = SessionHandle { tx };
        let provider = Arc::new(provider);
        let tools = Arc::new(tools);
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let join = tokio::spawn(async move {
            SessionRuntime::new(session_id, cwd, provider, tools, store, rx, loaded)
                .run()
                .await;
        });
        Self {
            handle,
            session,
            session_id,
            join,
        }
    }

    #[must_use]
    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    /// The sole loaded session.
    #[must_use]
    pub fn session(&self) -> SessionHandle {
        self.session.clone()
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub async fn join(self) -> Result<(), RuntimeError> {
        self.join
            .await
            .map_err(|_| RuntimeError::OperationFailed("runtime task panicked".to_owned()))
    }
}

struct Subscriber {
    tx: mpsc::Sender<Result<RuntimeEvent, RuntimeError>>,
}

/// The live, in-memory side of the active operation. Cloned whole to
/// stage a transition: a failed durable commit discards the staged clone
/// and never mutates live state (DESIGN.md §26.2).
#[derive(Clone)]
struct ActiveOperation {
    machine: OperationMachine,
    cancel: CancellationToken,
    state_seq: u64,
    /// The one in-flight effect intent, if any.
    open_effect: Option<EffectRecord>,
    /// Inbox items durably accepted but not yet applied.
    pending_steers: Vec<InboxId>,
    pending_followups: Vec<InboxId>,
}

/// The single-writer owner of one loaded session's mutable live state
/// (DESIGN.md §4.3).
struct SessionRuntime<P> {
    session_id: SessionId,
    cwd: String,
    provider: Arc<P>,
    tools: Arc<ToolRegistry>,
    store: SessionStore,
    commands: mpsc::Receiver<SessionCommand>,
    engine_tx: mpsc::Sender<EngineSignal>,
    engine_rx: mpsc::Receiver<EngineSignal>,
    tool_tx: mpsc::Sender<ToolSettlement>,
    tool_rx: mpsc::Receiver<ToolSettlement>,
    cancel_root: CancellationToken,
    tracker: TaskTracker,
    cursor: RuntimeCursor,
    /// Canonical semantic session view, mirroring the durable store.
    entries: Vec<SessionEntry>,
    /// Next storage-assigned entry sequence.
    next_entry_seq: u64,
    operation: Option<ActiveOperation>,
    /// Ephemeral draft of the in-flight model step; never durable.
    draft_text: String,
    draft_calls: Vec<ToolCall>,
    /// Monotonic model-step counter for the active operation; provider
    /// signals carry the step that produced them, and stale generations
    /// are dropped.
    model_step: u64,
    subscribers: Vec<Subscriber>,
    closed: bool,
    /// True when reopened from the store; the session row already exists.
    resumed: bool,
}

impl<P: Provider> SessionRuntime<P> {
    fn new(
        session_id: SessionId,
        cwd: String,
        provider: Arc<P>,
        tools: Arc<ToolRegistry>,
        store: SessionStore,
        commands: mpsc::Receiver<SessionCommand>,
        loaded: Option<LoadedSession>,
    ) -> Self {
        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        let mut runtime = Self {
            session_id,
            cwd,
            provider,
            tools,
            store,
            commands,
            engine_tx,
            engine_rx,
            tool_tx,
            tool_rx,
            cancel_root: CancellationToken::new(),
            tracker: TaskTracker::new(),
            cursor: RuntimeCursor::default(),
            entries: Vec::new(),
            next_entry_seq: 1,
            operation: None,
            draft_text: String::new(),
            draft_calls: Vec::new(),
            model_step: 0,
            subscribers: Vec::new(),
            closed: false,
            resumed: false,
        };
        if let Some(loaded) = loaded {
            runtime.resumed = true;
            runtime.restore_from(loaded);
        }
        runtime
    }

    /// Rebuild live state from a loaded session: transcript, entry
    /// sequence, and — for a non-terminal operation — the complete
    /// machine, its pending inbox, and its pending effect intent.
    fn restore_from(&mut self, loaded: LoadedSession) {
        let mut max_seq = 0;
        for (seq, entry) in loaded.entries {
            max_seq = max_seq.max(seq);
            self.entries.push(entry);
        }
        self.next_entry_seq = max_seq + 1;
        for operation in loaded.operations {
            let (state_seq, payload) = operation.latest;
            if matches!(payload.state, OperationState::Finished(_)) {
                // Terminal operations stay in the transcript only.
                continue;
            }
            // Suspended and mid-flight operations are both recoverable
            // state (§9.5); they rebuild and surface, and Step 3 decides
            // what resuming them means.
            if self.operation.is_some() {
                error!(
                    session = %self.session_id,
                    operation = %operation.id,
                    "multiple open operations in durable state; refusing to guess"
                );
                self.closed = true;
                return;
            }
            let steers: Vec<InboxItem> = loaded
                .pending_inbox
                .iter()
                .filter(|item| item.kind == InboxKind::Steer)
                .map(|item| InboxItem {
                    kind: item.kind.clone(),
                    text: item.text.clone(),
                })
                .collect();
            let followups: Vec<InboxItem> = loaded
                .pending_inbox
                .iter()
                .filter(|item| item.kind == InboxKind::FollowUp)
                .map(|item| InboxItem {
                    kind: item.kind.clone(),
                    text: item.text.clone(),
                })
                .collect();
            let machine = OperationMachine::restore(
                operation.id,
                payload.prompt.clone(),
                payload.tools.clone(),
                payload.state.clone(),
                payload.cancel_requested,
                steers,
                followups,
            );
            info!(
                session = %self.session_id,
                operation = %operation.id,
                state = ?payload.state,
                "reopened an open operation; recovery is Step 3 work"
            );
            self.operation = Some(ActiveOperation {
                machine,
                cancel: self.cancel_root.child_token(),
                state_seq,
                open_effect: payload.open_effect.clone(),
                pending_steers: loaded
                    .pending_inbox
                    .iter()
                    .filter(|item| item.kind == InboxKind::Steer)
                    .map(|item| item.id)
                    .collect(),
                pending_followups: loaded
                    .pending_inbox
                    .iter()
                    .filter(|item| item.kind == InboxKind::FollowUp)
                    .map(|item| item.id)
                    .collect(),
            });
        }
    }

    async fn run(mut self) {
        if !self.resumed && !self.closed {
            let record = SessionRecord {
                id: self.session_id,
                cwd: self.cwd.clone(),
                title: String::new(),
            };
            if let Err(err) = self.store.create_session(record).await {
                error!(
                    session = %self.session_id,
                    %err,
                    "session row not durable; session will not start"
                );
                self.closed = true;
                self.subscribers.clear();
                return;
            }
        }
        info!(session = %self.session_id, "session opened");
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    if self.handle_command(command).await {
                        break;
                    }
                }
                signal = self.engine_rx.recv() => {
                    if let Some(signal) = signal {
                        self.handle_engine(signal).await;
                    }
                }
                result = self.tool_rx.recv() => {
                    if let Some(result) = result {
                        self.handle_tool_result(result).await;
                    }
                }
            }
        }
        // The session task is ending; the close result has no caller.
        let _ = self.close_internal().await;
    }

    /// Returns true when the session loop must exit.
    async fn handle_command(&mut self, command: SessionCommand) -> bool {
        match command {
            SessionCommand::Submit { prompt, reply } => {
                let _ = reply.send(self.submit(prompt).await);
                false
            }
            SessionCommand::Steer { text, reply } => {
                let _ = reply.send(self.enqueue_inbox(InboxKind::Steer, text).await);
                false
            }
            SessionCommand::FollowUp { text, reply } => {
                let _ = reply.send(self.enqueue_inbox(InboxKind::FollowUp, text).await);
                false
            }
            SessionCommand::Cancel {
                operation_id,
                reply,
            } => {
                let _ = reply.send(self.cancel(operation_id).await);
                false
            }
            SessionCommand::Subscribe { reply } => {
                let _ = reply.send(self.subscribe());
                false
            }
            SessionCommand::Close { reply } => {
                // The reply arrives after suspension and drainage, not
                // before (DESIGN.md §25.2).
                let result = self.close_internal().await;
                let _ = reply.send(result);
                true
            }
        }
    }

    async fn submit(&mut self, prompt: String) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if let Some(active) = &self.operation {
            return Err(CommandError::Busy {
                operation_id: active.machine.operation_id(),
            });
        }
        let operation_id = OperationId::generate();
        let (machine, applied) =
            OperationMachine::accept(operation_id, prompt.clone(), self.tools.specs());
        let root_inbox = InboxRecord {
            id: InboxId::generate(),
            kind: InboxKind::Prompt,
            text: prompt.clone(),
            status: InboxStatus::Applied,
        };
        let entry = self.stage_entry(&applied.entries[0]);
        let checkpoint = CheckpointRecord {
            state_seq: 1,
            payload: CheckpointPayload {
                state: machine.state().clone(),
                cancel_requested: false,
                prompt: machine.prompt().to_owned(),
                tools: machine_snapshot_tools(&machine),
                open_effect: None,
            },
        };
        // Accepted intent is durable before acknowledgment (P4, §9.1).
        self.store
            .begin_operation(self.session_id, operation_id, root_inbox, checkpoint, entry)
            .await
            .map_err(persistence_command_error)?;

        let active = ActiveOperation {
            machine,
            cancel: self.cancel_root.child_token(),
            state_seq: 1,
            open_effect: None,
            pending_steers: Vec::new(),
            pending_followups: Vec::new(),
        };
        self.entries.extend(applied.entries.iter().cloned());
        self.next_entry_seq += 1;
        self.emit(RuntimeEvent::OperationStarted {
            cursor: RuntimeCursor::default(),
            operation_id,
            prompt,
        });
        self.operation = Some(active);
        self.draft_text.clear();
        self.draft_calls.clear();
        self.advance().await;
        Ok(operation_id)
    }

    async fn enqueue_inbox(&mut self, kind: InboxKind, text: String) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let inbox_id = InboxId::generate();
        // Stage on a full clone; a failed commit discards the clone and
        // never mutates live state (DESIGN.md §26.2).
        let mut staged = self
            .operation
            .clone()
            .ok_or(CommandError::NoActiveOperation)?;
        let applied = staged
            .machine
            .apply(Transition::ApplyInbox {
                item: InboxItem {
                    kind: kind.clone(),
                    text: text.clone(),
                },
            })
            .expect("inbox apply from an active operation");
        let applied_now = !applied.entries.is_empty();
        let applied_entries = applied.entries.clone();
        let record = InboxRecord {
            id: inbox_id,
            kind: kind.clone(),
            text,
            status: if applied_now {
                InboxStatus::Applied
            } else {
                InboxStatus::Pending
            },
        };
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            applied.entries,
            Vec::new(),
            Vec::new(),
            vec![record],
            Vec::new(),
        );
        self.store
            .commit(request)
            .await
            .map_err(persistence_command_error)?;

        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        if applied_now {
            self.entries.extend(applied_entries);
        } else {
            match kind {
                InboxKind::Steer => staged.pending_steers.push(inbox_id),
                InboxKind::FollowUp => staged.pending_followups.push(inbox_id),
                InboxKind::Prompt => {}
            }
        }
        self.operation = Some(staged);
        self.advance().await;
        Ok(())
    }

    /// Request semantic cancellation (DESIGN.md §9.4): the request is
    /// durable before acknowledgment, then descendant effects are
    /// signalled.
    async fn cancel(&mut self, operation_id: OperationId) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let active_id = self
            .operation
            .as_ref()
            .map(|active| active.machine.operation_id());
        if active_id.is_none() {
            return Err(CommandError::NoActiveOperation);
        }
        if active_id != Some(operation_id) {
            return Err(CommandError::NotActive { operation_id });
        }
        let mut staged = self.operation.clone().expect("checked above");
        staged
            .machine
            .apply(Transition::CancelRequested)
            .expect("cancel request from an active operation");
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        self.store
            .commit(request)
            .await
            .map_err(persistence_command_error)?;
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        staged.cancel.cancel();
        self.operation = Some(staged);
        Ok(())
    }

    /// Drive the machine forward from quiescent states: drain queued
    /// inbox items at their boundaries, then start the next model step
    /// or admit the next planned tool. Each move commits durably before
    /// its effect starts (§12.1).
    async fn advance(&mut self) {
        loop {
            let Some(state) = self
                .operation
                .as_ref()
                .map(|active| active.machine.state().clone())
            else {
                return;
            };
            match state {
                OperationState::Finished(_) => {
                    self.operation.take();
                    return;
                }
                OperationState::Accepted | OperationState::NeedAssistant => {
                    if self
                        .operation
                        .as_ref()
                        .is_some_and(|active| active.machine.has_queued_steers())
                    {
                        if !self.drain_queued(false).await {
                            return;
                        }
                        continue;
                    }
                    if !self.start_model_step().await {
                        return;
                    }
                    return;
                }
                OperationState::NeedContinuation => {
                    if self
                        .operation
                        .as_ref()
                        .is_some_and(|active| active.machine.has_queued_inbox())
                    {
                        if !self.drain_queued(true).await {
                            return;
                        }
                        continue;
                    }
                    panic!("NeedContinuation without queued inbox is impossible state");
                }
                OperationState::ToolsPlanned { .. } => {
                    if !self.admit_next_tool().await {
                        return;
                    }
                    return;
                }
                _ => return,
            }
        }
    }

    /// Drain queued inbox items as one durable transaction. Steers drain
    /// at reasoning boundaries; follow-ups only at the follow-up
    /// boundary. Returns false when persistence failed.
    async fn drain_queued(&mut self, include_followups: bool) -> bool {
        let (mut staged, drained, request, new_entry_seq) = {
            let active = self.operation.clone().expect("drain needs an operation");
            let mut staged = active.clone();
            let mut drained = staged.machine.drain_steers().expect("steer drain");
            let mut applied_ids = staged
                .pending_steers
                .drain(..drained.len())
                .collect::<Vec<_>>();
            if include_followups {
                let more = staged
                    .machine
                    .drain_followups()
                    .expect("follow-up drain at the follow-up boundary");
                applied_ids.extend(staged.pending_followups.drain(..more.len()));
                drained.extend(more);
            }
            let mut entries = Vec::new();
            for applied in &drained {
                entries.extend(applied.entries.iter().cloned());
            }
            let (request, new_entry_seq) = build_commit_request(
                self.session_id,
                &staged,
                staged.state_seq + 1,
                self.next_entry_seq,
                entries,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                applied_ids,
            );
            (staged, drained, request, new_entry_seq)
        };
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return false;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        for applied in &drained {
            self.entries.extend(applied.entries.iter().cloned());
        }
        self.operation = Some(staged);
        true
    }

    /// Commit the model-step effect intent, then spawn the provider
    /// effect. Returns false when persistence failed.
    async fn start_model_step(&mut self) -> bool {
        let prompt = project_transcript(&self.entries);
        let mut staged = self.operation.clone().expect("step needs an operation");
        let applied = staged
            .machine
            .apply(Transition::StartModelStep {
                prompt: prompt.clone(),
            })
            .expect("start model step from a quiescent state");
        let EffectIntent::ModelStep {
            operation_id,
            tools,
            ..
        } = applied.intents[0].clone()
        else {
            panic!("StartModelStep must yield a model-step intent");
        };
        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "model_step".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({ "prompt": prompt, "tools": tools }),
        };
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            vec![effect.clone()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return false;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        staged.open_effect = Some(effect);
        self.operation = Some(staged);
        self.spawn_model_step(operation_id, prompt, tools);
        true
    }

    /// Commit a tool effect intent, then spawn the tool effect (or
    /// settle a validation denial through the normal path). Returns
    /// false when persistence failed.
    async fn admit_next_tool(&mut self) -> bool {
        let mut staged = self.operation.clone().expect("admit needs an operation");
        let applied = staged
            .machine
            .apply(Transition::AdmitNextTool)
            .expect("admit next tool from ToolsPlanned");
        let EffectIntent::Tool { call } = applied.intents[0].clone() else {
            panic!("AdmitNextTool must yield a tool intent");
        };
        // Validation happens before the effect intent is durable; a
        // denial settles as a model-visible result (§17.3, §16.5).
        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: format!("tool:{}", call.name),
            recovery_class: self.tools.recovery_class(&call.name),
            effective_input: serde_json::json!({
                "tool": call.name,
                "arguments": call.arguments,
            }),
        };
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            vec![effect.clone()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return false;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        let effect_id = effect.id;
        staged.open_effect = Some(effect);
        self.operation = Some(staged);
        if let Err(message) = self.tools.validate(&call.name, &call.arguments) {
            // The denial settles through the normal tool-result path; the
            // tool never started, so no ToolStarted event is emitted.
            let _ = self.tool_tx.try_send((
                effect_id,
                ToolResult::Err {
                    call_id: call.call_id,
                    error: message,
                },
            ));
        } else {
            self.emit(RuntimeEvent::ToolStarted {
                cursor: RuntimeCursor::default(),
                operation_id: call.operation_id,
                call_id: call.call_id,
                tool: call.name.clone(),
            });
            self.spawn_tool_effect(effect_id_of(self.operation.as_ref()), call);
        }
        true
    }

    fn spawn_model_step(
        &mut self,
        operation_id: OperationId,
        prompt: String,
        tools: Vec<ToolSpec>,
    ) {
        let provider = Arc::clone(&self.provider);
        let cancel = self
            .operation
            .as_ref()
            .map(|active| active.cancel.child_token())
            .unwrap_or_else(|| self.cancel_root.child_token());
        let out = self.engine_tx.clone();
        self.model_step += 1;
        let step = self.model_step;
        let request = ProviderRequest {
            operation_id,
            step,
            prompt,
            tools,
        };
        debug!(%operation_id, step, "starting model step effect");
        let terminal = self.engine_tx.clone();
        self.tracker.spawn(async move {
            provider.run(request, cancel, out.clone()).await;
            let _ = terminal
                .send(EngineSignal::ProviderExited { operation_id, step })
                .await;
        });
    }

    fn spawn_tool_effect(&mut self, effect_id: Option<EffectId>, call: ToolCall) {
        let Some(effect_id) = effect_id else {
            return;
        };
        let tools = Arc::clone(&self.tools);
        let cancel = self
            .operation
            .as_ref()
            .map(|active| active.cancel.child_token())
            .unwrap_or_else(|| self.cancel_root.child_token());
        let tool_tx = self.tool_tx.clone();
        let ToolCall {
            call_id,
            name,
            arguments,
            ..
        } = call;
        debug!(tool = %name, %call_id, "dispatching tool effect");
        self.tracker.spawn(async move {
            let outcome = tools.execute(&name, &arguments, cancel).await;
            let result = if outcome.is_error {
                ToolResult::Err {
                    call_id,
                    error: outcome.output,
                }
            } else {
                ToolResult::Ok {
                    call_id,
                    output: outcome.output,
                }
            };
            let _ = tool_tx.send((effect_id, result)).await;
        });
    }

    async fn handle_engine(&mut self, signal: EngineSignal) {
        let Some(active) = &self.operation else {
            debug!("ignored engine signal with no active operation");
            return;
        };
        if active.machine.operation_id() != signal_operation_id(&signal) {
            debug!(?signal, "ignored stale engine signal");
            return;
        }
        if signal_step(&signal) != self.model_step {
            debug!(?signal, "ignored engine signal from a stale model step");
            return;
        }
        match signal {
            EngineSignal::TextDelta { text, .. } => {
                self.draft_text.push_str(&text);
                self.emit(RuntimeEvent::AssistantTextDelta {
                    cursor: RuntimeCursor::default(),
                    operation_id: active.machine.operation_id(),
                    text,
                });
            }
            EngineSignal::ToolCallCompleted { call, .. } => {
                if call.operation_id != active.machine.operation_id() {
                    debug!(?call, "dropped tool call attributed to another operation");
                    return;
                }
                // Buffered until the step completes; tool calls are never
                // executed from partial streamed JSON (DESIGN.md §15.2).
                self.draft_calls.push(call);
            }
            EngineSignal::Completed { .. } => {
                let text = std::mem::take(&mut self.draft_text);
                let tool_calls = std::mem::take(&mut self.draft_calls);
                self.settle_model_step(Transition::ProviderCompleted { text, tool_calls })
                    .await;
            }
            EngineSignal::Failed { message, .. } => {
                let cancel_requested = self
                    .operation
                    .as_ref()
                    .is_some_and(|active| active.machine.cancel_requested());
                let transition = if cancel_requested {
                    Transition::ProviderCancelled
                } else {
                    Transition::ProviderFailed { message }
                };
                self.settle_model_step(transition).await;
            }
            EngineSignal::Cancelled { .. } => {
                self.settle_model_step(Transition::ProviderCancelled).await;
            }
            EngineSignal::ProviderExited { .. } => {
                // A sentinel for the live step means the provider died
                // without a terminal signal; earlier steps were already
                // dropped by the step correlation above.
                self.settle_model_step(Transition::ProviderFailed {
                    message: "provider exited without a terminal signal".to_owned(),
                })
                .await;
            }
        }
    }

    /// Commit a model-step settlement atomically: settled effect (with
    /// its typed outcome), semantic entries, and the next total state
    /// agree in one transaction.
    async fn settle_model_step(&mut self, transition: Transition) {
        let mut staged = self.operation.clone().expect("settle needs an operation");
        if !matches!(
            staged.machine.state(),
            OperationState::AssistantEffectPending
        ) {
            // A same-step exit sentinel after an already-settled step.
            debug!("ignored settlement for an already-settled model step");
            return;
        }
        let applied = staged
            .machine
            .apply(transition)
            .expect("model-step settlement while AssistantEffectPending");
        let settled = staged
            .open_effect
            .take()
            .map(|effect| SettledEffect {
                id: effect.id,
                settlement: serde_json::json!({ "kind": "model_step" }),
            })
            .into_iter()
            .collect();
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            applied.entries.clone(),
            Vec::new(),
            settled,
            Vec::new(),
            Vec::new(),
        );
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.entries.extend(applied.entries);
        self.emit_terminal_state(&applied.state.clone());
        self.operation = Some(staged);
        self.advance().await;
    }

    async fn handle_tool_result(&mut self, settlement: ToolSettlement) {
        let (effect_id, result) = settlement;
        let expected = self
            .operation
            .as_ref()
            .and_then(|active| active.open_effect.as_ref().map(|e| e.id));
        if expected != Some(effect_id) {
            // Stale or unknown tool result: a typed diagnostic, never a
            // panic and never a state change.
            debug!(?effect_id, ?expected, "dropped stale tool settlement");
            return;
        }
        let mut staged = self.operation.clone().expect("settle needs an operation");
        let applied = staged
            .machine
            .apply(Transition::ToolSettled {
                result: result.clone(),
            })
            .expect("tool settlement while ToolEffectPending");
        let settled = vec![SettledEffect {
            id: effect_id,
            settlement: serde_json::json!({ "output": result.into_text() }),
        }];
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            applied.entries.clone(),
            Vec::new(),
            settled,
            Vec::new(),
            Vec::new(),
        );
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        staged.open_effect = None;
        self.entries.extend(applied.entries);
        self.emit_terminal_state(&applied.state.clone());
        self.operation = Some(staged);
        self.advance().await;
    }

    /// A required commit failed: the staged clone is discarded and live
    /// state stays at its last durable checkpoint. Fail the operation
    /// visibly from that checkpoint; if even the failure commit fails,
    /// fence the session — never continue as if durability succeeded
    /// (DESIGN.md §26.2).
    async fn fail_operation_on_persistence(&mut self, err: StoreError) {
        let Some(active) = &self.operation else {
            error!(session = %self.session_id, %err, "persistence failed with no active operation");
            return;
        };
        let operation_id = active.machine.operation_id();
        error!(
            %operation_id,
            %err,
            "durable commit failed; failing the operation from its last checkpoint"
        );
        if matches!(active.machine.state(), OperationState::Finished(_)) {
            // The failed write was the terminal checkpoint itself; the
            // durable operation stays open and recoverable.
            self.emit(RuntimeEvent::OperationFailed {
                cursor: RuntimeCursor::default(),
                operation_id,
                message: format!("persistence failed: {err}"),
            });
            self.operation.take();
            return;
        }
        // Stage the failure from the untouched live machine.
        let mut staged = active.clone();
        staged
            .machine
            .apply(Transition::FailOperation {
                message: format!("persistence failed: {err}"),
            })
            .expect("fail the operation from an open state");
        staged.open_effect = None;
        let (request, _new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        match self.store.commit(request).await {
            Ok(()) => {
                let applied_state = staged.machine.state().clone();
                self.operation = Some(staged);
                self.emit_terminal_state(&applied_state);
                if let Some(active) = &self.operation {
                    active.cancel.cancel();
                }
                self.operation.take();
            }
            Err(second) => {
                // Fatal: memory stays at the last confirmed checkpoint and
                // the session is fenced — no further work is accepted.
                error!(
                    %operation_id,
                    %second,
                    "failure commit also failed; fencing the session at its last checkpoint"
                );
                self.emit(RuntimeEvent::OperationFailed {
                    cursor: RuntimeCursor::default(),
                    operation_id,
                    message: format!("persistence failed fatally: {second}"),
                });
                self.closed = true;
                self.operation.take();
            }
        }
    }

    fn stage_entry(&mut self, entry: &SessionEntry) -> EntryRecord {
        EntryRecord {
            seq: self.next_entry_seq,
            entry: entry.clone(),
        }
    }

    fn emit_terminal_state(&mut self, state: &OperationState) {
        if let OperationState::Finished(outcome) = state {
            let Some(active) = &self.operation else {
                return;
            };
            let operation_id = active.machine.operation_id();
            match outcome {
                OperationOutcome::Completed => {
                    self.emit(RuntimeEvent::OperationFinished {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                    });
                }
                OperationOutcome::Failed(message) => {
                    self.emit(RuntimeEvent::OperationFailed {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        message: message.clone(),
                    });
                }
                OperationOutcome::Cancelled => {
                    self.emit(RuntimeEvent::OperationCancelled {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                    });
                }
                OperationOutcome::Indeterminate => {
                    self.emit(RuntimeEvent::OperationFailed {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        message: "indeterminate".to_owned(),
                    });
                }
            }
        }
    }

    fn subscribe(&mut self) -> SubscribeReply {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        let snapshot = self.snapshot();
        self.subscribers.push(Subscriber { tx });
        Ok((snapshot, rx))
    }

    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            cursor: self.cursor,
            operation: match &self.operation {
                None => OperationStatus::Idle,
                Some(active) => OperationStatus::Active {
                    operation_id: active.machine.operation_id(),
                    prompt: active.machine.prompt().to_owned(),
                    state: active.machine.state().clone(),
                },
            },
            entries: self.entries.clone(),
        }
    }

    /// Session close (DESIGN.md §9.5, §25): stop accepting work, suspend
    /// the open operation durably, signal owned effects, and drain them
    /// while joining so a blocked sender cannot deadlock shutdown. Close
    /// is never a user cancellation.
    async fn close_internal(&mut self) -> Result<(), CommandError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if let Some(active) = &mut self.operation {
            let mut staged = active.clone();
            staged
                .machine
                .apply(Transition::Suspend)
                .expect("suspend from an open operation");
            staged.open_effect = None;
            let (request, new_entry_seq) = build_commit_request(
                self.session_id,
                &staged,
                staged.state_seq + 1,
                self.next_entry_seq,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            match self.store.commit(request).await {
                Ok(()) => {
                    self.next_entry_seq = new_entry_seq;
                    active.machine = staged.machine;
                    active.state_seq = staged.state_seq;
                }
                Err(err) => {
                    error!(
                        session = %self.session_id,
                        %err,
                        "suspend checkpoint failed; durable operation stays open"
                    );
                }
            }
            active.cancel.cancel();
        }
        self.cancel_root.cancel();
        self.tracker.close();
        // Drain while joining: a provider blocked sending into a full
        // engine channel can only finish if someone keeps reading.
        {
            let wait = self.tracker.wait();
            tokio::pin!(wait);
            let mut engine_open = true;
            let mut tool_open = true;
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    signal = self.engine_rx.recv(), if engine_open => {
                        match signal {
                            Some(signal) => drop(signal),
                            None => engine_open = false,
                        }
                    }
                    result = self.tool_rx.recv(), if tool_open => {
                        match result {
                            Some(result) => drop(result),
                            None => tool_open = false,
                        }
                    }
                    else => break,
                }
            }
        }
        self.tracker.wait().await;
        self.emit(RuntimeEvent::SessionClosed {
            cursor: RuntimeCursor::default(),
        });
        self.subscribers.clear();
        Ok(())
    }

    fn emit(&mut self, mut event: RuntimeEvent) {
        self.cursor = self.cursor.next();
        set_cursor(&mut event, self.cursor);
        match &event {
            RuntimeEvent::AssistantTextDelta { .. } => {
                debug!(cursor = %self.cursor, "assistant text delta");
            }
            other => {
                info!(
                    cursor = %self.cursor,
                    session = %self.session_id,
                    event = event_kind(other),
                    "runtime event"
                );
            }
        }
        self.subscribers
            .retain(|sub| match sub.tx.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("detaching lagged runtime subscriber");
                    let _ = sub.tx.try_send(Err(RuntimeError::SubscriptionLagged));
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            });
    }
}

fn machine_snapshot_tools(machine: &OperationMachine) -> Vec<ToolSpec> {
    // The frozen capability snapshot is part of every checkpoint.
    machine.frozen_tools().clone()
}

fn effect_id_of(active: Option<&ActiveOperation>) -> Option<EffectId> {
    active.and_then(|active| active.open_effect.as_ref().map(|e| e.id))
}

/// Build the durable record of one staged transition. Entry sequences are
/// computed from the caller's next value and returned so the allocator
/// only advances after the commit succeeds (DESIGN.md §26.2).
#[allow(clippy::too_many_arguments)]
fn build_commit_request(
    session_id: SessionId,
    staged: &ActiveOperation,
    state_seq: u64,
    next_entry_seq: u64,
    entries: Vec<SessionEntry>,
    open_effects: Vec<EffectRecord>,
    settled_effects: Vec<SettledEffect>,
    inbox: Vec<InboxRecord>,
    inbox_applied: Vec<InboxId>,
) -> (CommitRequest, u64) {
    let mut seq = next_entry_seq;
    let entries = entries
        .into_iter()
        .map(|entry| {
            let entry_seq = seq;
            seq += 1;
            EntryRecord {
                seq: entry_seq,
                entry,
            }
        })
        .collect();
    let request = CommitRequest {
        session_id,
        operation_id: staged.machine.operation_id(),
        checkpoint: CheckpointRecord {
            state_seq,
            payload: CheckpointPayload {
                state: staged.machine.state().clone(),
                cancel_requested: staged.machine.cancel_requested(),
                prompt: staged.machine.prompt().to_owned(),
                tools: staged.machine.frozen_tools().to_vec(),
                open_effect: staged.open_effect.clone(),
            },
        },
        entries,
        open_effects,
        settled_effects,
        inbox,
        inbox_applied,
    };
    (request, seq)
}

fn persistence_command_error(err: StoreError) -> CommandError {
    CommandError::Persistence(err.to_string())
}

fn signal_operation_id(signal: &EngineSignal) -> OperationId {
    match signal {
        EngineSignal::TextDelta { operation_id, .. }
        | EngineSignal::ToolCallCompleted { operation_id, .. }
        | EngineSignal::Completed { operation_id, .. }
        | EngineSignal::Failed { operation_id, .. }
        | EngineSignal::Cancelled { operation_id, .. }
        | EngineSignal::ProviderExited { operation_id, .. } => *operation_id,
    }
}

fn signal_step(signal: &EngineSignal) -> u64 {
    match signal {
        EngineSignal::TextDelta { step, .. }
        | EngineSignal::ToolCallCompleted { step, .. }
        | EngineSignal::Completed { step, .. }
        | EngineSignal::Failed { step, .. }
        | EngineSignal::Cancelled { step, .. }
        | EngineSignal::ProviderExited { step, .. } => *step,
    }
}

fn set_cursor(event: &mut RuntimeEvent, cursor: RuntimeCursor) {
    match event {
        RuntimeEvent::OperationStarted { cursor: slot, .. }
        | RuntimeEvent::AssistantTextDelta { cursor: slot, .. }
        | RuntimeEvent::ToolStarted { cursor: slot, .. }
        | RuntimeEvent::OperationFinished { cursor: slot, .. }
        | RuntimeEvent::OperationFailed { cursor: slot, .. }
        | RuntimeEvent::OperationCancelled { cursor: slot, .. }
        | RuntimeEvent::SessionClosed { cursor: slot } => *slot = cursor,
    }
}

fn event_kind(event: &RuntimeEvent) -> &'static str {
    match event {
        RuntimeEvent::OperationStarted { .. } => "operation_started",
        RuntimeEvent::AssistantTextDelta { .. } => "assistant_text_delta",
        RuntimeEvent::ToolStarted { .. } => "tool_started",
        RuntimeEvent::OperationFinished { .. } => "operation_finished",
        RuntimeEvent::OperationFailed { .. } => "operation_failed",
        RuntimeEvent::OperationCancelled { .. } => "operation_cancelled",
        RuntimeEvent::SessionClosed { .. } => "session_closed",
    }
}

#[cfg(test)]
pub(crate) struct SaturatedHandle {
    handle: RuntimeHandle,
    _rx: mpsc::Receiver<SessionCommand>,
}

#[cfg(test)]
impl SaturatedHandle {
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::channel(1);
        let handle = RuntimeHandle { tx };
        handle
            .fill_queue()
            .expect("first fill occupies the bounded command queue");
        Self { handle, _rx: rx }
    }

    pub(crate) fn handle(&self) -> &RuntimeHandle {
        &self.handle
    }
}

#[cfg(test)]
impl RuntimeHandle {
    fn fill_queue(&self) -> Result<(), CommandError> {
        let (reply, _rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Submit {
                prompt: String::from("fill"),
                reply,
            })
            .map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
                mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
            })
    }
}
