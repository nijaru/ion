//! Process `Runtime`, single-writer `SessionRuntime`, and the durable
//! commit flow (DESIGN.md §4, §8, §9, §10, §11).
//!
//! The process-level `Runtime` owns composition and the session
//! registry; one loaded session has exactly one mutation authority, its
//! `SessionRuntime` task. Transitions are staged on a machine clone,
//! committed to SQLite as one transaction, and only then installed in
//! memory — a failed commit never updates authoritative state
//! (§26.2). Provider/tool I/O stays off the mutation line; only bounded
//! local persistence is awaited (§4.3).

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
    SessionEntry, Transition,
};
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    InboxStatus, SessionRecord, SessionStore, StoreError,
};
use crate::tool::{RecoveryClass, ToolCall, ToolRegistry, ToolResult, ToolSpec};

const COMMAND_CAPACITY: usize = 32;
const ENGINE_CAPACITY: usize = 64;
const SUBSCRIBER_CAPACITY: usize = 64;

type EventReceiver = mpsc::Receiver<Result<RuntimeEvent, RuntimeError>>;
type SubscribeReply = Result<(SessionSnapshot, EventReceiver), CommandError>;

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

    /// Join the active operation at its next safe continuation boundary
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
    /// user cancellation. An open operation stays recoverable.
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

    /// Compose the runtime with one durable session in `store`.
    #[must_use]
    pub fn start_with_store(
        provider: impl Provider,
        tools: ToolRegistry,
        store: SessionStore,
    ) -> Self {
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let handle = RuntimeHandle { tx: tx.clone() };
        let session = SessionHandle { tx };
        let provider = Arc::new(provider);
        let tools = Arc::new(tools);
        let session_id = SessionId::generate();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let join = tokio::spawn(async move {
            SessionRuntime::new(session_id, cwd, provider, tools, store, rx)
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

/// The live, in-memory side of the active operation. Durable truth is in
/// the store; this is rebuilt from it on resume.
struct ActiveOperation {
    machine: OperationMachine,
    cancel: CancellationToken,
    state_seq: u64,
    /// The one in-flight effect intent, if any.
    open_effect: Option<EffectId>,
    /// Inbox items durably accepted but not yet applied.
    pending_inbox: Vec<InboxId>,
}

/// The single-writer owner of one loaded session's mutable live state
/// (DESIGN.md §4.3): bounded mailbox, the active operation machine, the
/// session entry view, snapshots, and its owned effect tasks. It awaits
/// only bounded local persistence on its mutation line; provider/tool
/// I/O runs as spawned effects and re-enters as transitions.
struct SessionRuntime<P> {
    session_id: SessionId,
    cwd: String,
    provider: Arc<P>,
    tools: Arc<ToolRegistry>,
    store: SessionStore,
    commands: mpsc::Receiver<SessionCommand>,
    engine_tx: mpsc::Sender<EngineSignal>,
    engine_rx: mpsc::Receiver<EngineSignal>,
    tool_tx: mpsc::Sender<ToolResult>,
    tool_rx: mpsc::Receiver<ToolResult>,
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
    /// Whether the in-flight model step already delivered a terminal
    /// signal. The provider task's post-run exit sentinel is stale once
    /// it has, or once a later step started.
    step_terminal_seen: bool,
    /// Monotonic model-step counter for the active operation; exit
    /// sentinels carry the step that spawned them.
    model_step: u64,
    subscribers: Vec<Subscriber>,
    closed: bool,
}

impl<P: Provider> SessionRuntime<P> {
    fn new(
        session_id: SessionId,
        cwd: String,
        provider: Arc<P>,
        tools: Arc<ToolRegistry>,
        store: SessionStore,
        commands: mpsc::Receiver<SessionCommand>,
    ) -> Self {
        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        Self {
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
            step_terminal_seen: false,
            model_step: 0,
            subscribers: Vec::new(),
            closed: false,
        }
    }

    async fn run(mut self) {
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
        self.close_internal().await;
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
                let _ = reply.send(Ok(()));
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
        let entry = self.entry_record(&applied.entries[0]);
        let checkpoint = CheckpointRecord {
            state_seq: 1,
            payload: CheckpointPayload {
                state: machine.state().clone(),
                cancel_requested: false,
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
            pending_inbox: Vec::new(),
        };
        self.entries.extend(applied.entries.iter().cloned());
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
        let Some(active) = &mut self.operation else {
            return Err(CommandError::NoActiveOperation);
        };
        let inbox_id = InboxId::generate();
        // Stage on a clone; a failed commit never mutates live state
        // (DESIGN.md §26.2).
        let mut staged = active.machine.clone();
        let applied = staged
            .apply(Transition::ApplyInbox {
                item: InboxItem {
                    kind: kind.clone(),
                    text: text.clone(),
                },
            })
            .expect("inbox apply from an active operation");
        let applied_now = !applied.entries.is_empty();
        let record = InboxRecord {
            id: inbox_id,
            kind,
            text,
            status: if applied_now {
                InboxStatus::Applied
            } else {
                InboxStatus::Pending
            },
        };
        let request = build_commit_request(
            self.session_id,
            staged.operation_id(),
            active.state_seq + 1,
            &mut self.next_entry_seq,
            &staged,
            applied.entries.clone(),
            Vec::new(),
            Vec::new(),
            vec![record],
            Vec::new(),
        );
        self.store
            .commit(request)
            .await
            .map_err(persistence_command_error)?;

        active.machine = staged;
        active.state_seq += 1;
        if applied_now {
            self.entries.extend(applied.entries);
        } else {
            active.pending_inbox.push(inbox_id);
        }
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
        let operation = self
            .operation
            .as_ref()
            .map(|active| active.machine.operation_id());
        if operation.is_none() {
            return Err(CommandError::NoActiveOperation);
        }
        if operation != Some(operation_id) {
            return Err(CommandError::NotActive { operation_id });
        }
        let (staged, request) = {
            let active = self.operation.as_mut().expect("operation checked above");
            let mut staged = active.machine.clone();
            staged
                .apply(Transition::CancelRequested)
                .expect("cancel request from an active operation");
            let request = build_commit_request(
                self.session_id,
                staged.operation_id(),
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &staged,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            (staged, request)
        };
        self.store
            .commit(request)
            .await
            .map_err(persistence_command_error)?;
        let active = self.operation.as_mut().expect("operation checked above");
        active.machine = staged;
        active.state_seq += 1;
        active.cancel.cancel();
        Ok(())
    }

    /// Drive the machine forward from quiescent states: drain queued
    /// inbox items at the continuation boundary, then start the next
    /// model step or admit the next planned tool. Each move commits
    /// durably before its effect starts (§12.1).
    async fn advance(&mut self) {
        loop {
            let Some(active_state) = self
                .operation
                .as_ref()
                .map(|active| active.machine.state().clone())
            else {
                return;
            };
            match active_state {
                OperationState::Finished(_) => {
                    self.operation.take();
                    return;
                }
                OperationState::Accepted
                | OperationState::NeedAssistant
                | OperationState::NeedContinuation => {
                    if self
                        .operation
                        .as_ref()
                        .is_some_and(|active| active.machine.has_queued_inbox())
                    {
                        if !self.drain_inbox().await {
                            return;
                        }
                        continue;
                    }
                    assert!(
                        !matches!(active_state, OperationState::NeedContinuation),
                        "NeedContinuation without queued inbox is impossible state"
                    );
                    if !self.start_model_step().await {
                        return;
                    }
                    return;
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

    /// Drain queued inbox items as one durable transaction. Returns
    /// false when persistence failed and the operation was failed
    /// visibly.
    async fn drain_inbox(&mut self) -> bool {
        let (staged, drained, request) = {
            let active = self.operation.as_mut().expect("drain needs an operation");
            let mut staged = active.machine.clone();
            let drained = staged
                .drain_inbox()
                .expect("inbox drain at a continuation boundary");
            let mut entries = Vec::new();
            for applied in &drained {
                entries.extend(applied.entries.iter().cloned());
            }
            let applied_ids: Vec<InboxId> = active.pending_inbox.drain(..drained.len()).collect();
            let request = build_commit_request(
                self.session_id,
                staged.operation_id(),
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &staged,
                entries,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                applied_ids,
            );
            (staged, drained, request)
        };
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return false;
        }
        let active = self.operation.as_mut().expect("operation present");
        active.machine = staged;
        active.state_seq += 1;
        for applied in &drained {
            self.entries.extend(applied.entries.iter().cloned());
        }
        true
    }

    /// Commit the model-step effect intent, then spawn the provider
    /// effect. Returns false when persistence failed.
    async fn start_model_step(&mut self) -> bool {
        let (staged, applied, effect, request) = {
            let active = self.operation.as_mut().expect("step needs an operation");
            let mut staged = active.machine.clone();
            let applied = staged
                .apply(Transition::StartModelStep)
                .expect("start model step from a quiescent state");
            let EffectIntent::ModelStep { prompt, .. } = applied.intents[0].clone() else {
                panic!("StartModelStep must yield a model-step intent");
            };
            let effect = EffectRecord {
                id: EffectId::generate(),
                kind: "model_step".to_owned(),
                recovery_class: RecoveryClass::ReplaySafe,
                effective_input: serde_json::json!({ "prompt": prompt }),
            };
            let request = build_commit_request(
                self.session_id,
                staged.operation_id(),
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &staged,
                Vec::new(),
                vec![effect.clone()],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            (staged, applied, effect, request)
        };
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return false;
        }
        let active = self.operation.as_mut().expect("operation present");
        active.machine = staged;
        active.state_seq += 1;
        active.open_effect = Some(effect.id);
        let EffectIntent::ModelStep {
            operation_id,
            prompt,
            tools,
        } = applied.intents[0].clone()
        else {
            unreachable!("checked above");
        };
        self.spawn_model_step(operation_id, prompt, tools);
        true
    }

    /// Commit a tool effect intent, then spawn the tool effect (or
    /// settle a validation denial through the normal path). Returns
    /// false when persistence failed.
    async fn admit_next_tool(&mut self) -> bool {
        let (staged, call, effect, request) = {
            let active = self.operation.as_mut().expect("admit needs an operation");
            let mut staged = active.machine.clone();
            let applied = staged
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
                effective_input: call.arguments.clone(),
            };
            let request = build_commit_request(
                self.session_id,
                staged.operation_id(),
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &staged,
                Vec::new(),
                vec![effect.clone()],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            (staged, call, effect, request)
        };
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return false;
        }
        let active = self.operation.as_mut().expect("operation present");
        active.machine = staged;
        active.state_seq += 1;
        active.open_effect = Some(effect.id);
        if let Err(message) = self.tools.validate(&call.name, &call.arguments) {
            // The denial settles through the normal tool-result path; the
            // tool never started, so no ToolStarted event is emitted.
            let _ = self.tool_tx.try_send(ToolResult::Err {
                call_id: call.call_id,
                error: message,
            });
        } else {
            self.emit(RuntimeEvent::ToolStarted {
                cursor: RuntimeCursor::default(),
                operation_id: call.operation_id,
                call_id: call.call_id,
                tool: call.name.clone(),
            });
            self.spawn_tool_effect(call);
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
        self.step_terminal_seen = false;
        self.model_step += 1;
        let step = self.model_step;
        let request = ProviderRequest {
            operation_id,
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

    fn spawn_tool_effect(&mut self, call: ToolCall) {
        let operation_id = call.operation_id;
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
        let _ = operation_id;
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
            let _ = tool_tx.send(result).await;
        });
    }

    async fn handle_engine(&mut self, signal: EngineSignal) {
        let Some(active) = &self.operation else {
            debug!("ignored engine signal with no active operation");
            return;
        };
        let operation_id = active.machine.operation_id();
        if operation_id != signal_operation_id(&signal) {
            debug!(?signal, "ignored stale engine signal");
            return;
        }
        match signal {
            EngineSignal::TextDelta { text, .. } => {
                self.draft_text.push_str(&text);
                self.emit(RuntimeEvent::AssistantTextDelta {
                    cursor: RuntimeCursor::default(),
                    operation_id,
                    text,
                });
            }
            EngineSignal::ToolCallCompleted { call, .. } => {
                // Buffered until the step completes; tool calls are never
                // executed from partial streamed JSON (DESIGN.md §15.2).
                self.draft_calls.push(call);
            }
            EngineSignal::Completed { .. } => {
                self.step_terminal_seen = true;
                let text = std::mem::take(&mut self.draft_text);
                let tool_calls = std::mem::take(&mut self.draft_calls);
                self.settle_model_step(Transition::ProviderCompleted { text, tool_calls })
                    .await;
            }
            EngineSignal::Failed { message, .. } => {
                self.step_terminal_seen = true;
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
                self.step_terminal_seen = true;
                self.settle_model_step(Transition::ProviderCancelled).await;
            }
            EngineSignal::ProviderExited { step, .. } => {
                if self.step_terminal_seen || step != self.model_step {
                    debug!(?signal, "ignored stale provider exit sentinel");
                    return;
                }
                self.step_terminal_seen = true;
                self.settle_model_step(Transition::ProviderFailed {
                    message: "provider exited without a terminal signal".to_owned(),
                })
                .await;
            }
        }
    }

    /// Commit a model-step settlement atomically: settled effect, semantic
    /// entries, and the next total state agree in one transaction.
    async fn settle_model_step(&mut self, transition: Transition) {
        let (staged, applied, request) = {
            let active = self.operation.as_mut().expect("settle needs an operation");
            let mut staged = active.machine.clone();
            let applied = staged
                .apply(transition)
                .expect("model-step settlement while AssistantEffectPending");
            let settled = active.open_effect.take().into_iter().collect();
            let request = build_commit_request(
                self.session_id,
                staged.operation_id(),
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &staged,
                applied.entries.clone(),
                Vec::new(),
                settled,
                Vec::new(),
                Vec::new(),
            );
            (staged, applied, request)
        };
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        let active = self.operation.as_mut().expect("operation present");
        active.machine = staged;
        active.state_seq += 1;
        let finished = applied.state.clone();
        self.entries.extend(applied.entries);
        self.emit_terminal_state(&finished);
        self.advance().await;
    }

    async fn handle_tool_result(&mut self, result: ToolResult) {
        let (staged, applied, request) = {
            let active = self.operation.as_mut().expect("settle needs an operation");
            let mut staged = active.machine.clone();
            let applied = staged
                .apply(Transition::ToolSettled { result })
                .expect("tool settlement while ToolEffectPending");
            let settled = active.open_effect.take().into_iter().collect();
            let request = build_commit_request(
                self.session_id,
                staged.operation_id(),
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &staged,
                applied.entries.clone(),
                Vec::new(),
                settled,
                Vec::new(),
                Vec::new(),
            );
            (staged, applied, request)
        };
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        let active = self.operation.as_mut().expect("operation present");
        active.machine = staged;
        active.state_seq += 1;
        let finished = applied.state.clone();
        self.entries.extend(applied.entries);
        self.emit_terminal_state(&finished);
        self.advance().await;
    }

    /// A required commit failed: the staged transition never happened.
    /// Fail the operation visibly from its last durable state; never
    /// continue as if durability succeeded (DESIGN.md §26.2).
    async fn fail_operation_on_persistence(&mut self, err: StoreError) {
        let operation = self
            .operation
            .as_ref()
            .map(|active| active.machine.operation_id());
        let Some(operation_id) = operation else {
            error!(session = %self.session_id, %err, "persistence failed with no active operation");
            return;
        };
        error!(
            %operation_id,
            %err,
            "durable commit failed; failing the operation from its last checkpoint"
        );
        let was_finished = self
            .operation
            .as_ref()
            .is_some_and(|active| matches!(active.machine.state(), OperationState::Finished(_)));
        if was_finished {
            // The failed write was the terminal checkpoint itself; the
            // durable operation stays open and recoverable.
            self.operation.take();
            self.emit(RuntimeEvent::OperationFailed {
                cursor: RuntimeCursor::default(),
                operation_id,
                message: format!("persistence failed: {err}"),
            });
            return;
        }
        // Fail the operation durably from its last committed checkpoint.
        let (applied, request) = {
            let active = self.operation.as_mut().expect("operation present");
            let applied = active
                .machine
                .apply(Transition::FailOperation {
                    message: format!("persistence failed: {err}"),
                })
                .expect("fail the operation from an open state");
            let request = build_commit_request(
                self.session_id,
                operation_id,
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &active.machine,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            (applied, request)
        };
        match self.store.commit(request).await {
            Ok(()) => {}
            Err(second) => {
                error!(%operation_id, %second, "terminal checkpoint also failed; durable operation stays open");
            }
        }
        let active = self.operation.as_mut().expect("operation present");
        active.state_seq += 1;
        active.cancel.cancel();
        let finished = applied.state.clone();
        self.entries.extend(applied.entries);
        self.emit_terminal_state(&finished);
        self.operation.take();
    }

    fn entry_record(&mut self, entry: &SessionEntry) -> EntryRecord {
        let seq = self.next_entry_seq;
        self.next_entry_seq += 1;
        EntryRecord {
            seq,
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
    /// the open operation durably, signal owned effects, wait for them,
    /// drain, exit. Close is never a user cancellation.
    async fn close_internal(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(active) = &mut self.operation {
            let mut staged = active.machine.clone();
            staged
                .apply(Transition::Suspend)
                .expect("suspend from an open operation");
            let request = build_commit_request(
                self.session_id,
                staged.operation_id(),
                active.state_seq + 1,
                &mut self.next_entry_seq,
                &staged,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            match self.store.commit(request).await {
                Ok(()) => {
                    active.machine = staged;
                    active.state_seq += 1;
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
        self.tracker.wait().await;
        while let Ok(signal) = self.engine_rx.try_recv() {
            debug!(?signal, "dropping engine signal after session close");
        }
        while let Ok(result) = self.tool_rx.try_recv() {
            debug!(?result, "dropping tool result after session close");
        }
        self.emit(RuntimeEvent::SessionClosed {
            cursor: RuntimeCursor::default(),
        });
        self.subscribers.clear();
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

/// Build the durable record of one staged transition. Entry sequences are
/// assigned here so a commit owns its exact seq range.
#[allow(clippy::too_many_arguments)]
fn build_commit_request(
    session_id: SessionId,
    operation_id: OperationId,
    state_seq: u64,
    next_entry_seq: &mut u64,
    staged: &OperationMachine,
    entries: Vec<SessionEntry>,
    open_effects: Vec<EffectRecord>,
    settled_effects: Vec<EffectId>,
    inbox: Vec<InboxRecord>,
    inbox_applied: Vec<InboxId>,
) -> CommitRequest {
    let entries = entries
        .into_iter()
        .map(|entry| {
            let seq = *next_entry_seq;
            *next_entry_seq += 1;
            EntryRecord { seq, entry }
        })
        .collect();
    CommitRequest {
        session_id,
        operation_id,
        checkpoint: CheckpointRecord {
            state_seq,
            payload: CheckpointPayload {
                state: staged.state().clone(),
                cancel_requested: staged.cancel_requested(),
            },
        },
        entries,
        open_effects,
        settled_effects,
        inbox,
        inbox_applied,
    }
}

fn persistence_command_error(err: StoreError) -> CommandError {
    CommandError::Persistence(err.to_string())
}

fn signal_operation_id(signal: &EngineSignal) -> OperationId {
    match signal {
        EngineSignal::TextDelta { operation_id, .. }
        | EngineSignal::ToolCallCompleted { operation_id, .. }
        | EngineSignal::Completed { operation_id }
        | EngineSignal::Failed { operation_id, .. }
        | EngineSignal::Cancelled { operation_id }
        | EngineSignal::ProviderExited { operation_id, .. } => *operation_id,
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
