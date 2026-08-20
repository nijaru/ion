//! Process `Runtime`, single-writer `SessionRuntime`, and frontends
//! (DESIGN.md §4, §7, §8, §21).
//!
//! The process-level `Runtime` owns composition and the session
//! registry; one loaded session has exactly one mutation authority, its
//! `SessionRuntime` task. Effect tasks (provider steps, tool calls) run
//! outside the mutation line; their outcomes re-enter as transitions.

use std::fmt;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, info, warn};

use crate::error::{CommandError, RuntimeError};
use crate::ids::{OperationId, RuntimeCursor, SessionId};
use crate::provider::{EngineSignal, Provider, ProviderRequest};
use crate::session::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition,
};
use crate::tool::{ToolCall, ToolRegistry, ToolResult};

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
        after: Option<RuntimeCursor>,
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
/// the transition authority accepted the command.
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
    /// Accept a prompt and open a new operation when idle.
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
    /// (DESIGN.md §9.4). Acknowledgment means the request is recorded;
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
            .try_send(SessionCommand::Subscribe { after: None, reply })
            .map_err(command_send_error)?;
        let (snapshot, _events) = rx.await.map_err(|_| CommandError::RuntimeDropped)??;
        Ok(snapshot)
    }

    pub async fn subscribe(&self) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        self.subscribe_after(None).await
    }

    pub async fn subscribe_after(
        &self,
        after: Option<RuntimeCursor>,
    ) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Subscribe { after, reply })
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
/// exactly one loaded session (DESIGN.md §32 Step 1); multi-session
/// creation arrives with the durable store (§32 Step 2).
pub struct Runtime {
    handle: RuntimeHandle,
    session: SessionHandle,
    join: JoinHandle<()>,
}

impl Runtime {
    /// Compose the runtime with one session backed by `provider` and
    /// `tools`.
    #[must_use]
    pub fn start(provider: impl Provider, tools: ToolRegistry) -> Self {
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let handle = RuntimeHandle { tx: tx.clone() };
        let session = SessionHandle { tx };
        let provider = Arc::new(provider);
        let tools = Arc::new(tools);
        let join = tokio::spawn(async move {
            SessionRuntime::new(SessionId::FIRST, provider, tools, rx)
                .run()
                .await;
        });
        Self {
            handle,
            session,
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

    pub async fn join(self) -> Result<(), RuntimeError> {
        self.join
            .await
            .map_err(|_| RuntimeError::OperationFailed("runtime task panicked".to_owned()))
    }
}

struct Subscriber {
    tx: mpsc::Sender<Result<RuntimeEvent, RuntimeError>>,
}

/// The single-writer owner of one loaded session's mutable live state
/// (DESIGN.md §4.3): bounded mailbox, the active operation machine, the
/// session entry log, pending input state, snapshots, and its owned
/// effect tasks. It never awaits provider or tool I/O on its mutation
/// line; effects run as spawned tasks and re-enter as transitions.
struct SessionRuntime<P> {
    session_id: SessionId,
    provider: Arc<P>,
    tools: Arc<ToolRegistry>,
    commands: mpsc::Receiver<SessionCommand>,
    engine_tx: mpsc::Sender<EngineSignal>,
    engine_rx: mpsc::Receiver<EngineSignal>,
    tool_tx: mpsc::Sender<ToolResult>,
    tool_rx: mpsc::Receiver<ToolResult>,
    cancel_root: CancellationToken,
    tracker: TaskTracker,
    cursor: RuntimeCursor,
    events: Vec<RuntimeEvent>,
    /// Canonical semantic session log until the store takes over.
    entries: Vec<SessionEntry>,
    next_operation: u64,
    operation: Option<(OperationMachine, CancellationToken)>,
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
        provider: Arc<P>,
        tools: Arc<ToolRegistry>,
        commands: mpsc::Receiver<SessionCommand>,
    ) -> Self {
        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        Self {
            session_id,
            provider,
            tools,
            commands,
            engine_tx,
            engine_rx,
            tool_tx,
            tool_rx,
            cancel_root: CancellationToken::new(),
            tracker: TaskTracker::new(),
            cursor: RuntimeCursor::default(),
            events: Vec::new(),
            entries: Vec::new(),
            next_operation: 1,
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
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    if self.handle_command(command) {
                        break;
                    }
                }
                signal = self.engine_rx.recv() => {
                    if let Some(signal) = signal {
                        self.handle_engine(signal);
                    }
                }
                result = self.tool_rx.recv() => {
                    if let Some(result) = result {
                        self.handle_tool_result(result);
                    }
                }
            }
        }
        self.close_internal().await;
    }

    /// Returns true when the session loop must exit.
    fn handle_command(&mut self, command: SessionCommand) -> bool {
        match command {
            SessionCommand::Submit { prompt, reply } => {
                let _ = reply.send(self.submit(prompt));
                false
            }
            SessionCommand::Steer { text, reply } => {
                let _ = reply.send(self.enqueue_inbox(InboxKind::Steer, text));
                false
            }
            SessionCommand::FollowUp { text, reply } => {
                let _ = reply.send(self.enqueue_inbox(InboxKind::FollowUp, text));
                false
            }
            SessionCommand::Cancel {
                operation_id,
                reply,
            } => {
                let _ = reply.send(self.cancel(operation_id));
                false
            }
            SessionCommand::Subscribe { after, reply } => {
                let _ = reply.send(self.subscribe(after));
                false
            }
            SessionCommand::Close { reply } => {
                let _ = reply.send(Ok(()));
                true
            }
        }
    }

    fn submit(&mut self, prompt: String) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if let Some((machine, _)) = &self.operation {
            return Err(CommandError::Busy {
                operation_id: machine.operation_id(),
            });
        }
        let operation_id = OperationId::new(self.next_operation);
        self.next_operation += 1;
        let cancel = self.cancel_root.child_token();
        let (machine, applied) =
            OperationMachine::accept(operation_id, prompt.clone(), self.tools.specs());
        self.operation = Some((machine, cancel));
        self.draft_text.clear();
        self.draft_calls.clear();
        self.commit(&applied);
        self.emit(RuntimeEvent::OperationStarted {
            cursor: RuntimeCursor::default(),
            operation_id,
            prompt,
        });
        self.advance();
        Ok(operation_id)
    }

    fn enqueue_inbox(&mut self, kind: InboxKind, text: String) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let Some((machine, _)) = &mut self.operation else {
            return Err(CommandError::NoActiveOperation);
        };
        let applied = machine
            .apply(Transition::ApplyInbox {
                item: InboxItem { kind, text },
            })
            .expect("inbox apply from an active operation");
        self.commit(&applied);
        self.advance();
        Ok(())
    }

    /// Request semantic cancellation (DESIGN.md §9.4): record the
    /// request first, then signal descendant effects.
    fn cancel(&mut self, operation_id: OperationId) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let Some((machine, cancel)) = &mut self.operation else {
            return Err(CommandError::NoActiveOperation);
        };
        if machine.operation_id() != operation_id {
            return Err(CommandError::NotActive { operation_id });
        }
        let applied = machine
            .apply(Transition::CancelRequested)
            .expect("cancel request from an active operation");
        cancel.cancel();
        self.commit(&applied);
        Ok(())
    }

    /// Drive the machine forward from quiescent states: drain queued
    /// inbox items at the continuation boundary, then start the next
    /// model step or admit the next planned tool. Returns after spawning
    /// an effect; outcomes re-enter through the engine/tool channels.
    fn advance(&mut self) {
        loop {
            let Some((machine, _)) = &mut self.operation else {
                return;
            };
            match machine.state() {
                OperationState::Finished(_) => {
                    self.operation.take();
                    return;
                }
                OperationState::Accepted
                | OperationState::NeedAssistant
                | OperationState::NeedContinuation => {
                    if machine.has_queued_inbox() {
                        let drained = machine
                            .drain_inbox()
                            .expect("inbox drain at a continuation boundary");
                        for applied in drained {
                            self.commit(&applied);
                        }
                        continue;
                    }
                    assert!(
                        !matches!(machine.state(), OperationState::NeedContinuation),
                        "NeedContinuation without queued inbox is impossible state"
                    );
                    let applied = machine
                        .apply(Transition::StartModelStep)
                        .expect("start model step from a quiescent state");
                    self.spawn_intents(&applied);
                    self.commit(&applied);
                    return;
                }
                OperationState::ToolsPlanned { .. } => {
                    let applied = machine
                        .apply(Transition::AdmitNextTool)
                        .expect("admit next tool from ToolsPlanned");
                    self.spawn_intents(&applied);
                    self.commit(&applied);
                    return;
                }
                _ => return,
            }
        }
    }

    fn spawn_intents(&mut self, applied: &Applied) {
        let operation_id = self
            .operation
            .as_ref()
            .map(|(machine, _)| machine.operation_id());
        let Some(operation_id) = operation_id else {
            return;
        };
        for intent in &applied.intents {
            match intent {
                EffectIntent::ModelStep {
                    operation_id: id,
                    prompt,
                    tools,
                } => {
                    let provider = Arc::clone(&self.provider);
                    let cancel = self
                        .operation
                        .as_ref()
                        .map(|(_, token)| token.child_token())
                        .unwrap_or_else(|| self.cancel_root.child_token());
                    let out = self.engine_tx.clone();
                    debug!(%operation_id, "starting model step effect");
                    let id = *id;
                    let request = ProviderRequest {
                        operation_id: id,
                        prompt: prompt.clone(),
                        tools: tools.clone(),
                    };
                    self.step_terminal_seen = false;
                    self.model_step += 1;
                    let step = self.model_step;
                    let terminal = self.engine_tx.clone();
                    self.tracker.spawn(async move {
                        provider.run(request, cancel, out.clone()).await;
                        let _ = terminal
                            .send(EngineSignal::ProviderExited {
                                operation_id: id,
                                step,
                            })
                            .await;
                    });
                }
                EffectIntent::Tool { call } => {
                    let ToolCall {
                        call_id,
                        name,
                        arguments,
                        ..
                    } = call;
                    let call_id = *call_id;
                    // Canonicalization and schema validation happen before
                    // the effect starts; a denial becomes a model-visible
                    // tool result (DESIGN.md §17.3, §16.5).
                    if let Err(message) = self.tools.validate(name, arguments) {
                        self.tool_tx
                            .try_send(ToolResult::Err {
                                call_id,
                                error: message,
                            })
                            .expect("tool outcome channel closed while dispatching denial");
                        continue;
                    }
                    self.emit(RuntimeEvent::ToolStarted {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        call_id,
                        tool: name.clone(),
                    });
                    let tools = Arc::clone(&self.tools);
                    let cancel = self
                        .operation
                        .as_ref()
                        .map(|(_, token)| token.child_token())
                        .unwrap_or_else(|| self.cancel_root.child_token());
                    let tool_tx = self.tool_tx.clone();
                    let name = name.clone();
                    let arguments = arguments.clone();
                    debug!(%operation_id, %call_id, tool = %name, "dispatching tool effect");
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
            }
        }
    }

    fn handle_engine(&mut self, signal: EngineSignal) {
        let operation_id = match &self.operation {
            Some((machine, _)) => machine.operation_id(),
            None => {
                debug!(?signal, "ignored engine signal with no active operation");
                return;
            }
        };
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
                let applied = self
                    .operation
                    .as_mut()
                    .and_then(|(machine, _)| {
                        machine
                            .apply(Transition::ProviderCompleted { text, tool_calls })
                            .ok()
                    })
                    .expect("provider completion while AssistantEffectPending");
                self.commit(&applied);
                self.advance();
            }
            EngineSignal::Failed { message, .. } => {
                let cancel_requested = self
                    .operation
                    .as_ref()
                    .is_some_and(|(machine, _)| machine.cancel_requested());
                let transition = if cancel_requested {
                    Transition::ProviderCancelled
                } else {
                    Transition::ProviderFailed { message }
                };
                self.step_terminal_seen = true;
                let applied = self
                    .operation
                    .as_mut()
                    .and_then(|(machine, _)| machine.apply(transition).ok())
                    .expect("provider failure while AssistantEffectPending");
                self.commit(&applied);
                self.advance();
            }
            EngineSignal::Cancelled { .. } => {
                self.step_terminal_seen = true;
                let applied = self
                    .operation
                    .as_mut()
                    .and_then(|(machine, _)| machine.apply(Transition::ProviderCancelled).ok())
                    .expect("provider cancellation while AssistantEffectPending");
                self.commit(&applied);
                self.advance();
            }
            EngineSignal::ProviderExited { step, .. } => {
                if self.step_terminal_seen || step != self.model_step {
                    debug!(?signal, "ignored stale provider exit sentinel");
                    return;
                }
                let applied = self
                    .operation
                    .as_mut()
                    .and_then(|(machine, _)| {
                        machine
                            .apply(Transition::ProviderFailed {
                                message: "provider exited without a terminal signal".to_owned(),
                            })
                            .ok()
                    })
                    .expect("provider exit while AssistantEffectPending");
                self.commit(&applied);
                self.advance();
            }
        }
    }

    fn handle_tool_result(&mut self, result: ToolResult) {
        let Some((machine, _)) = &mut self.operation else {
            debug!("ignored tool result with no active operation");
            return;
        };
        let applied = machine
            .apply(Transition::ToolSettled { result })
            .expect("tool settlement while ToolEffectPending");
        self.commit(&applied);
        self.advance();
    }

    /// Append transition output to the session log and derive terminal
    /// live events.
    fn commit(&mut self, applied: &Applied) {
        self.entries.extend(applied.entries.iter().cloned());
        if let OperationState::Finished(outcome) = &applied.state {
            let Some((machine, _)) = &self.operation else {
                return;
            };
            let operation_id = machine.operation_id();
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

    fn subscribe(&mut self, after: Option<RuntimeCursor>) -> SubscribeReply {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        let snapshot = self.snapshot();
        if let Some(after) = after {
            for event in &self.events {
                if event.cursor() > after {
                    match tx.try_send(Ok(event.clone())) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            let _ = tx.try_send(Err(RuntimeError::SubscriptionLagged));
                            return Ok((snapshot, rx));
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            return Ok((snapshot, rx));
                        }
                    }
                }
            }
        }
        self.subscribers.push(Subscriber { tx });
        Ok((snapshot, rx))
    }

    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            cursor: self.cursor,
            operation: match &self.operation {
                None => OperationStatus::Idle,
                Some((machine, _)) => OperationStatus::Active {
                    operation_id: machine.operation_id(),
                    prompt: machine.prompt().to_owned(),
                    state: machine.state().clone(),
                },
            },
            entries: self.entries.clone(),
        }
    }

    /// Session close (DESIGN.md §9.5, §25): stop accepting work, signal
    /// owned effects, wait for them, drain, exit. An open operation is
    /// suspended — never recorded as a user cancellation.
    async fn close_internal(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some((machine, cancel)) = self.operation.as_mut() {
            let applied = machine
                .apply(Transition::Suspend)
                .expect("suspend from an open operation");
            cancel.cancel();
            self.commit(&applied);
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
        self.events.push(event.clone());
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

/// Print frontend: projects session events to a writer (DESIGN.md §21.1).
pub struct PrintFrontend<W> {
    writer: W,
}

impl<W: std::io::Write> PrintFrontend<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub async fn run(
        &mut self,
        session: &SessionHandle,
        prompt: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let (_snapshot, mut events) = session.subscribe().await?;
        session.submit(prompt).await?;
        loop {
            match events.recv().await? {
                RuntimeEvent::AssistantTextDelta { text, .. } => {
                    self.writer
                        .write_all(text.as_bytes())
                        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
                    self.writer
                        .flush()
                        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
                }
                RuntimeEvent::OperationFinished { .. } => return Ok(()),
                RuntimeEvent::OperationCancelled { .. } => {
                    return Err(RuntimeError::OperationCancelled);
                }
                RuntimeEvent::OperationFailed { message, .. } => {
                    return Err(RuntimeError::OperationFailed(message));
                }
                RuntimeEvent::SessionClosed { .. } => {
                    return Err(RuntimeError::Command(CommandError::Closed));
                }
                RuntimeEvent::OperationStarted { .. } | RuntimeEvent::ToolStarted { .. } => {}
            }
        }
    }
}
