//! Live runtime controller and handle.

use std::fmt;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, info, warn};

use crate::error::{CommandError, RuntimeError};
use crate::ids::{AgentId, RuntimeCursor, TurnId};
use crate::provider::{EngineSignal, Provider, ProviderRequest};
use crate::tool::{ToolCall, ToolRegistry, ToolResult};

const COMMAND_CAPACITY: usize = 32;
const ENGINE_CAPACITY: usize = 64;
const TOOL_CAPACITY: usize = 32;
const SUBSCRIBER_CAPACITY: usize = 64;

type EventReceiver = mpsc::Receiver<Result<RuntimeEvent, RuntimeError>>;
type SubscribeReply = Result<(RuntimeSnapshot, EventReceiver), CommandError>;

enum RuntimeCommand {
    Submit {
        prompt: String,
        reply: oneshot::Sender<Result<TurnId, CommandError>>,
    },
    CancelTurn {
        turn_id: TurnId,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    Subscribe {
        after: Option<RuntimeCursor>,
        reply: oneshot::Sender<SubscribeReply>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    RuntimeStarted {
        cursor: RuntimeCursor,
    },
    TurnStarted {
        cursor: RuntimeCursor,
        turn_id: TurnId,
        agent_id: AgentId,
        prompt: String,
    },
    AssistantTextDelta {
        cursor: RuntimeCursor,
        turn_id: TurnId,
        text: String,
    },
    TurnFinished {
        cursor: RuntimeCursor,
        turn_id: TurnId,
    },
    TurnCancelled {
        cursor: RuntimeCursor,
        turn_id: TurnId,
    },
    TurnFailed {
        cursor: RuntimeCursor,
        turn_id: TurnId,
        message: String,
    },
    RuntimeShutdown {
        cursor: RuntimeCursor,
    },
}

impl RuntimeEvent {
    #[must_use]
    pub const fn cursor(&self) -> RuntimeCursor {
        match self {
            Self::RuntimeStarted { cursor }
            | Self::TurnStarted { cursor, .. }
            | Self::AssistantTextDelta { cursor, .. }
            | Self::TurnFinished { cursor, .. }
            | Self::TurnCancelled { cursor, .. }
            | Self::TurnFailed { cursor, .. }
            | Self::RuntimeShutdown { cursor } => *cursor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnStatus {
    Idle,
    Running { turn_id: TurnId, prompt: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub cursor: RuntimeCursor,
    pub turn: TurnStatus,
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

#[derive(Clone)]
pub struct RuntimeHandle {
    tx: mpsc::Sender<RuntimeCommand>,
}

impl fmt::Debug for RuntimeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeHandle").finish_non_exhaustive()
    }
}

impl RuntimeHandle {
    pub async fn submit(&self, prompt: impl Into<String>) -> Result<TurnId, CommandError> {
        self.request(|reply| RuntimeCommand::Submit {
            prompt: prompt.into(),
            reply,
        })
        .await
    }

    pub async fn cancel(&self, turn_id: TurnId) -> Result<(), CommandError> {
        self.request(|reply| RuntimeCommand::CancelTurn { turn_id, reply })
            .await
    }

    pub async fn subscribe(&self) -> Result<(RuntimeSnapshot, EventSubscription), CommandError> {
        self.subscribe_after(None).await
    }

    pub async fn subscribe_after(
        &self,
        after: Option<RuntimeCursor>,
    ) -> Result<(RuntimeSnapshot, EventSubscription), CommandError> {
        let (snapshot, rx) = self
            .request(|reply| RuntimeCommand::Subscribe { after, reply })
            .await?;
        Ok((snapshot, EventSubscription { rx }))
    }

    pub async fn shutdown(&self) -> Result<(), CommandError> {
        self.request(|reply| RuntimeCommand::Shutdown { reply })
            .await
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, CommandError>>) -> RuntimeCommand,
    ) -> Result<T, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx.try_send(build(reply)).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
            mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
        })?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }
}

pub struct Runtime {
    handle: RuntimeHandle,
    join: JoinHandle<()>,
}

impl Runtime {
    #[must_use]
    pub fn start(provider: impl Provider, tools: ToolRegistry) -> Self {
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let handle = RuntimeHandle { tx };
        let join = tokio::spawn(async move {
            Controller::new(Arc::new(provider), Arc::new(tools), rx)
                .run()
                .await;
        });
        Self { handle, join }
    }

    #[must_use]
    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    pub async fn join(self) -> Result<(), RuntimeError> {
        self.join
            .await
            .map_err(|_| RuntimeError::TurnFailed("runtime task panicked".to_owned()))
    }
}

pub struct PrintFrontend<W> {
    writer: W,
}

impl<W: std::io::Write> PrintFrontend<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub async fn run(
        &mut self,
        handle: &RuntimeHandle,
        prompt: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let (_snapshot, mut events) = handle.subscribe().await?;
        handle.submit(prompt).await?;
        loop {
            match events.recv().await? {
                RuntimeEvent::AssistantTextDelta { text, .. } => {
                    self.writer
                        .write_all(text.as_bytes())
                        .map_err(|err| RuntimeError::TurnFailed(err.to_string()))?;
                    self.writer
                        .flush()
                        .map_err(|err| RuntimeError::TurnFailed(err.to_string()))?;
                }
                RuntimeEvent::TurnFinished { .. } => return Ok(()),
                RuntimeEvent::TurnCancelled { .. } => return Err(RuntimeError::TurnCancelled),
                RuntimeEvent::TurnFailed { message, .. } => {
                    return Err(RuntimeError::TurnFailed(message));
                }
                RuntimeEvent::RuntimeShutdown { .. } => {
                    return Err(RuntimeError::Command(CommandError::Closed));
                }
                RuntimeEvent::RuntimeStarted { .. } | RuntimeEvent::TurnStarted { .. } => {}
            }
        }
    }
}

struct Subscriber {
    tx: mpsc::Sender<Result<RuntimeEvent, RuntimeError>>,
}

enum TurnState {
    Idle,
    Running {
        turn_id: TurnId,
        prompt: String,
        cancel: CancellationToken,
        /// Sender for tool results back to the running provider task.
        tool_tx: mpsc::Sender<ToolResult>,
    },
}

struct Controller<P> {
    provider: Arc<P>,
    tools: Arc<ToolRegistry>,
    commands: mpsc::Receiver<RuntimeCommand>,
    engine_tx: mpsc::Sender<EngineSignal>,
    engine_rx: mpsc::Receiver<EngineSignal>,
    cancel_root: CancellationToken,
    tracker: TaskTracker,
    cursor: RuntimeCursor,
    events: Vec<RuntimeEvent>,
    next_turn: u64,
    turn: TurnState,
    subscribers: Vec<Subscriber>,
    shutting_down: bool,
}

impl<P: Provider> Controller<P> {
    fn new(
        provider: Arc<P>,
        tools: Arc<ToolRegistry>,
        commands: mpsc::Receiver<RuntimeCommand>,
    ) -> Self {
        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let mut controller = Self {
            provider,
            tools,
            commands,
            engine_tx,
            engine_rx,
            cancel_root: CancellationToken::new(),
            tracker: TaskTracker::new(),
            cursor: RuntimeCursor::default(),
            events: Vec::new(),
            next_turn: 1,
            turn: TurnState::Idle,
            subscribers: Vec::new(),
            shutting_down: false,
        };
        controller.emit(RuntimeEvent::RuntimeStarted {
            cursor: RuntimeCursor::default(),
        });
        controller
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        self.shutdown_internal().await;
                        break;
                    };
                    if self.handle_command(command).await {
                        break;
                    }
                }
                signal = self.engine_rx.recv() => {
                    if let Some(signal) = signal {
                        self.handle_engine(signal);
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, command: RuntimeCommand) -> bool {
        match command {
            RuntimeCommand::Submit { prompt, reply } => {
                let _ = reply.send(self.submit(prompt));
                false
            }
            RuntimeCommand::CancelTurn { turn_id, reply } => {
                let _ = reply.send(self.cancel_turn(turn_id));
                false
            }
            RuntimeCommand::Subscribe { after, reply } => {
                let _ = reply.send(self.subscribe(after));
                false
            }
            RuntimeCommand::Shutdown { reply } => {
                self.shutdown_internal().await;
                let _ = reply.send(Ok(()));
                true
            }
        }
    }

    fn submit(&mut self, prompt: String) -> Result<TurnId, CommandError> {
        if self.shutting_down {
            return Err(CommandError::Closed);
        }
        if let TurnState::Running { turn_id, .. } = self.turn {
            return Err(CommandError::Busy { turn_id });
        }

        let turn_id = TurnId::new(self.next_turn);
        self.next_turn += 1;
        let cancel = self.cancel_root.child_token();
        let (tool_tx, tool_rx) = mpsc::channel(TOOL_CAPACITY);
        self.turn = TurnState::Running {
            turn_id,
            prompt: prompt.clone(),
            cancel: cancel.clone(),
            tool_tx: tool_tx.clone(),
        };
        self.emit(RuntimeEvent::TurnStarted {
            cursor: RuntimeCursor::default(),
            turn_id,
            agent_id: AgentId::ROOT,
            prompt: prompt.clone(),
        });

        let provider = Arc::clone(&self.provider);
        let tools = self.tools.clone();
        let request = ProviderRequest {
            turn_id,
            prompt,
            tools: tools.specs(),
        };
        let out = self.engine_tx.clone();
        self.tracker.spawn(async move {
            provider.run(request, cancel, out.clone(), tool_rx).await;
            let _ = out.send(EngineSignal::ProviderExited { turn_id }).await;
        });
        Ok(turn_id)
    }

    fn handle_engine(&mut self, signal: EngineSignal) {
        let Some(active) = (match &self.turn {
            TurnState::Running { turn_id, .. } => Some(*turn_id),
            TurnState::Idle => None,
        }) else {
            debug!("ignored engine signal with no running turn");
            return;
        };
        match signal {
            EngineSignal::TextDelta { turn_id, text } if turn_id == active => {
                self.emit(RuntimeEvent::AssistantTextDelta {
                    cursor: RuntimeCursor::default(),
                    turn_id,
                    text,
                });
            }
            EngineSignal::ToolCallRequest { turn_id, call } if turn_id == active => {
                let (tool_tx, cancel) = match &self.turn {
                    TurnState::Running {
                        tool_tx, cancel, ..
                    } => (tool_tx.clone(), cancel.child_token()),
                    TurnState::Idle => return,
                };
                self.spawn_tool_call(tool_tx, cancel, call);
            }
            EngineSignal::Completed { turn_id } if turn_id == active => {
                self.finish_turn(turn_id);
            }
            EngineSignal::Cancelled { turn_id } if turn_id == active => {
                self.cancel_settled(turn_id);
            }
            EngineSignal::Failed { turn_id, message } if turn_id == active => {
                self.fail_turn(turn_id, message);
            }
            EngineSignal::ProviderExited { turn_id } if turn_id == active => {
                self.fail_turn(
                    turn_id,
                    "provider exited without a terminal signal".to_owned(),
                );
            }
            _ => debug!(?signal, "ignored stale engine signal"),
        }
    }

    /// Spawn a tracked, cancellation-aware task that executes one tool and
    /// feeds its result back to the running provider. The controller loop
    /// only dispatches here; it never awaits tool I/O.
    fn spawn_tool_call(
        &self,
        tool_tx: mpsc::Sender<ToolResult>,
        cancel: CancellationToken,
        call: ToolCall,
    ) {
        let ToolCall {
            turn_id,
            call_id,
            name,
            arguments,
        } = call;
        let tools = self.tools.clone();
        debug!(turn = %turn_id, %call_id, tool = %name, "dispatching tool call");
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

    fn cancel_turn(&mut self, turn_id: TurnId) -> Result<(), CommandError> {
        if self.shutting_down {
            return Err(CommandError::Closed);
        }
        match &self.turn {
            TurnState::Running {
                turn_id: active,
                cancel,
                ..
            } if *active == turn_id => {
                cancel.cancel();
                Ok(())
            }
            TurnState::Running { .. } => Err(CommandError::TurnNotActive { turn_id }),
            TurnState::Idle => Err(CommandError::NoActiveTurn),
        }
    }

    fn subscribe(&mut self, after: Option<RuntimeCursor>) -> SubscribeReply {
        if self.shutting_down {
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

    fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            cursor: self.cursor,
            turn: match &self.turn {
                TurnState::Idle => TurnStatus::Idle,
                TurnState::Running {
                    turn_id, prompt, ..
                } => TurnStatus::Running {
                    turn_id: *turn_id,
                    prompt: prompt.clone(),
                },
            },
        }
    }

    fn finish_turn(&mut self, turn_id: TurnId) {
        self.turn = TurnState::Idle;
        self.emit(RuntimeEvent::TurnFinished {
            cursor: RuntimeCursor::default(),
            turn_id,
        });
    }

    fn cancel_settled(&mut self, turn_id: TurnId) {
        self.turn = TurnState::Idle;
        self.emit(RuntimeEvent::TurnCancelled {
            cursor: RuntimeCursor::default(),
            turn_id,
        });
    }

    fn fail_turn(&mut self, turn_id: TurnId, message: String) {
        self.turn = TurnState::Idle;
        self.emit(RuntimeEvent::TurnFailed {
            cursor: RuntimeCursor::default(),
            turn_id,
            message,
        });
    }

    fn emit(&mut self, mut event: RuntimeEvent) {
        self.cursor = self.cursor.next();
        set_cursor(&mut event, self.cursor);
        match &event {
            RuntimeEvent::AssistantTextDelta { .. } => {
                debug!(cursor = %self.cursor, "assistant text delta");
            }
            other => {
                info!(cursor = %self.cursor, event = event_kind(other), "runtime event");
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

    async fn shutdown_internal(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.cancel_root.cancel();
        self.tracker.close();
        self.tracker.wait().await;
        while let Ok(signal) = self.engine_rx.try_recv() {
            self.handle_engine(signal);
        }
        if let TurnState::Running { turn_id, .. } = self.turn {
            self.cancel_settled(turn_id);
        }
        self.emit(RuntimeEvent::RuntimeShutdown {
            cursor: RuntimeCursor::default(),
        });
        self.subscribers.clear();
    }
}

fn set_cursor(event: &mut RuntimeEvent, cursor: RuntimeCursor) {
    match event {
        RuntimeEvent::RuntimeStarted { cursor: slot }
        | RuntimeEvent::TurnStarted { cursor: slot, .. }
        | RuntimeEvent::AssistantTextDelta { cursor: slot, .. }
        | RuntimeEvent::TurnFinished { cursor: slot, .. }
        | RuntimeEvent::TurnCancelled { cursor: slot, .. }
        | RuntimeEvent::TurnFailed { cursor: slot, .. }
        | RuntimeEvent::RuntimeShutdown { cursor: slot } => *slot = cursor,
    }
}

fn event_kind(event: &RuntimeEvent) -> &'static str {
    match event {
        RuntimeEvent::RuntimeStarted { .. } => "runtime_started",
        RuntimeEvent::TurnStarted { .. } => "turn_started",
        RuntimeEvent::AssistantTextDelta { .. } => "assistant_text_delta",
        RuntimeEvent::TurnFinished { .. } => "turn_finished",
        RuntimeEvent::TurnCancelled { .. } => "turn_cancelled",
        RuntimeEvent::TurnFailed { .. } => "turn_failed",
        RuntimeEvent::RuntimeShutdown { .. } => "runtime_shutdown",
    }
}

#[cfg(test)]
pub(crate) struct SaturatedHandle {
    handle: RuntimeHandle,
    _rx: mpsc::Receiver<RuntimeCommand>,
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
            .try_send(RuntimeCommand::Submit {
                prompt: String::from("fill"),
                reply,
            })
            .map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
                mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
            })
    }
}
