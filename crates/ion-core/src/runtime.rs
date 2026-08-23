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
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

use crate::context::{ContextPlan, project};
use crate::error::{CommandError, RuntimeError};
use crate::ids::{EffectId, InboxId, OperationId, RuntimeCursor, SessionId};
use crate::policy::{DefaultPolicy, PolicyDecision, PolicyEngine};
use crate::provider::{EngineSignal, ModelConfig, Provider, ProviderRequest, TokenUsage};
use crate::session::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    InboxStatus, LoadedSession, SessionRecord, SessionStore, SettledEffect, StoreError,
    UsageRecord,
};
use crate::tool::{RecoveryClass, ToolCall, ToolCatalog, ToolResult, ToolSpec};

const COMMAND_CAPACITY: usize = 32;
const ENGINE_CAPACITY: usize = 64;
/// Broadcast buffer per subscriber (§21.4): a slow UI never blocks or
/// grows the runtime; overflow surfaces as a reliable lag error.
const SUBSCRIBER_CAPACITY: usize = 64;
type SubscribeReply = Result<(SessionSnapshot, EventSubscription), CommandError>;
type ToolSettlement = (EffectId, ToolResult);

/// One-line display summary of a call's canonical target (best
/// effort; None when canonicalization fails — the denial surfaces
/// elsewhere).
fn target_summary(
    tools: &ToolCatalog,
    name: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    match tools.canonicalize(name, arguments) {
        Ok(crate::tool::CanonicalTarget::Path { path }) => Some(path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )),
        Ok(crate::tool::CanonicalTarget::Command { command }) => Some(command),
        Ok(crate::tool::CanonicalTarget::Remote { tool }) => Some(tool),
        Err(_) => None,
    }
}

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
    /// Streamed reasoning text, display-only. Never persisted: thinking
    /// never becomes assistant content (partial model output rule).
    ThinkingDelta {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        text: String,
    },
    ToolStarted {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        call_id: u64,
        tool: String,
        /// Short canonical-target summary for display (path or command).
        target: Option<String>,
    },
    /// A started tool effect settled durably. Emitted after the
    /// settlement checkpoint commits, so subscribers see completion
    /// exactly when it is durable.
    ToolSettled {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        call_id: u64,
        is_error: bool,
        /// Bounded tail of the settled output for frontend rendering.
        preview: Option<String>,
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
    /// Non-interactive policy gate (DESIGN.md §17.4): a concrete action
    /// needed an approval no caller could grant; the operation
    /// terminated durably with `ApprovalRequired`.
    OperationApprovalRequired {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        tool: String,
    },
    SessionClosed {
        cursor: RuntimeCursor,
    },
}

impl RuntimeEvent {
    /// The operation this event belongs to, when it is bound to one.
    #[must_use]
    pub const fn operation_id(&self) -> Option<OperationId> {
        match self {
            Self::OperationStarted { operation_id, .. }
            | Self::AssistantTextDelta { operation_id, .. }
            | Self::ThinkingDelta { operation_id, .. }
            | Self::ToolStarted { operation_id, .. }
            | Self::ToolSettled { operation_id, .. }
            | Self::OperationFinished { operation_id, .. }
            | Self::OperationFailed { operation_id, .. }
            | Self::OperationCancelled { operation_id, .. }
            | Self::OperationApprovalRequired { operation_id, .. } => Some(*operation_id),
            Self::SessionClosed { .. } => None,
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> RuntimeCursor {
        match self {
            Self::OperationStarted { cursor, .. }
            | Self::AssistantTextDelta { cursor, .. }
            | Self::ThinkingDelta { cursor, .. }
            | Self::ToolStarted { cursor, .. }
            | Self::ToolSettled { cursor, .. }
            | Self::OperationFinished { cursor, .. }
            | Self::OperationFailed { cursor, .. }
            | Self::OperationCancelled { cursor, .. }
            | Self::OperationApprovalRequired { cursor, .. }
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
    /// The session's durable model selection; authoritative across
    /// resume (§14.8).
    pub model_ref: String,
    /// Live draft of the active operation (§21.4): present iff an
    /// operation is running. A lagged subscriber reconstructs its view
    /// from this instead of guessing from partial deltas.
    pub live: Option<LiveOperationState>,
}

/// One started-but-unsettled tool call of the live operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTool {
    pub call_id: u64,
    pub tool: String,
    pub target: Option<String>,
}

/// Live, never-durable draft state of the active operation (§21.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOperationState {
    /// Accumulated assistant text of the in-flight step.
    pub draft_text: String,
    /// Display-only reasoning text; MAY be discarded by a frontend.
    pub draft_thinking: String,
    /// Started-but-unsettled tool calls, oldest first.
    pub pending_tools: Vec<PendingTool>,
}

pub struct EventSubscription {
    rx: broadcast::Receiver<RuntimeEvent>,
}

impl EventSubscription {
    pub async fn recv(&mut self) -> Result<RuntimeEvent, RuntimeError> {
        match self.rx.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(_skipped)) => {
                // Reliable by construction: the receiver detects the
                // gap against the ring tail (§21.4).
                Err(RuntimeError::SubscriptionLagged)
            }
            Err(broadcast::error::RecvError::Closed) => Err(RuntimeError::SubscriptionClosed),
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
    /// User-requested compaction: honored at the next continuation
    /// boundary of the active operation. Ok(false) = idle, nothing to
    /// compact (compaction runs within an operation, §14.7).
    Compact {
        instructions: Option<String>,
        reply: oneshot::Sender<Result<bool, CommandError>>,
    },
    SwitchModel {
        model_ref: String,
        reply: oneshot::Sender<Result<String, CommandError>>,
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
    /// Request compaction of the active operation at its next safe
    /// boundary (user-facing /compact; §14.7). Returns false when the
    /// session is idle - compaction runs within an operation.
    pub async fn compact(&self, instructions: Option<String>) -> Result<bool, CommandError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Compact {
                instructions,
                reply: tx,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::Closed)?
    }

    /// Durably select the model used by future model steps. A running
    /// step keeps its frozen model snapshot. Returns the previous id.
    pub async fn switch_model(&self, model_ref: impl Into<String>) -> Result<String, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SwitchModel {
                model_ref: model_ref.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

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
        Ok((snapshot, events))
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

/// Everything one spawned session task is composed from.
struct Composition<P> {
    provider: P,
    tools: ToolCatalog,
    store: SessionStore,
    policy: Arc<dyn PolicyEngine>,
    budget: RuntimeBudget,
    parent: Option<SessionId>,
}

impl<P: Provider> Composition<P> {
    fn new(provider: P, tools: impl Into<ToolCatalog>, store: SessionStore) -> Self {
        Self {
            provider,
            tools: tools.into(),
            store,
            policy: Arc::new(DefaultPolicy),
            budget: RuntimeBudget::unbounded(),
            parent: None,
        }
    }

    fn spawn(self, session_id: SessionId, loaded: Option<LoadedSession>) -> Runtime {
        let initial_model_ref = self.provider.initial_model_ref();
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let handle = RuntimeHandle { tx: tx.clone() };
        let session = SessionHandle { tx };
        let provider = Arc::new(self.provider);
        let tools = Arc::new(self.tools);
        let artifact_root = self.store.artifact_root();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let join = tokio::spawn(async move {
            SessionRuntime::new(
                session_id,
                cwd,
                SessionDeps {
                    provider,
                    initial_model_ref,
                    tools,
                    artifact_root,
                    store: self.store,
                    policy: self.policy,
                    budget: self.budget,
                    parent: self.parent,
                },
                rx,
                loaded,
            )
            .run()
            .await;
        });
        Runtime {
            handle,
            session,
            session_id,
            join,
        }
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
    pub fn start(provider: impl Provider, tools: impl Into<ToolCatalog>) -> Self {
        let store = SessionStore::open(crate::store::default_db_path())
            .expect("open the default session store");
        Self::start_with_store(provider, tools, store)
    }

    /// Compose the runtime with one new durable session in `store`.
    #[must_use]
    pub fn start_with_store(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
    ) -> Self {
        Composition::new(provider, tools, store).spawn(SessionId::generate(), None)
    }

    /// Compose the runtime with an explicit approval policy (DESIGN.md
    /// §17): the documented mechanism for callers that can grant
    /// actions non-interactively.
    #[must_use]
    pub fn start_with_policy(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        policy: Arc<dyn PolicyEngine>,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.spawn(SessionId::generate(), None)
    }

    /// Compose a bounded child session with durable lineage (§20.1,
    /// §20.3): the same primitive as a root session - own machine, own
    /// store record - plus a persisted parent reference.
    #[must_use]
    pub fn start_child(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        policy: Arc<dyn PolicyEngine>,
        budget: RuntimeBudget,
        parent: SessionId,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.budget = budget;
        composition.parent = Some(parent);
        composition.spawn(SessionId::generate(), None)
    }

    /// Compose the runtime with an explicit approval policy and a
    /// runtime-enforced budget (§20.5). Used for bounded child
    /// sessions; hosts may also budget the root session.
    #[must_use]
    pub fn start_budgeted(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        policy: Arc<dyn PolicyEngine>,
        budget: RuntimeBudget,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.budget = budget;
        composition.spawn(SessionId::generate(), None)
    }

    /// Reopen a previously persisted session: the transcript and any
    /// open operation are rebuilt from the store (DESIGN.md §32 Step 2,
    /// §9.5). Recovery decisions for a non-terminal operation are Step 3
    /// work; until then an open operation surfaces in the snapshot and
    /// blocks new submits.
    pub async fn open_session(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        session_id: SessionId,
    ) -> Result<Self, RuntimeError> {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        let composition = Composition::new(provider, tools, store);
        Ok(composition.spawn(session_id, Some(loaded)))
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

    /// Test hook (DESIGN.md §30.2): abort the session task at its current
    /// await point without close semantics — the process-loss crash
    /// window. Durable state stays at the last committed checkpoint.
    #[cfg(test)]
    pub(crate) fn crash(&self) {
        self.join.abort();
    }
}

/// Does a provider failure message indicate the context exceeded the
/// model's window (14.7.5)? Conservative substring match over the
/// common provider phrasings; unknown phrasings fail visibly instead
/// of triggering a speculative compaction.
fn is_context_overflow(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("context length")
        || lowered.contains("context window")
        || lowered.contains("too many token")
        || lowered.contains("prompt is too long")
}

/// A model-invoked compaction request parsed from `compact` tool
/// arguments (DESIGN.md §14.7.3).
#[derive(Debug, Clone)]
struct PendingCompact {
    instructions: Option<String>,
    continue_after: bool,
}

/// Parse `compact` tool arguments. A malformed payload is a
/// model-visible denial, not a harness failure.
fn parse_compact_arguments(arguments: &serde_json::Value) -> Result<PendingCompact, String> {
    let Some(object) = arguments.as_object() else {
        return Err("compact arguments must be an object".to_owned());
    };
    let instructions = match object.get("instructions") {
        None | Some(serde_json::Value::Null) => None,
        Some(value @ serde_json::Value::String(_)) => {
            Some(value.as_str().expect("string").to_owned())
        }
        Some(_) => return Err("compact instructions must be a string".to_owned()),
    };
    let continue_after = match object.get("continue_after_compaction") {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(flag)) => *flag,
        Some(_) => {
            return Err("continue_after_compaction must be a boolean".to_owned());
        }
    };
    Ok(PendingCompact {
        instructions,
        continue_after,
    })
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
/// The composed collaborators one session runtime runs with (DESIGN.md
/// §4.1): one provider port, one capability snapshot, one store, one
/// approval policy.
/// Runtime-enforced budget bounds (§20.5). `None`/zero means
/// unbounded; exact defaults are configuration, not architecture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeBudget {
    /// Maximum model steps (provider requests) per operation.
    pub max_model_steps: Option<u32>,
    /// Maximum admitted tool effects per operation.
    pub max_tool_calls: Option<u32>,
}

impl RuntimeBudget {
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            max_model_steps: None,
            max_tool_calls: None,
        }
    }
}

struct SessionDeps<P> {
    provider: Arc<P>,
    initial_model_ref: String,
    tools: Arc<ToolCatalog>,
    artifact_root: Option<PathBuf>,
    store: SessionStore,
    policy: Arc<dyn PolicyEngine>,
    budget: RuntimeBudget,
    /// Durable lineage for bounded child sessions (§20.3).
    parent: Option<SessionId>,
}

struct SessionRuntime<P> {
    session_id: SessionId,
    cwd: String,
    provider: Arc<P>,
    /// Authoritative model selection for future steps. The initial id
    /// is in the session row; changes are semantic entries.
    selected_model_ref: String,
    tools: Arc<ToolCatalog>,
    artifact_root: Option<PathBuf>,
    store: SessionStore,
    policy: Arc<dyn PolicyEngine>,
    budget: RuntimeBudget,
    parent_session_id: Option<SessionId>,
    /// Tool effects admitted by the active operation (budget counter).
    operation_tool_calls: u32,
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
    /// Live reasoning text for the current step; cleared at settlement.
    /// Display-only: thinking is never durable assistant content.
    draft_thinking: String,
    draft_calls: Vec<ToolCall>,
    /// Token usage buffered from the live model step; persisted at the
    /// settlement boundary (DESIGN.md §27.2).
    draft_usage: Option<TokenUsage>,
    /// Full token cost (input + output + cache) of the most recent
    /// settled step; anchors usage hints and the safety net (14.7).
    last_context_tokens: Option<u64>,
    /// Context size at the most recent emitted usage hint; the hint
    /// throttle anchor. In-memory only: losing it costs one extra
    /// hint after restart, never a missed one.
    last_hint_tokens: Option<u64>,
    /// Cached model context window (14.8); fetched from the adapter
    /// once, on first use.
    context_window: Option<u64>,
    /// A model-invoked compaction (14.7.3) waiting for the run to
    /// settle; consumed at the next continuation boundary.
    pending_compact: Option<PendingCompact>,
    /// Reopened Suspended operations awaiting durable settlement
    /// (§9.5); empty unless --resume found one.
    suspended_operations: Vec<(OperationId, u64, crate::store::CheckpointPayload)>,
    /// Whether the consumed compaction should be followed by a hidden
    /// recovery turn.
    recovery_after_compaction: bool,
    /// The running compaction was model-invoked (14.7.3); without a
    /// recovery request it finishes the operation instead of prompting.
    compaction_was_model_invoked: bool,
    /// An overflow already triggered the one compaction+retry
    /// (14.7.5); a second overflow fails the operation visibly.
    overflow_retry_used: bool,
    /// The previous step was compaction itself; prevents the
    /// compaction step's own usage from re-triggering compaction.
    last_step_was_compaction: bool,
    /// Monotonic model-step counter for the active operation; provider
    /// signals carry the step that produced them, and stale generations
    /// are dropped.
    model_step: u64,
    events: broadcast::Sender<RuntimeEvent>,
    /// Started-but-unsettled tool calls of the active operation;
    /// mirrors emitted ToolStarted/ToolSettled for snapshot
    /// reconstruction (§21.4).
    live_tools: Vec<PendingTool>,
    closed: bool,
    /// True when reopened from the store; the session row already exists.
    resumed: bool,
}

impl<P: Provider> SessionRuntime<P> {
    fn new(
        session_id: SessionId,
        cwd: String,
        deps: SessionDeps<P>,
        commands: mpsc::Receiver<SessionCommand>,
        loaded: Option<LoadedSession>,
    ) -> Self {
        let SessionDeps {
            provider,
            initial_model_ref,
            tools,
            artifact_root,
            store,
            policy,
            budget,
            parent,
        } = deps;
        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let mut runtime = Self {
            session_id,
            cwd,
            provider,
            selected_model_ref: initial_model_ref,
            tools,
            artifact_root,
            store,
            policy,
            budget,
            parent_session_id: parent,
            operation_tool_calls: 0,
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
            draft_thinking: String::new(),
            draft_calls: Vec::new(),
            draft_usage: None,
            last_context_tokens: None,
            last_hint_tokens: None,
            context_window: None,
            pending_compact: None,
            suspended_operations: Vec::new(),
            recovery_after_compaction: false,
            compaction_was_model_invoked: false,
            overflow_retry_used: false,
            last_step_was_compaction: false,
            model_step: 0,
            events,
            live_tools: Vec::new(),
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
        self.selected_model_ref = loaded.session.initial_model_ref.clone();
        let mut max_seq = 0;
        for (seq, entry) in loaded.entries {
            max_seq = max_seq.max(seq);
            if let SessionEntry::ModelChanged { model_ref } = &entry {
                self.selected_model_ref.clone_from(model_ref);
            }
            self.entries.push(entry);
        }
        self.next_entry_seq = max_seq + 1;
        for operation in loaded.operations {
            let (state_seq, payload) = operation.latest;
            if matches!(payload.state, OperationState::Finished(_)) {
                // Terminal operations stay in the transcript only.
                continue;
            }
            if matches!(payload.state, OperationState::Suspended) {
                // Suspend is teardown with effects cancelled (§9.4);
                // the operation can never continue. Skip it here; the
                // async recovery pass settles it durably so it cannot
                // block the session forever.
                self.suspended_operations
                    .push((operation.id, state_seq, payload.clone()));
                continue;
            }
            // Mid-flight operations are recoverable state (§9.5); they
            // rebuild and surface.
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
                initial_model_ref: self.selected_model_ref.clone(),
                parent_session_id: self.parent_session_id,
            };
            if let Err(err) = self.store.create_session(record).await {
                error!(
                    session = %self.session_id,
                    %err,
                    "session row not durable; session will not start"
                );
                self.closed = true;
                return;
            }
        }
        info!(session = %self.session_id, "session opened");
        if self.operation.is_some() {
            self.recover_open_operation().await;
        }
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
            SessionCommand::Compact {
                instructions,
                reply,
            } => {
                let requested = self.operation.is_some();
                if requested {
                    // Consumed at the next continuation boundary by the
                    // same path the model's own compact tool uses.
                    self.pending_compact = Some(PendingCompact {
                        instructions,
                        continue_after: false,
                    });
                }
                let _ = reply.send(Ok(requested));
                false
            }
            SessionCommand::SwitchModel { model_ref, reply } => {
                let _ = reply.send(self.switch_model(model_ref).await);
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
        self.draft_thinking.clear();
        self.draft_calls.clear();
        self.draft_usage = None;
        self.live_tools.clear();
        self.overflow_retry_used = false;
        self.operation_tool_calls = 0;
        // Usage/hint/compaction anchors are session-level: context
        // persists across operations, so they survive a new submit.
        self.advance().await;
        Ok(operation_id)
    }

    async fn switch_model(&mut self, model_ref: String) -> Result<String, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let model_ref = model_ref.trim().to_owned();
        if model_ref.is_empty() || !self.provider.supports_model(&model_ref) {
            return Err(CommandError::UnsupportedModel(model_ref));
        }
        let previous = self.selected_model_ref.clone();
        if model_ref == previous {
            return Ok(previous);
        }
        let entry = SessionEntry::ModelChanged {
            model_ref: model_ref.clone(),
        };
        let record = self.stage_entry(&entry);
        self.store
            .append_entry(self.session_id, record)
            .await
            .map_err(persistence_command_error)?;
        self.next_entry_seq += 1;
        self.entries.push(entry);
        self.selected_model_ref = model_ref;
        // Model-relative metadata and hint throttling cannot cross a
        // selection boundary.
        self.context_window = None;
        self.last_hint_tokens = None;
        Ok(previous)
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
            Vec::new(),
            vec![record],
            Vec::new(),
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

    /// Ordinary recovery for an operation found open after process loss
    /// (DESIGN.md §32 Step 3, §25.3): pending model steps and
    /// ReplaySafe tool effects replay with a persisted attempt count;
    /// unresolved NeverReplay effects settle as indeterminate and are
    /// never replayed (§12.2); quiescent operations simply continue.
    async fn recover_open_operation(&mut self) {
        let suspended = std::mem::take(&mut self.suspended_operations);
        for (operation_id, state_seq, payload) in suspended {
            let request = CommitRequest {
                session_id: self.session_id,
                operation_id,
                checkpoint: CheckpointRecord {
                    state_seq: state_seq + 1,
                    payload: crate::store::CheckpointPayload {
                        state: OperationState::Finished(OperationOutcome::Cancelled),
                        cancel_requested: false,
                        prompt: payload.prompt.clone(),
                        tools: payload.tools.clone(),
                        open_effect: None,
                    },
                },
                entries: Vec::new(),
                open_effects: Vec::new(),
                settled_effects: Vec::new(),
                indeterminate_effects: Vec::new(),
                inbox: Vec::new(),
                inbox_applied: Vec::new(),
                usage: Vec::new(),
            };
            if let Err(err) = self.store.commit(request).await {
                error!(session = %self.session_id, error = %err, "could not settle a suspended operation");
                self.closed = true;
                return;
            }
            info!(session = %self.session_id, operation = %operation_id, "settled a reopened suspended operation as cancelled");
        }
        let Some(state) = self
            .operation
            .as_ref()
            .map(|active| active.machine.state().clone())
        else {
            return;
        };
        match state {
            OperationState::AssistantEffectPending => {
                let Some(open) = self.operation.as_ref().and_then(|a| a.open_effect.clone()) else {
                    error!(session = %self.session_id, "pending model step without an effect intent; fencing");
                    self.closed = true;
                    return;
                };
                let Some((step, model, plan, persisted_tools)) =
                    model_step_from_input(&open.effective_input)
                else {
                    error!(session = %self.session_id, "pending model step lacks an exact model snapshot; fencing");
                    self.closed = true;
                    return;
                };
                let mut staged = self.operation.clone().expect("operation present");
                let applied = staged
                    .machine
                    .apply(Transition::RecoverModelStep {
                        model: model.clone(),
                        plan: plan.clone(),
                    })
                    .expect("recover a pending model step");
                let EffectIntent::ModelStep { tools, .. } = applied.intents[0].clone() else {
                    panic!("RecoverModelStep must yield a model-step intent");
                };
                if tools != persisted_tools {
                    error!(session = %self.session_id, "pending model step tool snapshot disagrees with checkpoint; fencing");
                    self.closed = true;
                    return;
                }
                let settled = vec![SettledEffect {
                    id: open.id,
                    settlement: serde_json::json!({ "recovered": "process_loss" }),
                }];
                let effect = EffectRecord {
                    id: EffectId::generate(),
                    kind: open.kind.clone(),
                    recovery_class: open.recovery_class,
                    effective_input: open.effective_input.clone(),
                    attempt: open.attempt + 1,
                };
                let (request, new_entry_seq) = build_commit_request(
                    self.session_id,
                    &staged,
                    staged.state_seq + 1,
                    self.next_entry_seq,
                    Vec::new(),
                    vec![effect.clone()],
                    settled,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
                if let Err(err) = self.store.commit(request).await {
                    self.fail_operation_on_persistence(err).await;
                    return;
                }
                let operation_id = staged.machine.operation_id();
                self.next_entry_seq = new_entry_seq;
                staged.state_seq += 1;
                staged.open_effect = Some(effect);
                self.operation = Some(staged);
                self.model_step = step.saturating_sub(1);
                warn!(%operation_id, model = %model.model_ref, "recovered a pending model step by replay");
                self.spawn_model_step(operation_id, model, plan, tools);
            }
            OperationState::CompactionPending => {
                let Some(open) = self.operation.as_ref().and_then(|a| a.open_effect.clone()) else {
                    error!(session = %self.session_id, "pending compaction without an effect intent; fencing");
                    self.closed = true;
                    return;
                };
                let Some((step, model, plan)) = compaction_from_input(&open.effective_input) else {
                    error!(session = %self.session_id, "pending compaction lacks an exact model snapshot; fencing");
                    self.closed = true;
                    return;
                };
                let mut staged = self.operation.clone().expect("operation present");
                staged
                    .machine
                    .apply(Transition::RecoverCompaction { plan: plan.clone() })
                    .expect("recover a pending compaction step");
                let settled = vec![SettledEffect {
                    id: open.id,
                    settlement: serde_json::json!({ "recovered": "process_loss" }),
                }];
                let effect = EffectRecord {
                    id: EffectId::generate(),
                    kind: open.kind.clone(),
                    recovery_class: open.recovery_class,
                    effective_input: open.effective_input.clone(),
                    attempt: open.attempt + 1,
                };
                let (request, new_entry_seq) = build_commit_request(
                    self.session_id,
                    &staged,
                    staged.state_seq + 1,
                    self.next_entry_seq,
                    Vec::new(),
                    vec![effect.clone()],
                    settled,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
                if let Err(err) = self.store.commit(request).await {
                    self.fail_operation_on_persistence(err).await;
                    return;
                }
                let operation_id = staged.machine.operation_id();
                self.next_entry_seq = new_entry_seq;
                staged.state_seq += 1;
                staged.open_effect = Some(effect);
                self.operation = Some(staged);
                self.model_step = step.saturating_sub(1);
                warn!(%operation_id, model = %model.model_ref, "recovered a pending compaction step by replay");
                self.spawn_model_step(operation_id, model, plan, Vec::new());
            }
            OperationState::ToolEffectPending { .. } => {
                let Some(open) = self.operation.as_ref().and_then(|a| a.open_effect.clone()) else {
                    error!(session = %self.session_id, "pending tool effect without an effect intent; fencing");
                    self.closed = true;
                    return;
                };
                match open.recovery_class {
                    RecoveryClass::ReplaySafe => {
                        // Re-execute with the exact effective input.
                        let call =
                            tool_call_from_input(&open.effective_input).unwrap_or_else(|| {
                                panic!("replay-safe tool effect without a usable input")
                            });
                        let mut staged = self.operation.clone().expect("operation present");
                        staged
                            .machine
                            .apply(Transition::RecoverTool { call: call.clone() })
                            .expect("recover a pending replay-safe tool effect");
                        let settled = vec![SettledEffect {
                            id: open.id,
                            settlement: serde_json::json!({ "recovered": "process_loss" }),
                        }];
                        let effect = EffectRecord {
                            id: EffectId::generate(),
                            kind: open.kind.clone(),
                            recovery_class: open.recovery_class,
                            effective_input: open.effective_input.clone(),
                            attempt: open.attempt + 1,
                        };
                        let (request, new_entry_seq) = build_commit_request(
                            self.session_id,
                            &staged,
                            staged.state_seq + 1,
                            self.next_entry_seq,
                            Vec::new(),
                            vec![effect.clone()],
                            settled,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        );
                        if let Err(err) = self.store.commit(request).await {
                            self.fail_operation_on_persistence(err).await;
                            return;
                        }
                        let operation_id = staged.machine.operation_id();
                        self.next_entry_seq = new_entry_seq;
                        staged.state_seq += 1;
                        let effect_id = effect.id;
                        staged.open_effect = Some(effect);
                        self.operation = Some(staged);
                        self.emit_tool_started(
                            operation_id,
                            call.call_id,
                            &call.name,
                            target_summary(&self.tools, &call.name, &call.arguments),
                        );
                        warn!(%operation_id, tool = %call.name, attempt = open.attempt + 1, "recovered a pending replay-safe tool by re-execution");
                        self.spawn_tool_effect(Some(effect_id), call, None);
                    }
                    RecoveryClass::NeverReplay => {
                        // Side effects cannot be classified (§12.4); an
                        // unresolved effect of this class is
                        // indeterminate, never replayed.
                        let mut staged = self.operation.clone().expect("operation present");
                        let applied = staged
                            .machine
                            .apply(Transition::SettleIndeterminate)
                            .expect("settle an unresolved effect as indeterminate");
                        // The indeterminate status IS the settlement; the
                        // effect must not also be marked settled.
                        let settled = Vec::new();
                        let indeterminate = vec![open.id];
                        let (request, new_entry_seq) = build_commit_request(
                            self.session_id,
                            &staged,
                            staged.state_seq + 1,
                            self.next_entry_seq,
                            applied.entries.clone(),
                            Vec::new(),
                            settled,
                            indeterminate,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        );
                        if let Err(err) = self.store.commit(request).await {
                            self.fail_operation_on_persistence(err).await;
                            return;
                        }
                        let operation_id = staged.machine.operation_id();
                        self.next_entry_seq = new_entry_seq;
                        staged.state_seq += 1;
                        staged.open_effect = None;
                        self.entries.extend(applied.entries);
                        self.emit_terminal_state(&applied.state);
                        self.operation = Some(staged);
                        warn!(%operation_id, "an unresolved never-replay effect settled as indeterminate");
                        self.advance().await;
                    }

                    RecoveryClass::Reconcile => {
                        // §12.3: classify the pending file mutation
                        // against the recorded evidence and the file
                        // state found after process loss.
                        let evidence = open
                            .effective_input
                            .get("reconciliation")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let verdict = match &evidence {
                            serde_json::Value::Null => crate::tool::ReconcileVerdict::Unknown,
                            evidence => match evidence.get("path").and_then(|v| v.as_str()) {
                                Some(path) => match crate::tool::file_snapshot(
                                    self.tools.cwd(),
                                    std::path::Path::new(path),
                                    true,
                                )
                                .await
                                {
                                    Ok(current) => crate::tool::classify_reconciliation_snapshot(
                                        evidence,
                                        current.as_ref(),
                                    ),
                                    Err(err) => {
                                        warn!(
                                            "cannot inspect reconciliation target during recovery: {err}"
                                        );
                                        crate::tool::ReconcileVerdict::Conflict
                                    }
                                },
                                None => crate::tool::ReconcileVerdict::Unknown,
                            },
                        };
                        match verdict {
                            crate::tool::ReconcileVerdict::SafeToExecute => {
                                // The evidence proves re-execution is
                                // exactly-once: the file still matches
                                // the recorded preimage.
                                let call = tool_call_from_input(&open.effective_input)
                                    .unwrap_or_else(|| {
                                        panic!("reconcilable tool effect without a usable input")
                                    });
                                let mut staged = self.operation.clone().expect("operation present");
                                staged
                                    .machine
                                    .apply(Transition::RecoverTool { call: call.clone() })
                                    .expect("recover a pending reconcilable tool effect");
                                let settled = vec![SettledEffect {
                                    id: open.id,
                                    settlement: serde_json::json!({
                                        "recovered": "reconciled_preimage_match",
                                    }),
                                }];
                                let effect = EffectRecord {
                                    id: EffectId::generate(),
                                    kind: open.kind.clone(),
                                    recovery_class: open.recovery_class,
                                    effective_input: open.effective_input.clone(),
                                    attempt: open.attempt + 1,
                                };
                                let effect_id = effect.id;
                                let (request, new_entry_seq) = build_commit_request(
                                    self.session_id,
                                    &staged,
                                    staged.state_seq + 1,
                                    self.next_entry_seq,
                                    Vec::new(),
                                    vec![effect.clone()],
                                    settled,
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                );
                                if let Err(err) = self.store.commit(request).await {
                                    self.fail_operation_on_persistence(err).await;
                                    return;
                                }
                                let operation_id = staged.machine.operation_id();
                                self.next_entry_seq = new_entry_seq;
                                staged.state_seq += 1;
                                staged.open_effect = Some(effect.clone());
                                self.operation = Some(staged);
                                self.emit_tool_started(
                                    operation_id,
                                    call.call_id,
                                    &call.name,
                                    target_summary(&self.tools, &call.name, &call.arguments),
                                );
                                self.spawn_tool_effect(
                                    Some(effect_id),
                                    call,
                                    Some(evidence.clone()),
                                );
                                info!(%operation_id, "reconciled a pending file mutation by preimage match");
                            }
                            crate::tool::ReconcileVerdict::AlreadyApplied => {
                                // The postimage is on disk: settle the
                                // effect as completed without repeating
                                // it.
                                let call_id = open
                                    .effective_input
                                    .get("call_id")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or_default();
                                let mut staged = self.operation.clone().expect("operation present");
                                let applied = staged
                                    .machine
                                    .apply(Transition::ToolSettled {
                                        result: ToolResult::Ok {
                                            call_id,
                                            output: "recovered: already applied".to_owned(),
                                            artifact: None,
                                        },
                                    })
                                    .expect("settle an already-applied reconcilable effect");
                                let settled = vec![SettledEffect {
                                    id: open.id,
                                    settlement: serde_json::json!({
                                        "recovered": "reconciled_postimage_match",
                                    }),
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
                                    Vec::new(),
                                    Vec::new(),
                                );
                                if let Err(err) = self.store.commit(request).await {
                                    self.fail_operation_on_persistence(err).await;
                                    return;
                                }
                                let operation_id = staged.machine.operation_id();
                                self.next_entry_seq = new_entry_seq;
                                staged.state_seq += 1;
                                staged.open_effect = None;
                                self.entries.extend(applied.entries);
                                self.operation = Some(staged);
                                info!(%operation_id, "reconciled a pending file mutation as already applied");
                                self.advance().await;
                            }
                            crate::tool::ReconcileVerdict::Conflict
                            | crate::tool::ReconcileVerdict::Unknown => {
                                // The file matches neither record, or no
                                // evidence exists: never overwrite; the
                                // user decides (§12.2, §12.3).
                                let mut staged = self.operation.clone().expect("operation present");
                                let applied = staged
                                    .machine
                                    .apply(Transition::SettleIndeterminate)
                                    .expect("settle an unresolved effect as indeterminate");
                                let indeterminate = vec![open.id];
                                let (request, new_entry_seq) = build_commit_request(
                                    self.session_id,
                                    &staged,
                                    staged.state_seq + 1,
                                    self.next_entry_seq,
                                    applied.entries.clone(),
                                    Vec::new(),
                                    Vec::new(),
                                    indeterminate,
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                );
                                if let Err(err) = self.store.commit(request).await {
                                    self.fail_operation_on_persistence(err).await;
                                    return;
                                }
                                let operation_id = staged.machine.operation_id();
                                self.next_entry_seq = new_entry_seq;
                                staged.state_seq += 1;
                                staged.open_effect = None;
                                self.entries.extend(applied.entries);
                                self.emit_terminal_state(&applied.state);
                                self.operation = Some(staged);
                                warn!(%operation_id, "a pending file mutation settled as indeterminate (conflict)");
                                self.advance().await;
                            }
                        }
                    }
                }
            }
            OperationState::Accepted
            | OperationState::NeedAssistant
            | OperationState::NeedContinuation
            | OperationState::ToolsPlanned { .. }
            | OperationState::Suspended => {
                // Quiescent or fully-committed states continue through
                // ordinary flow.
                self.advance().await;
            }
            OperationState::Finished(_) => {}
        }
    }

    /// Drive the machine forward from quiescent states: drain queued
    /// inbox items at their boundaries, then start the next model step
    /// or admit the next planned tool. Each move commits durably before
    /// its effect starts (§12.1).
    /// The durable seq of the first in-memory transcript entry.
    fn first_entry_seq(&self) -> u64 {
        self.next_entry_seq - self.entries.len() as u64
    }

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
                    if let Some(request) = self.pending_compact.take() {
                        // §14.7.3: the run has settled; compact now with
                        // the caller's preservation instructions.
                        self.recovery_after_compaction = request.continue_after;
                        self.compaction_was_model_invoked = true;
                        if !self.start_compaction(request.instructions).await {
                            return;
                        }
                        return;
                    }
                    if self.safety_net_compaction_due() {
                        // §14.7.4: compact at the continuation boundary
                        // when the context nears the model's window.
                        if !self.start_compaction(None).await {
                            return;
                        }
                        return;
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
                Vec::new(),
                applied_ids,
                Vec::new(),
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

    /// Project the model-step input and append the context-usage hint
    /// when one is due (14.7.2). Both the fresh start and the recovery
    /// re-projection go through here so a recovered step sees the same
    /// trailing-edge hint policy. The hint is derived, never persisted.
    async fn current_model_config(&mut self) -> ModelConfig {
        if self.context_window.is_none() {
            self.context_window = self
                .provider
                .context_window_for(&self.selected_model_ref)
                .await;
        }
        ModelConfig {
            model_ref: self.selected_model_ref.clone(),
            context_window: self.context_window,
        }
    }

    async fn project_model_step_plan(&mut self) -> crate::context::ContextPlan {
        let _ = self.current_model_config().await;
        let mut plan = project(&self.entries, self.first_entry_seq());
        let hint = crate::context::usage_hint(
            self.last_context_tokens.unwrap_or(0),
            self.context_window,
            self.last_hint_tokens,
        );
        if let Some(hint) = hint {
            self.last_hint_tokens = self.last_context_tokens;
            crate::context::push_hint(&mut plan, hint);
        }
        plan
    }

    /// Safety-net compaction check (§14.7.4), evaluated at
    /// continuation boundaries only: compact when the measured context
    /// is within the reserve of the model's actual window. A fixed
    /// token threshold would be wrong for every model except the one
    /// it was tuned for. Unknown windows disable the net; overflow
    /// recovery (14.7.5) is the backstop.
    fn safety_net_compaction_due(&self) -> bool {
        const RESERVE_TOKENS: u64 = 16_000;
        if self.last_step_was_compaction {
            return false;
        }
        match self.context_window {
            Some(window) => {
                self.last_context_tokens.unwrap_or(0) > window.saturating_sub(RESERVE_TOKENS)
            }
            None => false,
        }
    }

    /// Commit the compaction effect intent, then spawn the provider
    /// effect that produces the readable summary. Returns false when
    /// persistence failed.
    async fn start_compaction(&mut self, instructions: Option<String>) -> bool {
        let model = self.current_model_config().await;
        let mut plan = project(&self.entries, self.first_entry_seq());
        let mut content = crate::context::SUMMARIZE_INSTRUCTION.to_owned();
        if let Some(instructions) = instructions {
            content.push_str("\n\nPreservation instructions from the caller: ");
            content.push_str(&instructions);
        }
        plan.messages
            .push(crate::context::ContextMessage::User { content });
        let mut staged = self
            .operation
            .clone()
            .expect("compaction needs an operation");
        let applied = staged
            .machine
            .apply(Transition::StartCompaction { plan: plan.clone() })
            .expect("start compaction from a continuation boundary");
        let EffectIntent::Compaction { operation_id, .. } = applied.intents[0].clone() else {
            panic!("StartCompaction must yield a compaction intent");
        };
        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "compaction".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({
                "step": self.model_step + 1,
                "model": model,
                "plan": plan
            }),
            attempt: 1,
        };
        staged.open_effect = Some(effect.clone());
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            vec![effect],
            Vec::new(),
            Vec::new(),
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
        self.operation = Some(staged);
        self.last_step_was_compaction = true;
        self.last_hint_tokens = None;
        info!(%operation_id, "starting automatic compaction");
        self.spawn_model_step(operation_id, model, plan, Vec::new());
        true
    }

    /// Commit the model-step effect intent, then spawn the provider
    /// effect. Returns false when persistence failed.
    /// Fail the active operation durably for a spent budget
    /// dimension (§20.5): model-visible, terminal, no retry.
    async fn fail_budgeted(&mut self, dimension: &str) {
        if self.operation.is_none() {
            return;
        }
        let message = format!("operation budget exceeded: {dimension}");
        warn!(session = %self.session_id, %message, "budget exhausted");
        let mut staged = self
            .operation
            .clone()
            .expect("budget fail needs an operation");
        let applied = staged
            .machine
            .apply(Transition::FailOperation {
                message: message.clone(),
            })
            .expect("fail operation for budget");
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            applied.entries.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
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
        // Emits OperationFailed for the Failed outcome (terminal event
        // contract); idling here mirrors the approval-required path so
        // no command observes a Finished-but-open operation.
        self.emit_terminal_state(&applied.state);
        self.operation.take();
    }

    async fn start_model_step(&mut self) -> bool {
        if self
            .budget
            .max_model_steps
            .is_some_and(|max| self.model_step >= u64::from(max))
        {
            // Budget bounds are runtime-enforced (§20.5): the
            // operation fails model-visibly instead of looping.
            self.fail_budgeted("model steps").await;
            return false;
        }
        let plan = self.project_model_step_plan().await;
        let model = self.current_model_config().await;
        let mut staged = self.operation.clone().expect("step needs an operation");
        let applied = staged
            .machine
            .apply(Transition::StartModelStep {
                model: model.clone(),
                plan: plan.clone(),
            })
            .expect("start model step from a quiescent state");
        let EffectIntent::ModelStep {
            operation_id,
            model,
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
            effective_input: serde_json::json!({
                "step": self.model_step + 1,
                "model": model,
                "plan": plan,
                "tools": tools
            }),
            attempt: 1,
        };
        // The pending effect is part of the checkpoint: it must be on the
        // staged operation before the commit is built.
        staged.open_effect = Some(effect.clone());
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            vec![effect],
            Vec::new(),
            Vec::new(),
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
        self.operation = Some(staged);
        self.spawn_model_step(operation_id, model, plan, tools);
        true
    }

    /// Commit a tool effect intent, then spawn the tool effect (or
    /// settle a validation denial through the normal path). Returns
    /// false when persistence failed.
    async fn admit_next_tool(&mut self) -> bool {
        // The policy gate runs before any effect intent is committed
        // (§17.3): peek the next call, canonicalize it, and decide.
        let Some(call) = self
            .operation
            .as_ref()
            .and_then(|active| active.machine.next_planned_call().cloned())
        else {
            error!(session = %self.session_id, "admit with no planned call; fencing");
            self.closed = true;
            return false;
        };
        // The compact tool is a harness maintenance action (14.7.3):
        // always allowed, never a capability grant. Its arguments are
        // parsed here and consumed at the next continuation boundary;
        // execution itself settles as a normal no-op tool result.
        let mut compact_request: Option<Result<PendingCompact, String>> = None;
        if call.name == "compact" {
            compact_request = Some(parse_compact_arguments(&call.arguments));
        }
        let canonical = self.tools.canonicalize(&call.name, &call.arguments);
        let decision = match &compact_request {
            Some(Ok(_)) => PolicyDecision::Allow,
            // Malformed compact arguments deny model-visibly with the
            // parse message, before the policy gate sees the tool.
            Some(Err(message)) => PolicyDecision::Deny(message.clone()),
            None => match &canonical {
                // Delegation is a structural capability (§20.4): every
                // effect a child can produce is individually gated
                // inside the child, so spawning one needs no grant -
                // same reasoning as compact.
                Ok(_) if call.name == "delegate" => PolicyDecision::Allow,
                Ok(target) => self.policy.decide(&call.name, target),
                // Canonicalization failure is a model-visible denial,
                // not a harness failure: the model produced an unusable
                // input.
                Err(message) => PolicyDecision::Deny(message.clone()),
            },
        };
        // Tool-call budget (§20.5): spent budget denies further calls
        // model-visibly; the model can still finish its turn. Compact
        // stays exempt: it is harness maintenance, not a capability.
        let over_tool_budget = compact_request.is_none()
            && self
                .budget
                .max_tool_calls
                .is_some_and(|max| self.operation_tool_calls >= max);
        let decision = if over_tool_budget {
            PolicyDecision::Deny("operation tool-call budget exhausted".to_owned())
        } else {
            decision
        };
        if decision == PolicyDecision::ApprovalRequired {
            // §17.4: nothing may execute, so nothing is committed as an
            // effect intent; the operation terminates with the durable
            // ApprovalRequired outcome.
            let mut staged = self.operation.clone().expect("admit needs an operation");
            let applied = staged
                .machine
                .apply(Transition::ApprovalRequired {
                    tool: call.name.clone(),
                })
                .expect("approval-required from ToolsPlanned");
            let (request, new_entry_seq) = build_commit_request(
                self.session_id,
                &staged,
                staged.state_seq + 1,
                self.next_entry_seq,
                applied.entries.clone(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
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
            self.operation = Some(staged);
            warn!(tool = %call.name, "approval required; terminating the operation");
            self.emit_terminal_state(&applied.state);
            // Terminal: idle the operation here (the caller's arm
            // returns without re-reading state), synchronously so no
            // command can observe a Finished-but-open operation.
            self.operation.take();
            return true;
        }

        let mut denial: Option<String> = match decision {
            PolicyDecision::Deny(message) => Some(message),
            PolicyDecision::Allow => match compact_request {
                Some(Ok(request)) => {
                    self.pending_compact = Some(request);
                    None
                }
                Some(Err(message)) => Some(message),
                None => self.tools.validate(&call.name, &call.arguments).err(),
            },
            PolicyDecision::ApprovalRequired => unreachable!("handled above"),
        };
        // §12.3: file-mutating effects persist reconciliation evidence
        // with the intent, before execution. An evidence failure means
        // the invocation could not be classified, so it is denied
        // model-visibly instead of admitted blind.
        let evidence = if denial.is_none() && matches!(call.name.as_str(), "write" | "edit") {
            match crate::tool::reconciliation_evidence(
                self.tools.cwd(),
                &call.name,
                &call.arguments,
            )
            .await
            {
                Ok(evidence) => Some(evidence),
                Err(message) => {
                    denial = Some(message);
                    None
                }
            }
        } else {
            None
        };
        let mut staged = self.operation.clone().expect("admit needs an operation");
        let applied = staged
            .machine
            .apply(Transition::AdmitNextTool)
            .expect("admit next tool from ToolsPlanned");
        let EffectIntent::Tool { call } = applied.intents[0].clone() else {
            panic!("AdmitNextTool must yield a tool intent");
        };
        // The exact invocation the executor will use is part of the
        // durable intent (§17.3: never approve one string and execute
        // a materially different one).
        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: format!("tool:{}", call.name),
            recovery_class: self.tools.recovery_class(&call.name),
            effective_input: serde_json::json!({
                "tool": call.name,
                "arguments": call.arguments,
                "call_id": call.call_id,
                "canonical": canonical.ok(),
                "reconciliation": evidence,
            }),
            attempt: 1,
        };
        // The pending effect is part of the checkpoint: it must be on the
        // staged operation before the commit is built.
        let effect_id = effect.id;
        staged.open_effect = Some(effect.clone());
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            vec![effect],
            Vec::new(),
            Vec::new(),
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
        self.operation = Some(staged);
        if let Some(message) = denial {
            // The denial settles through the normal tool-result path; the
            // tool never started, so no ToolStarted event is emitted.
            let _ = self.tool_tx.try_send((
                effect_id,
                ToolResult::Err {
                    call_id: call.call_id,
                    error: message,
                    artifact: None,
                },
            ));
        } else {
            self.operation_tool_calls += 1;
            let target = target_summary(&self.tools, &call.name, &call.arguments);
            self.emit_tool_started(call.operation_id, call.call_id, &call.name, target);
            let reconciliation = self
                .operation
                .as_ref()
                .and_then(|active| active.open_effect.as_ref())
                .and_then(|effect| effect.effective_input.get("reconciliation"))
                .cloned();
            self.spawn_tool_effect(effect_id_of(self.operation.as_ref()), call, reconciliation);
        }
        true
    }

    fn spawn_model_step(
        &mut self,
        operation_id: OperationId,
        model: ModelConfig,
        plan: ContextPlan,
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
            model: model.clone(),
            plan,
            tools,
        };
        debug!(%operation_id, step, model = %model.model_ref, "starting model step effect");
        let terminal = self.engine_tx.clone();
        self.tracker.spawn(async move {
            provider.run(request, cancel, out.clone()).await;
            let _ = terminal
                .send(EngineSignal::ProviderExited { operation_id, step })
                .await;
        });
    }

    fn spawn_tool_effect(
        &mut self,
        effect_id: Option<EffectId>,
        call: ToolCall,
        reconciliation: Option<serde_json::Value>,
    ) {
        let Some(effect_id) = effect_id else {
            return;
        };
        let tools = Arc::clone(&self.tools);
        let artifact_root = self.artifact_root.clone();
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
            let outcome = tools
                .execute_with_reconciliation(
                    &name,
                    &arguments,
                    reconciliation.as_ref(),
                    artifact_root.as_deref(),
                    cancel,
                )
                .await;
            let result = ToolResult::from_outcome(call_id, outcome);
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
        if matches!(active.machine.state(), OperationState::CompactionPending) {
            self.settle_compaction(signal).await;
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
            EngineSignal::ThinkingDelta { text, .. } => {
                self.draft_thinking.push_str(&text);
                self.emit(RuntimeEvent::ThinkingDelta {
                    cursor: RuntimeCursor::default(),
                    operation_id: active.machine.operation_id(),
                    text,
                });
            }
            EngineSignal::UsageUpdate { usage, .. } => {
                self.last_context_tokens =
                    Some(usage.input + usage.output + usage.cache_read + usage.cache_write);
                self.draft_usage = Some(usage);
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
                if !cancel_requested
                    && is_context_overflow(&message)
                    && !self.overflow_retry_used
                    && !self.last_step_was_compaction
                {
                    // 14.7.5: one compaction, one retry. The failed
                    // attempt produced no durable effect beyond its
                    // intent; its partial request state is discarded.
                    self.settle_overflow_to_compaction().await;
                    return;
                }
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
    /// agree in one transaction. Every settlement path ends the live
    /// reasoning draft (display-only, §21.3).
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
        self.draft_thinking.clear();
        self.last_step_was_compaction = false;
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
        // Usage persists with the settlement, independent of operation
        // success (DESIGN.md §27.2).
        let usage = self
            .draft_usage
            .take()
            .map(|u| {
                vec![UsageRecord {
                    step: self.model_step,
                    input_tokens: u.input,
                    output_tokens: u.output,
                    cache_read_tokens: u.cache_read,
                    cache_write_tokens: u.cache_write,
                }]
            })
            .unwrap_or_default();
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
            Vec::new(),
            usage,
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

    /// Settle a context-overflow failure into a compaction (14.7.5):
    /// the failed attempt's effect settles without entries, the
    /// Compaction intent commits in the same transaction, and the
    /// retry is the natural continuation after the summary lands.
    async fn settle_overflow_to_compaction(&mut self) {
        self.overflow_retry_used = true;
        let mut staged = self.operation.clone().expect("settle needs an operation");
        let model = self.current_model_config().await;
        let mut plan = project(&self.entries, self.first_entry_seq());
        plan.messages.push(crate::context::ContextMessage::User {
            content: crate::context::SUMMARIZE_INSTRUCTION.to_owned(),
        });
        let applied = staged
            .machine
            .apply(Transition::OverflowCompaction { plan: plan.clone() })
            .expect("overflow compaction while AssistantEffectPending");
        let EffectIntent::Compaction { operation_id, .. } = applied.intents[0].clone() else {
            panic!("OverflowCompaction must yield a compaction intent");
        };
        let settled = staged
            .open_effect
            .take()
            .map(|effect| SettledEffect {
                id: effect.id,
                settlement: serde_json::json!({ "kind": "model_step", "overflow": true }),
            })
            .into_iter()
            .collect();
        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "compaction".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({
                "step": self.model_step + 1,
                "model": model,
                "plan": plan
            }),
            attempt: 1,
        };
        staged.open_effect = Some(effect.clone());
        let (request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            applied.entries.clone(),
            vec![effect],
            settled,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.operation = Some(staged);
        // The failed attempt's partial buffers are discarded; usage
        // from a rejected request is not trustworthy.
        self.draft_text.clear();
        self.draft_thinking.clear();
        self.draft_calls.clear();
        self.draft_usage = None;
        self.last_step_was_compaction = true;
        self.last_hint_tokens = None;
        warn!(%operation_id, "context overflow; compacting once and retrying");
        self.spawn_model_step(operation_id, model, plan, Vec::new());
    }

    /// Settle a compaction step: the summary becomes a readable entry
    /// covering everything before it; failure continues without a
    /// baseline (visible in tracing), unless cancellation was requested.
    async fn settle_compaction(&mut self, signal: EngineSignal) {
        let mut staged = self.operation.clone().expect("settle needs an operation");
        if !matches!(staged.machine.state(), OperationState::CompactionPending) {
            debug!("ignored settlement for an already-settled compaction step");
            return;
        }
        let transition = match signal {
            EngineSignal::TextDelta { text, .. } => {
                self.draft_text.push_str(&text);
                return;
            }
            EngineSignal::Completed { .. } => {
                let summary = std::mem::take(&mut self.draft_text);
                Transition::CompactionCompleted {
                    summary,
                    covers_through_seq: self.next_entry_seq - 1,
                }
            }
            EngineSignal::Failed { message, .. } => {
                warn!(message = %message, "compaction generation failed; continuing without a baseline");
                Transition::CompactionFailed
            }
            EngineSignal::Cancelled { .. } | EngineSignal::ProviderExited { .. } => {
                Transition::CompactionFailed
            }
            EngineSignal::ThinkingDelta { .. } => return,
            EngineSignal::ToolCallCompleted { .. } | EngineSignal::UsageUpdate { .. } => return,
        };
        let applied = staged
            .machine
            .apply(transition)
            .expect("compaction settlement while CompactionPending");
        let settled = staged
            .open_effect
            .take()
            .map(|effect| SettledEffect {
                id: effect.id,
                settlement: serde_json::json!({ "kind": "compaction" }),
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
        if self.compaction_was_model_invoked && !self.recovery_after_compaction {
            // 14.7.3: the compact tool call ended the run's planning;
            // without a recovery request the operation is done.
            self.compaction_was_model_invoked = false;
            let mut staged = staged;
            let applied = staged
                .machine
                .apply(Transition::FinishAfterCompaction)
                .expect("finish after model-invoked compaction");
            let (request, new_entry_seq) = build_commit_request(
                self.session_id,
                &staged,
                staged.state_seq + 1,
                new_entry_seq,
                applied.entries.clone(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            if let Err(err) = self.store.commit(request).await {
                self.fail_operation_on_persistence(err).await;
                return;
            }
            self.next_entry_seq = new_entry_seq;
            self.entries.extend(applied.entries);
            self.emit_terminal_state(&applied.state);
            self.operation.take();
            return;
        }
        self.emit_terminal_state(&applied.state.clone());
        self.operation = Some(staged);
        if self.recovery_after_compaction {
            self.recovery_after_compaction = false;
            // §14.7.3: one hidden recovery turn that resumes only
            // unfinished work without repeating settled effects.
            if let Err(err) = self
                .enqueue_inbox(InboxKind::Steer, crate::context::RESUME_MESSAGE.to_owned())
                .await
            {
                warn!(?err, "recovery turn could not be enqueued");
            }
        }
        self.advance().await;
    }

    async fn handle_tool_result(&mut self, settlement: ToolSettlement) {
        let (effect_id, result) = settlement;
        let call_id = result.call_id();
        let is_error = matches!(&result, ToolResult::Err { .. });
        let preview = result.display_preview();
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
        self.live_tools.retain(|pending| pending.call_id != call_id);
        self.emit(RuntimeEvent::ToolSettled {
            cursor: RuntimeCursor::default(),
            operation_id: staged.machine.operation_id(),
            call_id,
            is_error,
            preview,
        });
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
        // A terminal operation has no unsettled tools; any survivor is
        // a cancelled or failed call whose spinner must not resurrect
        // in a post-lag reconstruction.
        self.live_tools.clear();
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
                OperationOutcome::ApprovalRequired { tool } => {
                    self.emit(RuntimeEvent::OperationApprovalRequired {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        tool: tool.clone(),
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
        let snapshot = self.snapshot();
        let rx = self.events.subscribe();
        Ok((snapshot, EventSubscription { rx }))
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
            model_ref: self.selected_model_ref.clone(),
            live: self.operation.as_ref().map(|_| LiveOperationState {
                draft_text: self.draft_text.clone(),
                draft_thinking: self.draft_thinking.clone(),
                pending_tools: self.live_tools.clone(),
            }),
        }
    }

    /// Emit ToolStarted and mirror it into the live-tool set so a
    /// lagged subscriber's snapshot names the same spinners.
    fn emit_tool_started(
        &mut self,
        operation_id: OperationId,
        call_id: u64,
        tool: &str,
        target: Option<String>,
    ) {
        self.emit(RuntimeEvent::ToolStarted {
            cursor: RuntimeCursor::default(),
            operation_id,
            call_id,
            target: target.clone(),
            tool: tool.to_owned(),
        });
        self.live_tools.retain(|pending| pending.call_id != call_id);
        self.live_tools.push(PendingTool {
            call_id,
            tool: tool.to_owned(),
            target,
        });
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
        // A full ring drops the oldest buffered events for that
        // receiver; the receiver detects the gap and reports lag
        // reliably (broadcast semantics, §21.4). No receivers is the
        // normal idle case.
        let _ = self.events.send(event);
    }
}

/// Reconstruct a tool call from a persisted effect's exact effective
/// input (DESIGN.md §12.1). Returns None for inputs from older schemas
/// that lack the call identity.
fn tool_call_from_input(input: &serde_json::Value) -> Option<ToolCall> {
    let operation_id = OperationId::from_uuid(uuid::Uuid::now_v7());
    Some(ToolCall {
        operation_id,
        call_id: input.get("call_id")?.as_u64()?,
        name: input.get("tool")?.as_str()?.to_owned(),
        arguments: input.get("arguments")?.clone(),
    })
}

/// Reconstruct the frozen model snapshot from a persisted provider
/// effect's exact effective input (DESIGN.md §14.8). Recovery replays
/// this identity or fences; it never substitutes a launch default.
fn model_from_input(model: &serde_json::Value) -> Option<ModelConfig> {
    Some(ModelConfig {
        model_ref: model.get("model_ref")?.as_str()?.to_owned(),
        context_window: model
            .get("context_window")
            .and_then(serde_json::Value::as_u64),
    })
}

/// `(step, model, plan)` from a persisted model-step effect input.
fn model_step_from_input(
    input: &serde_json::Value,
) -> Option<(u64, ModelConfig, ContextPlan, Vec<ToolSpec>)> {
    let step = input.get("step")?.as_u64()?;
    let model = model_from_input(input.get("model")?)?;
    let plan = serde_json::from_value(input.get("plan")?.clone()).ok()?;
    let tools = serde_json::from_value(input.get("tools")?.clone()).ok()?;
    Some((step, model, plan, tools))
}

/// `(step, model, plan)` from a persisted compaction effect input.
fn compaction_from_input(input: &serde_json::Value) -> Option<(u64, ModelConfig, ContextPlan)> {
    let step = input.get("step")?.as_u64()?;
    let model = model_from_input(input.get("model")?)?;
    let plan = serde_json::from_value(input.get("plan")?.clone()).ok()?;
    Some((step, model, plan))
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
    indeterminate_effects: Vec<EffectId>,
    inbox: Vec<InboxRecord>,
    inbox_applied: Vec<InboxId>,
    usage: Vec<UsageRecord>,
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
        indeterminate_effects,
        inbox,
        inbox_applied,
        usage,
    };
    (request, seq)
}

fn persistence_command_error(err: StoreError) -> CommandError {
    CommandError::Persistence(err.to_string())
}

fn signal_operation_id(signal: &EngineSignal) -> OperationId {
    match signal {
        EngineSignal::TextDelta { operation_id, .. }
        | EngineSignal::ThinkingDelta { operation_id, .. }
        | EngineSignal::ToolCallCompleted { operation_id, .. }
        | EngineSignal::UsageUpdate { operation_id, .. }
        | EngineSignal::Completed { operation_id, .. }
        | EngineSignal::Failed { operation_id, .. }
        | EngineSignal::Cancelled { operation_id, .. }
        | EngineSignal::ProviderExited { operation_id, .. } => *operation_id,
    }
}

fn signal_step(signal: &EngineSignal) -> u64 {
    match signal {
        EngineSignal::TextDelta { step, .. }
        | EngineSignal::ThinkingDelta { step, .. }
        | EngineSignal::ToolCallCompleted { step, .. }
        | EngineSignal::UsageUpdate { step, .. }
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
        | RuntimeEvent::ThinkingDelta { cursor: slot, .. }
        | RuntimeEvent::ToolStarted { cursor: slot, .. }
        | RuntimeEvent::ToolSettled { cursor: slot, .. }
        | RuntimeEvent::OperationFinished { cursor: slot, .. }
        | RuntimeEvent::OperationFailed { cursor: slot, .. }
        | RuntimeEvent::OperationCancelled { cursor: slot, .. }
        | RuntimeEvent::OperationApprovalRequired { cursor: slot, .. }
        | RuntimeEvent::SessionClosed { cursor: slot } => *slot = cursor,
    }
}

fn event_kind(event: &RuntimeEvent) -> &'static str {
    match event {
        RuntimeEvent::OperationStarted { .. } => "operation_started",
        RuntimeEvent::AssistantTextDelta { .. } => "assistant_text_delta",
        RuntimeEvent::ThinkingDelta { .. } => "thinking_delta",
        RuntimeEvent::ToolStarted { .. } => "tool_started",
        RuntimeEvent::ToolSettled { .. } => "tool_settled",
        RuntimeEvent::OperationFinished { .. } => "operation_finished",
        RuntimeEvent::OperationFailed { .. } => "operation_failed",
        RuntimeEvent::OperationCancelled { .. } => "operation_cancelled",
        RuntimeEvent::OperationApprovalRequired { .. } => "operation_approval_required",
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
