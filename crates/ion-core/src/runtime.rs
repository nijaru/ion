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
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

use crate::context::{
    CapabilitySnapshot, ContextManifest, ContextPlan, TrustedResource, project_with_manifest,
};
use crate::error::{CommandError, RuntimeError};
use crate::ids::{EffectId, InboxId, OperationId, RuntimeCursor, RuntimeInstanceId, SessionId};
use crate::policy::{DefaultPolicy, PolicyDecision, PolicyEngine};
use crate::provider::{
    EngineSignal, ModelCapabilities, ModelConfig, Provider, ProviderRequest, TokenUsage,
};
use crate::session::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
use crate::store::{
    AssistantFrame, CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord,
    InboxRecord, InboxStatus, LoadedSession, SessionRecord, SessionStore, SettledEffect,
    StoreError, ToolProgressCheckpoint, UsageRecord,
};
use crate::tool::{
    RecoveryClass, ToolCall, ToolCatalog, ToolProgress, ToolRegistry, ToolResult, ToolSpec,
};

mod effects;
mod persistence;
mod recovery;
use persistence::{
    build_commit_request, compaction_from_input, model_step_from_input, tool_call_from_input,
};

const COMMAND_CAPACITY: usize = 32;
const ENGINE_CAPACITY: usize = 64;
const INDETERMINATE_MESSAGE: &str = "external effect is indeterminate; inspect it before retrying";
const ASSISTANT_FRAME_MAX_BYTES: usize = 64 * 1024;
/// Broadcast buffer per subscriber (§21.4): a slow UI never blocks or
/// grows the runtime; overflow surfaces as a reliable lag error.
const SUBSCRIBER_CAPACITY: usize = 64;
type SubscribeReply = Result<(SessionSnapshot, EventSubscription), CommandError>;

/// Test-controlled production boundary. The gate only pauses the existing
/// runtime procedure; it does not replace transitions or persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectBoundary {
    ModelExecution,
    ModelSettlement,
    CompactionExecution,
    CancellationSignal,
    QueuedAcceptanceCommit,
    ToolExecution,
    ToolSettlement,
    CloseSuspendCommit,
}

#[derive(Clone)]
pub(crate) struct EffectGate {
    boundary: EffectBoundary,
    reached: Arc<AtomicBool>,
    released: Arc<AtomicBool>,
    reached_notify: Arc<Notify>,
    release_notify: Arc<Notify>,
}

impl EffectGate {
    #[cfg(test)]
    pub(crate) fn new(boundary: EffectBoundary) -> Self {
        Self {
            boundary,
            reached: Arc::new(AtomicBool::new(false)),
            released: Arc::new(AtomicBool::new(false)),
            reached_notify: Arc::new(Notify::new()),
            release_notify: Arc::new(Notify::new()),
        }
    }

    async fn wait(&self, boundary: EffectBoundary) {
        if self.boundary != boundary || self.released.load(Ordering::Acquire) {
            return;
        }
        let released = self.release_notify.notified();
        if !self.reached.swap(true, Ordering::AcqRel) {
            self.reached_notify.notify_waiters();
        }
        if !self.released.load(Ordering::Acquire) {
            released.await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_until_reached(&self) {
        let reached = self.reached_notify.notified();
        if !self.reached.load(Ordering::Acquire) {
            reached.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.release_notify.notify_waiters();
    }
}
enum ToolSignal {
    Progress {
        effect_id: EffectId,
        call_id: u64,
        output: String,
    },
    Settled {
        effect_id: EffectId,
        result: ToolResult,
    },
}

type ToolSettlement = ToolSignal;

/// Keep auxiliary recovery output bounded without splitting UTF-8.
fn bounded_frame_text(text: &str) -> String {
    const MARKER: &str = "[earlier output truncated]\n";
    if text.len() <= ASSISTANT_FRAME_MAX_BYTES {
        return text.to_owned();
    }
    let content_limit = ASSISTANT_FRAME_MAX_BYTES.saturating_sub(MARKER.len());
    let mut start = text.len().saturating_sub(content_limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("{MARKER}{}", &text[start..])
}

/// One-line display summary of a call's canonical target (best
/// effort; None when canonicalization fails — the denial surfaces
/// elsewhere).
fn target_summary_registry(
    tools: &ToolRegistry,
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
    /// A repeat-sensitive external effect could not be classified safely
    /// after process loss. The caller must inspect it before retrying.
    OperationIndeterminate {
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
            | Self::OperationIndeterminate { operation_id, .. }
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
            | Self::OperationIndeterminate { cursor, .. }
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
    /// Identity of the loaded runtime incarnation. It changes on reopen even
    /// when the durable session id stays the same.
    pub runtime_instance_id: RuntimeInstanceId,
    /// An unresolved external effect that was settled as indeterminate.
    /// Recovery can finish before a frontend subscribes, so this warning is
    /// carried in the snapshot as well as the live event.
    pub indeterminate: Option<IndeterminateWarning>,
    pub operation: OperationStatus,
    pub entries: Vec<SessionEntry>,
    /// Number of durable entries present when this runtime was reopened.
    /// Frontends use it to place a resume boundary without reading the
    /// session store directly. `None` means this runtime created a new
    /// session.
    pub reopen_entry_count: Option<usize>,
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

/// Durable recovery warning for an external effect whose final outcome is
/// unknown. There is deliberately no automatic retry operation here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndeterminateWarning {
    pub operation_id: OperationId,
    pub message: String,
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
    SubmitIfIdle {
        prompt: String,
        reply: oneshot::Sender<Result<OperationId, CommandError>>,
    },
    Enqueue {
        prompt: String,
        reply: oneshot::Sender<Result<OperationId, CommandError>>,
    },
    Steer {
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
        self.tx.try_send(build(reply)).map_err(command_send_error)?;
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
    /// Accept a prompt durably and open a new operation only when idle.
    pub async fn submit_if_idle(
        &self,
        prompt: impl Into<String>,
    ) -> Result<OperationId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SubmitIfIdle {
                prompt: prompt.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Accept a prompt durably. If another operation is active, the new
    /// operation waits in acceptance order and is promoted after the active
    /// operation reaches a terminal outcome.
    pub async fn enqueue(&self, prompt: impl Into<String>) -> Result<OperationId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Enqueue {
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
    trusted_resources: Vec<TrustedResource>,
    effect_gate: Option<Arc<EffectGate>>,
    /// Host-selected workspace identity. A reopened session uses the
    /// persisted value instead, so process cwd cannot silently change it.
    cwd: Option<String>,
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
            trusted_resources: Vec::new(),
            effect_gate: None,
            cwd: None,
        }
    }

    fn spawn(mut self, session_id: SessionId, loaded: Option<LoadedSession>) -> Runtime {
        let initial_model_ref = self.provider.initial_model_ref();
        let runtime_instance_id = RuntimeInstanceId::generate();
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let session = SessionHandle { tx };
        let provider = Arc::new(self.provider);
        let tools = Arc::new(self.tools);
        let artifact_root = self.store.artifact_root();
        let trusted_resources = self.trusted_resources;
        let effect_gate = self.effect_gate;
        let cwd = loaded
            .as_ref()
            .map(|loaded| loaded.session.cwd.clone())
            .or_else(|| self.cwd.take())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let join = tokio::spawn(async move {
            SessionRuntime::new(
                session_id,
                runtime_instance_id,
                cwd,
                SessionDeps {
                    provider,
                    initial_model_ref,
                    tools,
                    artifact_root,
                    trusted_resources,
                    effect_gate,
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
            session,
            session_id,
            join,
        }
    }
}

/// Process-level runtime: composition and the session registry. v0 keeps
/// exactly one loaded session (DESIGN.md §32 Step 1).
pub struct Runtime {
    session: SessionHandle,
    session_id: SessionId,
    join: JoinHandle<()>,
}

impl Runtime {
    /// Compose the runtime with one new durable session in `store`.
    #[must_use]
    pub fn start_with_store(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
    ) -> Self {
        Composition::new(provider, tools, store).spawn(SessionId::generate(), None)
    }

    #[cfg(test)]
    pub(crate) fn start_with_effect_gate(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        gate: EffectGate,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.effect_gate = Some(Arc::new(gate));
        composition.spawn(SessionId::generate(), None)
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
        Self::start_with_policy_and_resources(provider, tools, store, policy, Vec::new())
    }

    /// Compose a runtime with an explicit trusted project-resource snapshot.
    /// Trust is supplied by the host; retrieved text cannot grant it.
    #[must_use]
    pub fn start_with_policy_and_resources(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        policy: Arc<dyn PolicyEngine>,
        trusted_resources: Vec<TrustedResource>,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.trusted_resources = trusted_resources;
        composition.spawn(SessionId::generate(), None)
    }

    /// Compose a new session with an explicit workspace identity. Hosts such
    /// as ACP may expose a workspace other than the process cwd; that
    /// identity must be durable so a later load validates the same session.
    #[must_use]
    pub fn start_with_policy_and_resources_in_cwd(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        policy: Arc<dyn PolicyEngine>,
        trusted_resources: Vec<TrustedResource>,
        cwd: impl Into<String>,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.trusted_resources = trusted_resources;
        composition.cwd = Some(cwd.into());
        composition.spawn(SessionId::generate(), None)
    }

    /// Compose a bounded child with an explicitly inherited trusted-resource
    /// snapshot. The child receives no broader capability set than its host.
    #[must_use]
    pub fn start_child_with_resources(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        policy: Arc<dyn PolicyEngine>,
        budget: RuntimeBudget,
        parent: SessionId,
        trusted_resources: Vec<TrustedResource>,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.budget = budget;
        composition.parent = Some(parent);
        composition.trusted_resources = trusted_resources;
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
        Self::open_session_with_resources(provider, tools, store, session_id, Vec::new()).await
    }

    /// Reopen a session with the host's explicit trusted-resource snapshot.
    pub async fn open_session_with_resources(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        session_id: SessionId,
        trusted_resources: Vec<TrustedResource>,
    ) -> Result<Self, RuntimeError> {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        let mut composition = Composition::new(provider, tools, store);
        composition.trusted_resources = trusted_resources;
        Ok(composition.spawn(session_id, Some(loaded)))
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
/// model's window (14.7.4)? Conservative substring match over the
/// common provider phrasings; unknown phrasings fail visibly instead
/// of triggering a speculative compaction.
fn is_context_overflow(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("context length")
        || lowered.contains("context window")
        || lowered.contains("too many token")
        || lowered.contains("prompt is too long")
}

/// The live, in-memory side of the active operation. Cloned whole to
/// stage a transition: a failed durable commit discards the staged clone
/// and never mutates live state (DESIGN.md §26.2).
#[derive(Clone)]
struct ActiveOperation {
    machine: OperationMachine,
    /// Durable identity of the registry captured for the current model step.
    capability_snapshot: CapabilitySnapshot,
    /// Immutable registry captured for the current model step. Tool calls
    /// are admitted and executed against this registry, never a later live
    /// catalog generation.
    tool_registry: ToolRegistry,
    cancel: CancellationToken,
    state_seq: u64,
    /// The one in-flight effect intent, if any.
    open_effect: Option<EffectRecord>,
    /// Inbox items durably accepted but not yet applied.
    pending_steers: Vec<InboxId>,
}

/// The single-writer owner of one loaded session's mutable live state
/// (DESIGN.md §4.3).
/// The composed collaborators one session runtime runs with (DESIGN.md
/// §4.1): one provider port, one capability snapshot, one store, one
/// approval policy.
/// Runtime-enforced budget bounds (§20.5). `None`/zero means
/// unbounded; exact defaults are configuration, not architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    trusted_resources: Vec<TrustedResource>,
    effect_gate: Option<Arc<EffectGate>>,
    store: SessionStore,
    policy: Arc<dyn PolicyEngine>,
    budget: RuntimeBudget,
    /// Durable lineage for bounded child sessions (§20.3).
    parent: Option<SessionId>,
}

struct SessionRuntime<P> {
    session_id: SessionId,
    runtime_instance_id: RuntimeInstanceId,
    cwd: String,
    provider: Arc<P>,
    /// Authoritative model selection for future steps. The initial id
    /// is in the session row; changes are semantic entries.
    selected_model_ref: String,
    tools: Arc<ToolCatalog>,
    artifact_root: Option<PathBuf>,
    trusted_resources: Vec<TrustedResource>,
    effect_gate: Option<Arc<EffectGate>>,
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
    /// Accepted operations waiting for the single active operation to
    /// reach a terminal outcome. Each remains a complete durable
    /// `Accepted` checkpoint until promotion.
    queued_operations: Vec<ActiveOperation>,
    /// Ephemeral draft of the in-flight model step; never durable.
    draft_text: String,
    /// Live reasoning text for the current step; cleared at settlement.
    /// Display-only: thinking is never durable assistant content.
    draft_thinking: String,
    /// Monotonic frame sequence for the current assistant effect.
    assistant_frame_seq: u64,
    draft_calls: Vec<ToolCall>,
    /// Token usage buffered from the live model step; persisted at the
    /// settlement boundary (DESIGN.md §27.2).
    draft_usage: Option<TokenUsage>,
    /// Full token cost (input + output + cache) of the most recent
    /// settled step; anchors the safety net (14.7.3).
    last_context_tokens: Option<u64>,
    /// Stable-prefix fingerprint used to explain prompt-cache expectations
    /// at the next model-step boundary.
    last_prefix_fingerprint: Option<String>,
    /// Cached model context window (14.8); fetched from the adapter
    /// once, on first use.
    context_window: Option<u64>,
    /// Cached model capability metadata, keyed by the selected model.
    model_capabilities: Option<(String, ModelCapabilities)>,
    /// A user-requested compaction waiting for the run to settle; consumed
    /// at the next continuation boundary.
    pending_compact: Option<Option<String>>,
    /// Reopened Suspended operations awaiting durable settlement
    /// (§9.5); empty unless --resume found one.
    suspended_operations: Vec<(
        OperationId,
        u64,
        crate::store::CheckpointPayload,
        CapabilitySnapshot,
    )>,
    /// An overflow already triggered the one compaction+retry
    /// (14.7.4); a second overflow fails the operation visibly.
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
    /// Persisted indeterminate outcomes that must remain visible to a
    /// frontend attaching after startup recovery.
    indeterminate_warning: Option<IndeterminateWarning>,
    closed: bool,
    /// True when reopened from the store; the session row already exists.
    resumed: bool,
    /// Durable entry count at the reopen boundary for frontend resume
    /// markers. This is presentation metadata, not session authority.
    reopen_entry_count: Option<usize>,
}

impl<P: Provider> SessionRuntime<P> {
    fn new(
        session_id: SessionId,
        runtime_instance_id: RuntimeInstanceId,
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
            trusted_resources,
            effect_gate,
            store,
            policy,
            budget,
            parent,
        } = deps;
        let reopen_entry_count = loaded.as_ref().map(|loaded| loaded.entries.len());
        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let mut runtime = Self {
            session_id,
            runtime_instance_id,
            cwd,
            provider,
            selected_model_ref: initial_model_ref,
            tools,
            artifact_root,
            trusted_resources,
            effect_gate,
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
            queued_operations: Vec::new(),
            draft_text: String::new(),
            draft_thinking: String::new(),
            assistant_frame_seq: 0,
            draft_calls: Vec::new(),
            draft_usage: None,
            last_context_tokens: None,
            last_prefix_fingerprint: None,
            context_window: None,
            model_capabilities: None,
            pending_compact: None,
            suspended_operations: Vec::new(),
            overflow_retry_used: false,
            last_step_was_compaction: false,
            model_step: 0,
            events,
            live_tools: Vec::new(),
            indeterminate_warning: None,
            closed: false,
            resumed: false,
            reopen_entry_count,
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
        let assistant_frames = loaded.assistant_frames;
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
            if matches!(
                payload.state,
                OperationState::Finished(OperationOutcome::Indeterminate)
            ) {
                self.indeterminate_warning = Some(IndeterminateWarning {
                    operation_id: operation.id,
                    message: INDETERMINATE_MESSAGE.to_owned(),
                });
            }
            if matches!(payload.state, OperationState::Finished(_)) {
                // Terminal operations stay in the transcript only.
                continue;
            }
            if matches!(payload.state, OperationState::Suspended) {
                // Suspend is teardown with effects cancelled (§9.4);
                // the operation can never continue. Skip it here; the
                // async recovery pass settles it durably so it cannot
                // block the session forever.
                self.suspended_operations.push((
                    operation.id,
                    state_seq,
                    payload.clone(),
                    operation.capability_snapshot.clone(),
                ));
                continue;
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
            let machine = OperationMachine::restore(
                operation.id,
                payload.prompt.clone(),
                operation.capability_snapshot.tools.clone(),
                payload.state.clone(),
                payload.cancel_requested,
                steers,
            );
            info!(
                session = %self.session_id,
                operation = %operation.id,
                state = ?payload.state,
                "reopened an open operation; recovery is Step 3 work"
            );
            let active = ActiveOperation {
                machine,
                capability_snapshot: operation.capability_snapshot.clone(),
                tool_registry: self.tools.snapshot(),
                cancel: self.cancel_root.child_token(),
                state_seq,
                open_effect: payload.open_effect.clone(),
                pending_steers: loaded
                    .pending_inbox
                    .iter()
                    .filter(|item| item.kind == InboxKind::Steer)
                    .map(|item| item.id)
                    .collect(),
            };
            if matches!(payload.state, OperationState::AssistantEffectPending)
                && let Some(effect_id) = payload.open_effect.as_ref().map(|effect| effect.id)
                && let Some(frame) = assistant_frames.iter().find(|frame| {
                    frame.operation_id == operation.id && frame.effect_id == effect_id
                })
            {
                self.draft_text = frame.text.clone();
                self.draft_thinking = frame.thinking.clone();
                self.assistant_frame_seq = frame.frame_seq;
            }
            if matches!(payload.state, OperationState::Accepted) {
                // Accepted-but-not-started operations are durable queued
                // work, not competing active transition authorities.
                self.queued_operations.push(active);
            } else if self.operation.replace(active).is_some() {
                error!(
                    session = %self.session_id,
                    operation = %operation.id,
                    "multiple non-queued open operations in durable state; refusing to guess"
                );
                self.closed = true;
                return;
            }
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
        if self.operation.is_some() || !self.suspended_operations.is_empty() {
            self.recover_open_operation().await;
        }
        if self.operation.is_none()
            && !self.queued_operations.is_empty()
            && self.promote_next_queued()
        {
            self.advance().await;
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
            SessionCommand::SubmitIfIdle { prompt, reply } => {
                let _ = reply.send(self.submit_if_idle(prompt).await);
                false
            }
            SessionCommand::Enqueue { prompt, reply } => {
                let _ = reply.send(self.enqueue(prompt).await);
                false
            }
            SessionCommand::Steer { text, reply } => {
                let _ = reply.send(self.enqueue_steer(text).await);
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
                    // harness-owned maintenance path.
                    self.pending_compact = Some(instructions);
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

    async fn submit_if_idle(&mut self, prompt: String) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if let Some(active) = &self.operation {
            return Err(CommandError::Busy {
                operation_id: active.machine.operation_id(),
            });
        }
        if let Some(queued) = self.queued_operations.first() {
            return Err(CommandError::Busy {
                operation_id: queued.machine.operation_id(),
            });
        }
        self.accept_operation(prompt).await
    }

    /// Accept one prompt as a distinct durable operation. The operation is
    /// started immediately when the session is idle; otherwise it remains
    /// in `Accepted` state until the current operation settles.
    async fn enqueue(&mut self, prompt: String) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        self.accept_operation(prompt).await
    }

    async fn accept_operation(&mut self, prompt: String) -> Result<OperationId, CommandError> {
        let operation_id = OperationId::generate();
        let is_queued = self.operation.is_some() || !self.queued_operations.is_empty();
        let tool_registry = self.tools.snapshot();
        let (machine, applied) =
            OperationMachine::accept(operation_id, prompt.clone(), tool_registry.specs());
        let capability_snapshot = tool_registry.capability_snapshot();
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
                capability_snapshot_id: capability_snapshot.id.clone(),
                open_effect: None,
            },
            capability_snapshot: capability_snapshot.clone(),
        };
        // Accepted intent is durable before acknowledgment (P4, §9.1).
        self.store
            .begin_operation(self.session_id, operation_id, root_inbox, checkpoint, entry)
            .await
            .map_err(persistence_command_error)?;
        if is_queued {
            self.wait_effect_boundary(EffectBoundary::QueuedAcceptanceCommit)
                .await;
        }

        let active = ActiveOperation {
            machine,
            capability_snapshot,
            tool_registry,
            cancel: self.cancel_root.child_token(),
            state_seq: 1,
            open_effect: None,
            pending_steers: Vec::new(),
        };
        self.entries.extend(applied.entries.iter().cloned());
        self.next_entry_seq += 1;
        if self.operation.is_none() && self.queued_operations.is_empty() {
            self.start_active(active);
            self.advance().await;
        } else {
            self.queued_operations.push(active);
        }
        Ok(operation_id)
    }

    fn start_active(&mut self, active: ActiveOperation) {
        let operation_id = active.machine.operation_id();
        let prompt = active.machine.prompt().to_owned();
        self.operation = Some(active);
        self.draft_text.clear();
        self.draft_thinking.clear();
        self.assistant_frame_seq = 0;
        self.draft_calls.clear();
        self.draft_usage = None;
        self.live_tools.clear();
        self.pending_compact = None;
        self.overflow_retry_used = false;
        self.last_step_was_compaction = false;
        self.model_step = 0;
        self.operation_tool_calls = 0;
        self.emit(RuntimeEvent::OperationStarted {
            cursor: RuntimeCursor::default(),
            operation_id,
            prompt,
        });
    }

    fn promote_next_queued(&mut self) -> bool {
        if self.operation.is_some() {
            return false;
        }
        if self.queued_operations.is_empty() {
            return false;
        }
        let next = self.queued_operations.remove(0);
        self.start_active(next);
        true
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
        self.model_capabilities = None;
        self.last_prefix_fingerprint = None;
        Ok(previous)
    }

    async fn enqueue_steer(&mut self, text: String) -> Result<(), CommandError> {
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
                    kind: InboxKind::Steer,
                    text: text.clone(),
                },
            })
            .expect("inbox apply from an active operation");
        let applied_now = !applied.entries.is_empty();
        let applied_entries = applied.entries.clone();
        let record = InboxRecord {
            id: inbox_id,
            kind: InboxKind::Steer,
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
            staged.pending_steers.push(inbox_id);
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
        self.operation = Some(staged);
        self.wait_effect_boundary(EffectBoundary::CancellationSignal)
            .await;
        self.operation
            .as_ref()
            .expect("cancelled operation installed")
            .cancel
            .cancel();
        Ok(())
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
                    if self.promote_next_queued() {
                        continue;
                    }
                    return;
                }
                OperationState::Accepted | OperationState::NeedAssistant => {
                    if self
                        .operation
                        .as_ref()
                        .is_some_and(|active| active.machine.has_queued_steers())
                    {
                        if !self.drain_queued().await {
                            return;
                        }
                        continue;
                    }
                    if let Some(request) = self.pending_compact.take() {
                        // The run has settled; compact now with the
                        // caller's preservation instructions.
                        if !self.start_compaction(request).await {
                            return;
                        }
                        return;
                    }
                    if self.safety_net_compaction_due() {
                        // §14.7.3: compact at the continuation boundary
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
                        .is_some_and(|active| active.machine.has_queued_steers())
                    {
                        if !self.drain_queued().await {
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

    /// Drain queued steers as one durable transaction at a reasoning
    /// boundary. Returns false when persistence failed.
    async fn drain_queued(&mut self) -> bool {
        let (mut staged, drained, request, new_entry_seq) = {
            let active = self.operation.clone().expect("drain needs an operation");
            let mut staged = active.clone();
            let drained = staged.machine.drain_steers().expect("steer drain");
            let applied_ids = staged
                .pending_steers
                .drain(..drained.len())
                .collect::<Vec<_>>();
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

    /// Project the model-step input from canonical entries and the current
    /// manifest. Compaction is harness-owned; no synthetic model message
    /// is injected into this projection.
    async fn current_model_config(&mut self) -> ModelConfig {
        if self
            .model_capabilities
            .as_ref()
            .is_none_or(|(model_ref, _)| model_ref != &self.selected_model_ref)
        {
            let capabilities = self
                .provider
                .capabilities_for(&self.selected_model_ref)
                .await;
            self.model_capabilities = Some((self.selected_model_ref.clone(), capabilities));
        }
        if self.context_window.is_none() {
            self.context_window = self
                .provider
                .context_window_for(&self.selected_model_ref)
                .await;
        }
        ModelConfig {
            model_ref: self.selected_model_ref.clone(),
            context_window: self.context_window,
            capabilities: self
                .model_capabilities
                .as_ref()
                .expect("model capabilities cached")
                .1,
        }
    }

    fn current_context_manifest(&self) -> (CapabilitySnapshot, ContextManifest) {
        let snapshot = self.tools.snapshot().capability_snapshot();
        let manifest = ContextManifest::new(&snapshot, self.trusted_resources.clone());
        (snapshot, manifest)
    }

    fn cache_expectation(&self, model: &ModelConfig, prefix_fingerprint: &str) -> &'static str {
        if !model.capabilities.prompt_cache {
            return "unsupported";
        }
        match self.last_prefix_fingerprint.as_deref() {
            None => "cold_start",
            Some(previous) if previous == prefix_fingerprint => "prefix_reuse_expected",
            Some(_) => "prefix_changed",
        }
    }

    async fn project_model_step_plan(
        &mut self,
        manifest: &ContextManifest,
    ) -> crate::context::ContextPlan {
        let _ = self.current_model_config().await;
        project_with_manifest(&self.entries, self.first_entry_seq(), manifest)
    }

    async fn wait_effect_boundary(&self, boundary: EffectBoundary) {
        if let Some(gate) = &self.effect_gate {
            gate.wait(boundary).await;
        }
    }

    /// Safety-net compaction check (§14.7.3), evaluated at
    /// continuation boundaries only: compact when the measured context
    /// is within the reserve of the model's actual window. A fixed
    /// token threshold would be wrong for every model except the one
    /// it was tuned for. Unknown windows disable the net; overflow
    /// recovery (14.7.4) is the backstop.
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
        let (_, manifest) = self.current_context_manifest();
        let mut plan = project_with_manifest(&self.entries, self.first_entry_seq(), &manifest);
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
        self.last_prefix_fingerprint = None;
        info!(%operation_id, "starting automatic compaction");
        self.wait_effect_boundary(EffectBoundary::CompactionExecution)
            .await;
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
        let (_, planning_manifest) = self.current_context_manifest();
        let plan = self.project_model_step_plan(&planning_manifest).await;
        let model = self.current_model_config().await;
        let mut staged = self.operation.clone().expect("step needs an operation");
        let step_registry = self.tools.snapshot();
        let capability_snapshot = step_registry.capability_snapshot();
        staged
            .machine
            .set_step_tools(capability_snapshot.tools.clone());
        staged.tool_registry = step_registry;
        staged.capability_snapshot = capability_snapshot.clone();
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
        let context_manifest =
            ContextManifest::new(&capability_snapshot, self.trusted_resources.clone());
        let prefix_fingerprint = context_manifest.stable_prefix_fingerprint(&model.model_ref);
        let cache_expectation = self.cache_expectation(&model, &prefix_fingerprint);
        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "model_step".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({
                "step": self.model_step + 1,
                "model": model,
                "plan": plan,
                "capability_snapshot_id": capability_snapshot.id,
                "context_manifest_id": context_manifest.id,
                "prefix_fingerprint": prefix_fingerprint,
                "cache_expectation": cache_expectation
            }),
            attempt: 1,
        };
        // The pending effect is part of the checkpoint: it must be on the
        // staged operation before the commit is built.
        staged.open_effect = Some(effect.clone());
        let (mut request, new_entry_seq) = build_commit_request(
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
        request.context_manifests.push(context_manifest);
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return false;
        }
        self.wait_effect_boundary(EffectBoundary::ModelExecution)
            .await;
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.operation = Some(staged);
        self.last_prefix_fingerprint = Some(prefix_fingerprint);
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
        let active_capability = self
            .operation
            .as_ref()
            .and_then(|active| active.capability_snapshot.identity(&call.name))
            .map(str::to_owned);
        let current_capability = self
            .tools
            .snapshot()
            .capability_snapshot()
            .identity(&call.name)
            .map(str::to_owned);
        let step_tools = self
            .operation
            .as_ref()
            .map(|active| active.tool_registry.clone())
            .expect("admit needs the current step tool registry");
        let canonical = if active_capability == current_capability {
            step_tools.canonicalize(&call.name, &call.arguments)
        } else {
            Err(format!("capability `{}` is no longer available", call.name))
        };
        let decision = match &canonical {
            // Delegation is a structural capability (§20.4): every
            // effect a child can produce is individually gated inside the
            // child, so spawning one needs no grant.
            Ok(_) if call.name == "delegate" => PolicyDecision::Allow,
            Ok(target) => self.policy.decide(&call.name, target),
            // Canonicalization failure is a model-visible denial, not a
            // harness failure: the model produced an unusable input.
            Err(message) => PolicyDecision::Deny(message.clone()),
        };
        // Tool-call budget (§20.5): spent budget denies further calls
        // model-visibly; the model can still finish its turn.
        let over_tool_budget = self
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
            PolicyDecision::Allow => step_tools.validate(&call.name, &call.arguments).err(),
            PolicyDecision::ApprovalRequired => unreachable!("handled above"),
        };
        // §12.3: file-mutating effects persist reconciliation evidence
        // with the intent, before execution. An evidence failure means
        // the invocation could not be classified, so it is denied
        // model-visibly instead of admitted blind.
        let evidence = if denial.is_none() && matches!(call.name.as_str(), "write" | "edit") {
            match crate::tool::reconciliation_evidence(
                step_tools.cwd(),
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
            recovery_class: step_tools.recovery_class(&call.name),
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
            let _ = self.tool_tx.try_send(ToolSignal::Settled {
                effect_id,
                result: ToolResult::Err {
                    call_id: call.call_id,
                    error: message,
                    artifact: None,
                },
            });
        } else {
            self.wait_effect_boundary(EffectBoundary::ToolExecution)
                .await;
            self.operation_tool_calls += 1;
            let target = target_summary_registry(&step_tools, &call.name, &call.arguments);
            self.emit_tool_started(call.operation_id, call.call_id, &call.name, target);
            let reconciliation = self
                .operation
                .as_ref()
                .and_then(|active| active.open_effect.as_ref())
                .and_then(|effect| effect.effective_input.get("reconciliation"))
                .cloned();
            let effect_id = self
                .operation
                .as_ref()
                .and_then(|active| active.open_effect.as_ref().map(|effect| effect.id));
            self.spawn_tool_effect(effect_id, call, reconciliation, step_tools);
        }
        true
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
                    self.indeterminate_warning = Some(IndeterminateWarning {
                        operation_id,
                        message: INDETERMINATE_MESSAGE.to_owned(),
                    });
                    self.emit(RuntimeEvent::OperationIndeterminate {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        message: INDETERMINATE_MESSAGE.to_owned(),
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
            runtime_instance_id: self.runtime_instance_id,
            indeterminate: self.indeterminate_warning.clone(),
            reopen_entry_count: self.reopen_entry_count,
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
        let close_gate = self.effect_gate.clone();
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
            if let Some(gate) = close_gate {
                gate.wait(EffectBoundary::CloseSuspendCommit).await;
            }
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
        | RuntimeEvent::OperationIndeterminate { cursor: slot, .. }
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
        RuntimeEvent::OperationIndeterminate { .. } => "operation_indeterminate",
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
            .try_send(SessionCommand::SubmitIfIdle {
                prompt: String::from("fill"),
                reply,
            })
            .map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
                mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
            })
    }
}
