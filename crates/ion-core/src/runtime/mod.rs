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

use std::collections::{BTreeMap, HashMap};
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
    CapabilitySnapshot, ContextManifest, ContextPlan, TrustedResource,
    project_with_manifest_for_model,
};
use crate::effect::{
    CacheExpectation, CompactionInvocation, DurableEffect, ModelStepPlan, ToolInvocation,
};
use crate::error::{CommandError, RuntimeError};
use crate::harness::HarnessProfile;
use crate::ids::{
    AgentId, EffectId, EntryId, InboxId, OperationId, RuntimeCursor, RuntimeInstanceId, SessionId,
};
use crate::operation::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
use crate::policy::{DefaultPolicy, PolicyDecision, PolicyEngine};
use crate::provider::{
    EngineSignal, ModelCapabilities, ModelConfig, ModelPricing, Provider, ProviderRequest,
    TokenUsage,
};
use crate::store::{
    AssistantFrame, CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord,
    InboxRecord, InboxStatus, LoadedSession, SessionRecord, SessionStore, SettledEffect,
    StoreError, ToolProgressCheckpoint, UsageRecord,
};
use crate::tool::{
    PolicyRoute, RecoveryClass, ResolvedInvocation, ToolCall, ToolCatalog, ToolProgress,
    ToolRegistry, ToolResult, ToolSelection, ToolSpec,
};

mod effects;
mod persistence;
mod recovery;
use persistence::build_commit_request;

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
    PendingNextRunCommit,
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
        operation_id: OperationId,
        effect_id: EffectId,
        call_id: u64,
        output: String,
    },
    Settled {
        operation_id: OperationId,
        effect_id: EffectId,
        result: ToolResult,
    },
}

/// Signals from a running user shell passthrough, drained by the session
/// loop: streamed progress for display, then the bounded settlement.
enum ShellSignal {
    Output {
        lane_name: String,
        output: String,
    },
    Settled {
        lane_name: String,
        command: String,
        outcome: crate::tool::ToolOutcome,
        cancelled: bool,
        exclude_from_context: bool,
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
/// Best-effort proposed-change preview for an `edit` awaiting
/// approval: a bounded unified hunk against the file's current
/// content, sharing the settled outcome's implementation. Display-only
/// and read-only; `None` when the arguments are unusable or the file
/// cannot be read (the denial surfaces through the normal path).
async fn edit_approval_preview(
    tools: &ToolRegistry,
    arguments: &serde_json::Value,
) -> Option<String> {
    let path = arguments.get("path")?.as_str()?;
    let old_str = arguments.get("old_str")?.as_str()?;
    let new_str = arguments
        .get("new_str")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let original = crate::tool::read_secure_text(tools.cwd(), std::path::Path::new(path), false)
        .await
        .ok()?;
    crate::tool::edit_diff_hunk(path, &original, old_str, new_str, 3, 24)
}

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
    /// Bounded live output from a running tool. The latest checkpoint is
    /// persisted before this presentation event is emitted; it is never a
    /// semantic result or completion signal.
    ToolProgress {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        call_id: u64,
        output: String,
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
    /// A user shell passthrough started (pi parity: `!`/`!!`). Display
    /// only; the settled entry is the durable truth.
    ShellStarted {
        cursor: RuntimeCursor,
        lane_name: String,
        command: String,
        exclude_from_context: bool,
    },
    /// Bounded live output from a running user shell passthrough. Never
    /// a semantic result or completion signal.
    ShellOutput {
        cursor: RuntimeCursor,
        lane_name: String,
        output: String,
    },
    /// A user shell passthrough settled durably. Emitted after the
    /// ShellExecution entry commits, so subscribers see completion
    /// exactly when it is durable. `output_preview` is a bounded tail
    /// for frontend rendering; the entry is the semantic truth.
    ShellSettled {
        cursor: RuntimeCursor,
        lane_name: String,
        command: String,
        exit_code: Option<i64>,
        cancelled: bool,
        exclude_from_context: bool,
        output_preview: Option<String>,
    },
    /// Provider-reported usage for the current model step. This is a
    /// display event; the durable usage row is still committed only with
    /// model-step settlement. `step` identifies the model step so a
    /// frontend can replace (not re-add) a replayed usage event.
    UsageUpdate {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        step: u64,
        usage: TokenUsage,
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
    /// needed an approval no one can grant in this mode, so the operation
    /// terminates visibly instead of inviting a model retry loop.
    OperationApprovalRequired {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        tool: String,
    },
    /// The operation parked for an interactive approval before any
    /// effect intent was committed (DESIGN.md §17.4). The staged
    /// invocation is durable in the operation state; the decision
    /// arrives through `SessionHandle::decide_approval`.
    ApprovalPending {
        cursor: RuntimeCursor,
        operation_id: OperationId,
        tool: String,
        /// Short canonical-target summary for display (path or command).
        target: Option<String>,
        /// Bounded proposed-change preview for display only (currently
        /// a unified hunk for `edit`, computed against the file's
        /// current content; `None` for other tools or when the file
        /// cannot be read). Never semantic: the decision re-resolves
        /// the call against live state.
        preview: Option<String>,
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
            | Self::ToolProgress { operation_id, .. }
            | Self::ToolSettled { operation_id, .. }
            | Self::UsageUpdate { operation_id, .. }
            | Self::OperationFinished { operation_id, .. }
            | Self::OperationFailed { operation_id, .. }
            | Self::OperationIndeterminate { operation_id, .. }
            | Self::OperationCancelled { operation_id, .. }
            | Self::OperationApprovalRequired { operation_id, .. }
            | Self::ApprovalPending { operation_id, .. } => Some(*operation_id),
            Self::ShellStarted { .. }
            | Self::ShellOutput { .. }
            | Self::ShellSettled { .. }
            | Self::SessionClosed { .. } => None,
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> RuntimeCursor {
        match self {
            Self::OperationStarted { cursor, .. }
            | Self::AssistantTextDelta { cursor, .. }
            | Self::ThinkingDelta { cursor, .. }
            | Self::ToolStarted { cursor, .. }
            | Self::ToolProgress { cursor, .. }
            | Self::ToolSettled { cursor, .. }
            | Self::UsageUpdate { cursor, .. }
            | Self::OperationFinished { cursor, .. }
            | Self::OperationFailed { cursor, .. }
            | Self::OperationIndeterminate { cursor, .. }
            | Self::OperationCancelled { cursor, .. }
            | Self::OperationApprovalRequired { cursor, .. }
            | Self::ApprovalPending { cursor, .. }
            | Self::ShellStarted { cursor, .. }
            | Self::ShellOutput { cursor, .. }
            | Self::ShellSettled { cursor, .. }
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

/// Most recently settled operation on the public `main` projection. A
/// frontend that loses the terminal event can still classify the durable
/// outcome without reaching through the runtime into the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSettlement {
    pub operation_id: OperationId,
    pub outcome: OperationOutcome,
}

/// One user-initiated shell passthrough (pi parity: `!command`
/// output joins the model projection; `!!command` output stays
/// durable but excluded). The reply returns Ok once the run's
/// durable intent is committed and the process spawned; completion
/// is observed through `ShellSettled` events and the durable
/// entry. Only an idle lane accepts one: an active operation owns
/// the branch, and shell output must not race its tool effects./// Snapshot-plus-cursor view of one session (DESIGN.md §21.2). The
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
    /// Latest durable terminal outcome on `main`, retained independently of
    /// whether its live terminal event is still present in the bounded ring.
    pub latest_settlement: Option<OperationSettlement>,
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
    /// The durable thinking-level selection for future steps; `None` is
    /// the adapter default. Authoritative across resume like
    /// `model_ref`.
    pub thinking: Option<String>,
    /// Durable lane-owned next-run input awaiting promotion (§9.2):
    /// the queued prompt plus its provisioned entry identity. `None`
    /// when the lane has no pending input.
    pub pending_next_run: Option<NextRunInput>,
    /// Most recently settled model-step usage. This is a bounded projection
    /// of the durable usage ledger for frontend resynchronization; it is not
    /// a cost estimate or a replacement for the ledger.
    pub latest_usage: Option<TokenUsage>,
    /// Session-lifetime token totals from the durable usage ledger
    /// (pi-parity footer stats): the cumulative `↑ ↓ R W` figures.
    pub usage_totals: TokenUsage,
    /// Published per-million-token pricing for the session's current
    /// model (pi-parity cost footer). `None` hides the cost segment.
    pub model_pricing: Option<ModelPricing>,
    /// Cached context-window hint for the session's current model;
    /// `None` hides the context-percentage segment.
    pub context_window: Option<u64>,
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

/// Durable lane-owned next-run input as exposed to frontends: the
/// provisioned entry identity plus the queued prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextRunInput {
    pub entry_id: crate::ids::EntryId,
    pub prompt: String,
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
    CreateLane {
        lane_name: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    AdmitAgentLane {
        agent_id: AgentId,
        control_parent_id: AgentId,
        source_lane_name: String,
        reply: oneshot::Sender<Result<String, CommandError>>,
    },
    AdmitStructuralScope {
        scope: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    SubmitIfIdle {
        lane_name: String,
        prompt: String,
        reply: oneshot::Sender<Result<OperationId, CommandError>>,
    },
    NextRun {
        lane_name: String,
        prompt: String,
        reply: oneshot::Sender<Result<crate::ids::EntryId, CommandError>>,
    },
    /// Remove the lane's pending next-run input and return it (pi
    /// parity: alt+up dequeues a queued prompt back into the editor).
    /// The cleared state is durable before the command returns.
    DequeueNextRun {
        lane_name: String,
        reply: oneshot::Sender<Result<Option<String>, CommandError>>,
    },
    /// One user-initiated shell passthrough (pi parity: `!command`
    /// output joins the model projection; `!!command` output stays
    /// durable but excluded). The reply returns Ok once the run's
    /// durable intent is committed and the process spawned; completion
    /// is observed through `ShellSettled` events and the durable
    /// entry. Only an idle lane accepts one: an active operation owns
    /// the branch, and shell output must not race its tool effects.
    RunShell {
        lane_name: String,
        command: String,
        exclude_from_context: bool,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    /// Cancel the running user shell passthrough, if any. The settled
    /// entry still lands durably with `cancelled: true`.
    CancelShell {
        reply: oneshot::Sender<Result<bool, CommandError>>,
    },
    Steer {
        text: String,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    SendAgentMessage {
        from: AgentId,
        lane_name: String,
        text: String,
        reply: oneshot::Sender<Result<OperationId, CommandError>>,
    },
    Cancel {
        operation_id: OperationId,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    /// A durable approval decision for a parked operation (DESIGN.md
    /// §17.4). Acceptance is durable before the command returns.
    DecideApproval {
        operation_id: OperationId,
        allow: bool,
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
    /// User-requested compaction: honored at the next continuation
    /// boundary of the active operation. Ok(false) = idle, nothing to
    /// compact (compaction runs within an operation, §14.7).
    Compact {
        instructions: Option<String>,
        reply: oneshot::Sender<Result<bool, CommandError>>,
    },
    /// Set the lane's thinking level for future model steps (pi parity:
    /// /thinking, shift+tab). Durable before the command returns; the
    /// active operation keeps its frozen per-step selection.
    SwitchThinking {
        lane_name: String,
        thinking: Option<String>,
        reply: oneshot::Sender<Result<Option<String>, CommandError>>,
    },
    SwitchModel {
        lane_name: String,
        model_ref: String,
        reply: oneshot::Sender<Result<String, CommandError>>,
    },
    Subscribe {
        reply: oneshot::Sender<SubscribeReply>,
    },
    SubscribeAll {
        reply: oneshot::Sender<SubscribeReply>,
    },
    Close {
        reply: oneshot::Sender<Result<(), CommandError>>,
    },
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
    /// Create a shared-history lane at the main lane's current durable leaf.
    /// The new lane inherits the main lane's current model configuration.
    pub async fn create_lane(&self, lane_name: impl Into<String>) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::CreateLane {
                lane_name: lane_name.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    pub(crate) async fn admit_agent_lane(
        &self,
        agent_id: AgentId,
        control_parent_id: AgentId,
        source_lane_name: impl Into<String>,
    ) -> Result<String, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::AdmitAgentLane {
                agent_id,
                control_parent_id,
                source_lane_name: source_lane_name.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    pub(crate) async fn admit_structural_scope(
        &self,
        scope: impl Into<String>,
    ) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::AdmitStructuralScope {
                scope: scope.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Accept a prompt durably on main and open a new operation only when idle.
    pub async fn submit_if_idle(
        &self,
        prompt: impl Into<String>,
    ) -> Result<OperationId, CommandError> {
        self.submit_if_idle_on_lane(crate::session::lane::MAIN, prompt)
            .await
    }

    /// Accept a prompt durably on one named lane only when that lane is idle.
    pub async fn submit_if_idle_on_lane(
        &self,
        lane_name: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<OperationId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SubmitIfIdle {
                lane_name: lane_name.into(),
                prompt: prompt.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Persist main's next-run input. If main is idle it is accepted
    /// immediately; otherwise only its semantic entry identity is reserved.
    pub async fn next_run(
        &self,
        prompt: impl Into<String>,
    ) -> Result<crate::ids::EntryId, CommandError> {
        self.next_run_on_lane(crate::session::lane::MAIN, prompt)
            .await
    }

    /// Persist one named lane's next-run input. Operation identity is created
    /// only when that lane actually accepts the run.
    pub async fn next_run_on_lane(
        &self,
        lane_name: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Result<crate::ids::EntryId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::NextRun {
                lane_name: lane_name.into(),
                prompt: prompt.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Run one user shell passthrough on the main lane (pi parity:
    /// `!command` / `!!command`). Returns Ok once the run's durable
    /// intent is committed and the process spawned; the settled entry
    /// and `ShellSettled` event carry the outcome.
    pub async fn run_shell(
        &self,
        command: impl Into<String>,
        exclude_from_context: bool,
    ) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::RunShell {
                lane_name: crate::session::lane::MAIN.to_owned(),
                command: command.into(),
                exclude_from_context,
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Cancel the running user shell passthrough (pi parity: esc while
    /// `!`/`!!` runs). The settlement entry still lands durably with
    /// `cancelled: true`; returns false when nothing is running.
    pub async fn cancel_shell(&self) -> Result<bool, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::CancelShell { reply })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Remove the main lane's pending next-run input and return its
    /// prompt (pi parity: alt+up dequeues a queued prompt back to the
    /// editor). Durable before the command returns; `Ok(None)` when no
    /// input is queued.
    pub async fn dequeue_next_run(&self) -> Result<Option<String>, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::DequeueNextRun {
                lane_name: crate::session::lane::MAIN.to_owned(),
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

    pub(crate) async fn send_agent_message(
        &self,
        from: AgentId,
        lane_name: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<OperationId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SendAgentMessage {
                from,
                lane_name: lane_name.into(),
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

    /// Durably select the model used by future main-lane model steps.
    /// A running step keeps its frozen model snapshot. Returns the previous id.
    pub async fn switch_model(&self, model_ref: impl Into<String>) -> Result<String, CommandError> {
        self.switch_model_on_lane(crate::session::lane::MAIN, model_ref)
            .await
    }

    /// Durably select the model used by future steps on one named lane.
    /// Set the main lane's thinking level for future model steps (pi
    /// parity: /thinking, shift+tab). Durable before return; `None`
    /// restores the adapter default. Returns the previous selection.
    pub async fn switch_thinking(
        &self,
        thinking: Option<String>,
    ) -> Result<Option<String>, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SwitchThinking {
                lane_name: crate::session::lane::MAIN.to_owned(),
                thinking,
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    pub async fn switch_model_on_lane(
        &self,
        lane_name: impl Into<String>,
        model_ref: impl Into<String>,
    ) -> Result<String, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SwitchModel {
                lane_name: lane_name.into(),
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

    /// Decide one parked approval (DESIGN.md §17.4). `allow` commits the
    /// staged tool effect intent and executes it; denial records a
    /// model-visible denial result and continues the operation. The
    /// decision is durable before this returns.
    pub async fn decide_approval(
        &self,
        operation_id: OperationId,
        allow: bool,
    ) -> Result<(), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::DecideApproval {
                operation_id,
                allow,
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

    /// Main-lane snapshot plus main-lane bounded live events (DESIGN.md
    /// §16.1). A frontend that falls behind resynchronizes from a fresh
    /// snapshot; sibling-lane work cannot pollute or overflow this event ring.
    pub async fn subscribe(&self) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::Subscribe { reply })
            .map_err(command_send_error)?;
        let (snapshot, events) = rx.await.map_err(|_| CommandError::RuntimeDropped)??;
        Ok((snapshot, events))
    }

    /// Session-wide event observation for the family controller. The snapshot
    /// remains the public main-lane projection; internal callers use the event
    /// stream only for exact operation-addressed waits across shared lanes.
    pub(crate) async fn subscribe_all(
        &self,
    ) -> Result<(SessionSnapshot, EventSubscription), CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SubscribeAll { reply })
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
    interactive_approvals: bool,
    budget: RuntimeBudget,
    parent: Option<SessionId>,
    fork_source: Option<(SessionId, Option<EntryId>)>,
    trusted_resources: Vec<TrustedResource>,
    effect_gate: Option<Arc<EffectGate>>,
    /// Host-selected workspace identity. A reopened session uses the
    /// persisted value instead, so process cwd cannot silently change it.
    cwd: Option<String>,
    defer_loaded_start: bool,
}

impl<P: Provider> Composition<P> {
    fn new(provider: P, tools: impl Into<ToolCatalog>, store: SessionStore) -> Self {
        Self {
            provider,
            tools: tools.into(),
            store,
            policy: Arc::new(DefaultPolicy),
            interactive_approvals: false,
            budget: RuntimeBudget::unbounded(),
            parent: None,
            fork_source: None,
            trusted_resources: Vec::new(),
            effect_gate: None,
            cwd: None,
            defer_loaded_start: false,
        }
    }

    fn spawn(mut self, session_id: SessionId, loaded: Option<LoadedSession>) -> Runtime {
        let initial_model_ref = self.provider.initial_model_ref();
        let deferred_loaded_start = self.defer_loaded_start && loaded.is_some();
        let runtime_instance_id = RuntimeInstanceId::generate();
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let session = SessionHandle { tx };
        let provider = Arc::new(self.provider);
        let runtime_tools = self.tools.clone();
        let tools = Arc::new(self.tools);
        let runtime_store = self.store.clone();
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
                    interactive_approvals: self.interactive_approvals,
                    budget: self.budget,
                    parent: self.parent,
                    fork_source: self.fork_source,
                    defer_loaded_start: deferred_loaded_start,
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
            store: runtime_store,
            tools: runtime_tools,
            deferred_loaded_start,
            join,
        }
    }
}

/// Process-level runtime: composition and the session registry. v0 keeps
/// exactly one loaded session (DESIGN.md §32 Step 1).
pub struct Runtime {
    session: SessionHandle,
    session_id: SessionId,
    store: SessionStore,
    tools: ToolCatalog,
    deferred_loaded_start: bool,
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

    #[cfg(test)]
    pub(crate) fn start_interactive_with_effect_gate(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        gate: EffectGate,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.interactive_approvals = true;
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

    /// Compose an interactive runtime (DESIGN.md §17.4): approval-required
    /// calls park the operation for a durable decision via
    /// [`SessionHandle::decide_approval`] instead of terminating it.
    /// Non-interactive hosts use [`Self::start_with_policy_and_resources`],
    /// which keeps fail-closed termination.
    #[must_use]
    pub fn start_interactive(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        policy: Arc<dyn PolicyEngine>,
        trusted_resources: Vec<TrustedResource>,
    ) -> Self {
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.interactive_approvals = true;
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
        Self::open_session_with_resources(
            provider,
            tools,
            store,
            session_id,
            Arc::new(DefaultPolicy),
            Vec::new(),
        )
        .await
    }

    /// Reopen a session with the host's explicit approval policy and
    /// trusted-resource snapshot. The policy must match the one the
    /// session ran under; a resumed session never silently widens or
    /// narrows its grants (DESIGN.md §17).
    pub async fn open_session_with_resources(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        session_id: SessionId,
        policy: Arc<dyn PolicyEngine>,
        trusted_resources: Vec<TrustedResource>,
    ) -> Result<Self, RuntimeError> {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.trusted_resources = trusted_resources;
        Ok(composition.spawn(session_id, Some(loaded)))
    }

    /// Reopen a separately hosted agent session with its durable lineage and budget.
    /// The loaded session remains the source of workspace/model/topology state;
    /// the host supplies only live provider, policy, and resource dependencies.
    pub async fn open_hosted(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        session_id: SessionId,
        config: HostedRuntimeConfig,
    ) -> Result<Self, RuntimeError> {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        if loaded.session.control_parent_session_id != Some(config.control_parent) {
            return Err(RuntimeError::OperationFailed(
                "hosted agent session does not belong to the requested family root".to_owned(),
            ));
        }
        let fork_source = loaded
            .session
            .fork_source_session_id
            .map(|source| (source, loaded.session.fork_source_entry_id));
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = config.policy;
        composition.budget = config.budget;
        composition.parent = Some(config.control_parent);
        composition.fork_source = fork_source;
        composition.trusted_resources = config.trusted_resources;
        Ok(composition.spawn(session_id, Some(loaded)))
    }

    /// Reopen an interactive session (DESIGN.md §17.4): a parked
    /// approval re-surfaces for a durable decision instead of
    /// terminating the operation.
    pub async fn open_interactive(
        provider: impl Provider,
        tools: impl Into<ToolCatalog>,
        store: SessionStore,
        session_id: SessionId,
        policy: Arc<dyn PolicyEngine>,
        trusted_resources: Vec<TrustedResource>,
    ) -> Result<Self, RuntimeError> {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        let mut composition = Composition::new(provider, tools, store);
        composition.policy = policy;
        composition.interactive_approvals = true;
        composition.trusted_resources = trusted_resources;
        composition.defer_loaded_start = true;
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

    /// Attach the family-scoped agent authority for this durable root.
    /// Existing retained identities and active descendant residency are
    /// reconstructed from the store before the controller is returned.
    pub async fn agent_family(
        &self,
        max_active: usize,
    ) -> Result<crate::agent::Family, crate::agent::Error> {
        if self.deferred_loaded_start {
            crate::agent::Family::attach_durable(
                self.session_id,
                self.session.clone(),
                self.store.clone(),
                max_active,
            )
            .await
        } else {
            crate::agent::Family::attach(
                self.session_id,
                self.session.clone(),
                self.store.clone(),
                max_active,
            )
            .await
        }
    }

    pub(crate) async fn admit_structural_scope(
        &self,
        scope: &str,
    ) -> Result<(), crate::agent::Error> {
        if self.deferred_loaded_start {
            // The session task has not restored its loaded copy yet. Update the
            // durable lanes directly; its first command reloads this state before
            // recovery, so no stale in-memory grant can win.
            let mut loaded = self.store.load(self.session_id).await?;
            let published = self.tools.admission_scopes();
            for lane in &mut loaded.lanes {
                let materialized = lane.config.scopes.materialize(&published);
                let inserted = lane.name == crate::session::lane::MAIN
                    && lane.config.scopes.insert(scope.to_owned());
                if materialized || inserted {
                    self.store
                        .set_lane_config(self.session_id, &lane.name, lane.config.clone())
                        .await?;
                }
            }
            return Ok(());
        }
        self.session.admit_structural_scope(scope).await?;
        Ok(())
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

/// One fully prepared post-policy tool admission. Resolution and
/// reconciliation are bundled so the durable commit boundary cannot receive
/// mismatched canonical/recovery/evidence/denial state.
enum PreparedToolAdmission {
    Execute {
        resolved: ResolvedInvocation,
        reconciliation: Option<serde_json::Value>,
    },
    Deny {
        resolved: Option<ResolvedInvocation>,
        message: String,
    },
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
    pending_inputs: Vec<InboxId>,
}

/// Ephemeral execution state owned by the currently resident operation.
/// Keeping these fields together is a prerequisite for operation-addressed
/// residency: no draft, step counter, budget counter, or live tool state may
/// remain session-global once more than one lane can execute concurrently.
#[derive(Debug, Default)]
struct OperationResidency {
    operation_tool_calls: u32,
    draft_text: String,
    draft_thinking: String,
    assistant_frame_seq: u64,
    draft_calls: Vec<ToolCall>,
    draft_usage: Option<TokenUsage>,
    pending_compact: Option<Option<String>>,
    overflow_retry_used: bool,
    last_step_was_compaction: bool,
    model_step: u64,
    live_tools: Vec<PendingTool>,
}

struct ResidentOperation {
    lane_name: String,
    active: ActiveOperation,
    live: OperationResidency,
}

impl ResidentOperation {
    fn new(lane_name: impl Into<String>, active: ActiveOperation) -> Self {
        Self {
            lane_name: lane_name.into(),
            active,
            live: OperationResidency::default(),
        }
    }
}

#[derive(Debug, Default)]
struct LaneResidency {
    last_context_tokens: Option<u64>,
    last_prefix_fingerprint: Option<String>,
    latest_usage: Option<TokenUsage>,
    context_window: Option<u64>,
    model_capabilities: Option<(String, ModelCapabilities)>,
    /// Cached per-model published pricing, resolved once per model
    /// selection like `model_capabilities`.
    model_pricing: Option<ModelPricing>,
}

struct ResidentLane {
    durable: crate::session::lane::Lane,
    live: LaneResidency,
}

impl ResidentLane {
    fn new(durable: crate::session::lane::Lane) -> Self {
        Self {
            durable,
            live: LaneResidency::default(),
        }
    }
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

/// Host dependencies needed to reopen a separately hosted agent runtime.
#[derive(Clone)]
pub struct HostedRuntimeConfig {
    pub policy: Arc<dyn PolicyEngine>,
    pub budget: RuntimeBudget,
    pub control_parent: SessionId,
    pub trusted_resources: Vec<TrustedResource>,
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
    /// This host can grant approvals interactively: approval-required
    /// calls park the operation for a durable decision instead of
    /// terminating it (DESIGN.md §17.4).
    interactive_approvals: bool,
    budget: RuntimeBudget,
    /// Durable control lineage for separately hosted descendants.
    parent: Option<SessionId>,
    /// Explicit history lineage; independent from control parentage.
    fork_source: Option<(SessionId, Option<EntryId>)>,
    defer_loaded_start: bool,
}

struct SessionRuntime<P> {
    session_id: SessionId,
    runtime_instance_id: RuntimeInstanceId,
    cwd: String,
    provider: Arc<P>,
    tools: Arc<ToolCatalog>,
    artifact_root: Option<PathBuf>,
    trusted_resources: Vec<TrustedResource>,
    effect_gate: Option<Arc<EffectGate>>,
    store: SessionStore,
    policy: Arc<dyn PolicyEngine>,
    /// This host can grant approvals interactively (§17.4).
    interactive_approvals: bool,
    budget: RuntimeBudget,
    control_parent_session_id: Option<SessionId>,
    fork_source_session_id: Option<SessionId>,
    fork_source_entry_id: Option<EntryId>,
    commands: mpsc::Receiver<SessionCommand>,
    engine_tx: mpsc::Sender<EngineSignal>,
    engine_rx: mpsc::Receiver<EngineSignal>,
    tool_tx: mpsc::Sender<ToolSettlement>,
    tool_rx: mpsc::Receiver<ToolSettlement>,
    shell_tx: mpsc::Sender<ShellSignal>,
    shell_rx: mpsc::Receiver<ShellSignal>,
    /// The running passthrough's cancel token, when one is in flight.
    shell_cancel: Option<tokio_util::sync::CancellationToken>,
    cancel_root: CancellationToken,
    tracker: TaskTracker,
    cursor: RuntimeCursor,
    /// Full canonical conversation tree in global durable sequence order.
    tree_entries: Vec<EntryRecord>,
    /// Derived lookup index for walking parent-linked lane branches without
    /// turning context projection into an O(n²) scan. `tree_entries` remains
    /// the authority; this index is rebuilt on reopen and extended on commit.
    entry_index: HashMap<EntryId, usize>,
    /// Durable lane projections paired with lane-relative live cache and
    /// telemetry observations. Public commands still address `main` until the
    /// lane command surface lands.
    lanes: BTreeMap<String, ResidentLane>,
    /// Next session-global durable entry sequence.
    next_entry_seq: u64,
    /// Live operation residency keyed by durable operation identity.
    operations: HashMap<OperationId, ResidentOperation>,
    /// Reopened Suspended operations awaiting durable settlement
    /// (§9.5); empty unless --resume found one.
    suspended_operations: Vec<(
        OperationId,
        u64,
        crate::store::CheckpointPayload,
        CapabilitySnapshot,
    )>,
    /// Session-wide event ring used only by operation-addressed family waits.
    events: broadcast::Sender<RuntimeEvent>,
    /// Main-lane event ring paired with the public main-lane snapshot.
    main_events: broadcast::Sender<RuntimeEvent>,
    /// Most recently settled main-lane operation. Unlike the event ring this
    /// is retained until a newer main operation settles, so lag recovery can
    /// classify a missed terminal event.
    main_latest_settlement: Option<OperationSettlement>,
    /// Main-lane indeterminate outcome that must remain visible to a
    /// frontend attaching after startup recovery. Shared-history worker
    /// outcomes remain observable through Family/durable operation state and
    /// must not leak into the public main-lane snapshot.
    main_indeterminate_warning: Option<IndeterminateWarning>,
    closed: bool,
    /// True when reopened from the store; the session row already exists.
    resumed: bool,
    loaded: Option<LoadedSession>,
    defer_loaded_start: bool,
    /// Durable entry count at the reopen boundary for frontend resume
    /// markers. This is presentation metadata, not session authority.
    reopen_entry_count: Option<usize>,
    /// Session-lifetime token totals from the durable usage ledger
    /// (pi-parity footer stats). Maintained in memory as settled usage
    /// commits; reloaded exactly at reopen.
    usage_totals: TokenUsage,
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
            interactive_approvals,
            budget,
            parent,
            fork_source,
            defer_loaded_start,
        } = deps;
        let (engine_tx, engine_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (tool_tx, tool_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (shell_tx, shell_rx) = mpsc::channel(ENGINE_CAPACITY);
        let (events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let (main_events, _) = broadcast::channel(SUBSCRIBER_CAPACITY);
        let mut lanes = BTreeMap::new();
        lanes.insert(
            crate::session::lane::MAIN.to_owned(),
            ResidentLane::new(crate::session::lane::Lane {
                name: crate::session::lane::MAIN.to_owned(),
                state: crate::session::lane::State {
                    leaf: None,
                    current_operation: None,
                    pending_next_run: None,
                    pending_shell: None,
                },
                config: crate::session::lane::Config::new(initial_model_ref),
            }),
        );
        let resumed = loaded.is_some();
        Self {
            session_id,
            runtime_instance_id,
            cwd,
            provider,
            tools,
            artifact_root,
            trusted_resources,
            effect_gate,
            store,
            policy,
            interactive_approvals,
            budget,
            control_parent_session_id: parent,
            fork_source_session_id: fork_source.map(|(session, _)| session),
            fork_source_entry_id: fork_source.and_then(|(_, entry)| entry),
            commands,
            engine_tx,
            engine_rx,
            tool_tx,
            shell_tx,
            tool_rx,
            shell_rx,
            shell_cancel: None,
            cancel_root: CancellationToken::new(),
            tracker: TaskTracker::new(),
            cursor: RuntimeCursor::default(),
            tree_entries: Vec::new(),
            entry_index: HashMap::new(),
            lanes,
            next_entry_seq: 1,
            operations: HashMap::new(),
            suspended_operations: Vec::new(),
            events,
            main_events,
            main_latest_settlement: None,
            main_indeterminate_warning: None,
            closed: false,
            resumed,
            loaded,
            defer_loaded_start,
            reopen_entry_count: None,
            usage_totals: TokenUsage::default(),
        }
    }

    /// Rebuild live state from a loaded session: transcript, entry
    /// sequence, and — for a non-terminal operation — the complete
    /// machine, its pending inbox, and its pending effect intent.
    fn restore_from(&mut self, loaded: LoadedSession) {
        let latest_usage = loaded.latest_usage.map(|usage| TokenUsage {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_tokens,
            cache_write: usage.cache_write_tokens,
        });
        self.usage_totals = loaded.usage_totals;
        let last_context_tokens = latest_usage.map(TokenUsage::context_tokens);
        let assistant_frames = loaded.assistant_frames;
        let max_seq = loaded
            .entries
            .iter()
            .map(|record| record.seq)
            .max()
            .unwrap_or(0);
        self.tree_entries = loaded.entries;
        self.entry_index = self
            .tree_entries
            .iter()
            .enumerate()
            .map(|(index, record)| (record.id, index))
            .collect();
        self.lanes = loaded
            .lanes
            .into_iter()
            .map(|lane| (lane.name.clone(), ResidentLane::new(lane)))
            .collect();
        if !self.lanes.contains_key(crate::session::lane::MAIN) {
            error!(session = %self.session_id, "reopened session has no main lane; fencing");
            self.closed = true;
            return;
        }
        {
            let live = &mut self
                .lanes
                .get_mut(crate::session::lane::MAIN)
                .expect("checked main lane")
                .live;
            live.latest_usage = latest_usage;
            live.last_context_tokens = last_context_tokens;
        }
        let Some(main_branch) = self.lane_branch_records(crate::session::lane::MAIN) else {
            error!(session = %self.session_id, "main lane branch is incomplete; fencing");
            self.closed = true;
            return;
        };
        self.reopen_entry_count = Some(main_branch.len());
        self.next_entry_seq = max_seq + 1;
        self.main_latest_settlement = loaded
            .operations
            .iter()
            .filter(|operation| operation.lane_name == crate::session::lane::MAIN)
            .filter_map(|operation| match &operation.latest.1.state {
                OperationState::Finished(outcome) => Some((
                    operation.accepted_seq,
                    OperationSettlement {
                        operation_id: operation.id,
                        outcome: outcome.clone(),
                    },
                )),
                _ => None,
            })
            .max_by_key(|(accepted_seq, _)| *accepted_seq)
            .map(|(_, settlement)| settlement);
        self.main_indeterminate_warning = loaded
            .operations
            .iter()
            .filter(|operation| operation.lane_name == crate::session::lane::MAIN)
            .filter_map(|operation| match &operation.latest.1.state {
                OperationState::Finished(OperationOutcome::Indeterminate) => Some((
                    operation.accepted_seq,
                    IndeterminateWarning {
                        operation_id: operation.id,
                        message: INDETERMINATE_MESSAGE.to_owned(),
                    },
                )),
                _ => None,
            })
            .max_by_key(|(accepted_seq, _)| *accepted_seq)
            .map(|(_, warning)| warning);

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
                self.suspended_operations.push((
                    operation.id,
                    state_seq,
                    payload.clone(),
                    operation.capability_snapshot.clone(),
                ));
                continue;
            }
            let pending_inputs: Vec<InboxItem> = operation
                .pending_inbox
                .iter()
                .filter(|item| !matches!(&item.kind, InboxKind::Prompt))
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
                pending_inputs,
            );
            info!(
                session = %self.session_id,
                operation = %operation.id,
                state = ?payload.state,
                "reopened an open operation; recovery is Step 3 work"
            );
            let tool_registry = self
                .tool_registry_for_lane(&operation.lane_name)
                .expect("loaded operation origin lane exists")
                .available_for_snapshot(&operation.capability_snapshot);
            let active = ActiveOperation {
                machine,
                capability_snapshot: operation.capability_snapshot.clone(),
                tool_registry,
                cancel: self.cancel_root.child_token(),
                state_seq,
                open_effect: payload.open_effect.clone(),
                pending_inputs: operation
                    .pending_inbox
                    .iter()
                    .filter(|item| !matches!(&item.kind, InboxKind::Prompt))
                    .map(|item| item.id)
                    .collect(),
            };
            let mut resident = ResidentOperation::new(operation.lane_name.clone(), active);
            if matches!(payload.state, OperationState::AssistantEffectPending)
                && let Some(effect_id) = payload.open_effect.as_ref().map(|effect| effect.id)
                && let Some(frame) = assistant_frames.iter().find(|frame| {
                    frame.operation_id == operation.id && frame.effect_id == effect_id
                })
            {
                resident.live.draft_text = frame.text.clone();
                resident.live.draft_thinking = frame.thinking.clone();
                resident.live.assistant_frame_seq = frame.frame_seq;
            }
            if self.operations.insert(operation.id, resident).is_some() {
                error!(
                    session = %self.session_id,
                    operation = %operation.id,
                    "multiple open operations in durable state; refusing to guess"
                );
                self.closed = true;
                return;
            }
        }
    }

    fn lane(&self, lane_name: &str) -> Option<&crate::session::lane::Lane> {
        self.lanes.get(lane_name).map(|lane| &lane.durable)
    }

    fn lane_mut(&mut self, lane_name: &str) -> Option<&mut crate::session::lane::Lane> {
        self.lanes.get_mut(lane_name).map(|lane| &mut lane.durable)
    }

    fn main_lane(&self) -> &crate::session::lane::Lane {
        self.lane(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }

    fn tool_registry_for_lane(&self, lane_name: &str) -> Option<ToolRegistry> {
        let config = &self.lane(lane_name)?.config;
        Some(
            self.tools
                .snapshot_for_scopes(&config.scopes)
                .selected(&config.tools),
        )
    }

    fn tool_registry_for_operation(&self, operation_id: OperationId) -> Option<ToolRegistry> {
        let lane_name = self.operation_lane_name(operation_id)?;
        self.tool_registry_for_lane(lane_name)
    }

    fn lane_live(&self, lane_name: &str) -> Option<&LaneResidency> {
        self.lanes.get(lane_name).map(|lane| &lane.live)
    }

    fn lane_live_mut(&mut self, lane_name: &str) -> Option<&mut LaneResidency> {
        self.lanes.get_mut(lane_name).map(|lane| &mut lane.live)
    }

    fn operation_lane_live(&self, operation_id: OperationId) -> Option<&LaneResidency> {
        let lane_name = self.operation_lane_name(operation_id)?;
        self.lane_live(lane_name)
    }

    fn operation_lane_live_mut(&mut self, operation_id: OperationId) -> Option<&mut LaneResidency> {
        let lane_name = self.operation_lane_name(operation_id)?.to_owned();
        self.lane_live_mut(&lane_name)
    }

    fn main_lane_live(&self) -> &LaneResidency {
        self.lane_live(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }

    fn resident(&self, operation_id: OperationId) -> Option<&ResidentOperation> {
        self.operations.get(&operation_id)
    }

    fn resident_mut(&mut self, operation_id: OperationId) -> Option<&mut ResidentOperation> {
        self.operations.get_mut(&operation_id)
    }

    fn active(&self, operation_id: OperationId) -> Option<&ActiveOperation> {
        self.resident(operation_id).map(|resident| &resident.active)
    }

    fn live(&self, operation_id: OperationId) -> Option<&OperationResidency> {
        self.resident(operation_id).map(|resident| &resident.live)
    }

    fn live_mut(&mut self, operation_id: OperationId) -> Option<&mut OperationResidency> {
        self.resident_mut(operation_id)
            .map(|resident| &mut resident.live)
    }

    fn operation_lane_name(&self, operation_id: OperationId) -> Option<&str> {
        self.resident(operation_id)
            .map(|resident| resident.lane_name.as_str())
            .or_else(|| {
                self.lanes.iter().find_map(|(name, lane)| {
                    (lane.durable.state.current_operation == Some(operation_id))
                        .then_some(name.as_str())
                })
            })
    }

    fn lane_resident_id(&self, lane_name: &str) -> Option<OperationId> {
        let lane = self.lanes.get(lane_name)?;
        lane.durable.state.current_operation.or_else(|| {
            self.operations.iter().find_map(|(operation_id, resident)| {
                (resident.lane_name == lane_name).then_some(*operation_id)
            })
        })
    }

    fn main_resident_id(&self) -> Option<OperationId> {
        self.lane_resident_id(crate::session::lane::MAIN)
    }

    fn main_resident(&self) -> Option<&ResidentOperation> {
        self.resident(self.main_resident_id()?)
    }

    fn main_resident_mut(&mut self) -> Option<&mut ResidentOperation> {
        self.resident_mut(self.main_resident_id()?)
    }

    fn main_active(&self) -> Option<&ActiveOperation> {
        self.main_resident().map(|resident| &resident.active)
    }

    fn main_live(&self) -> Option<&OperationResidency> {
        self.main_resident().map(|resident| &resident.live)
    }

    fn main_live_mut(&mut self) -> Option<&mut OperationResidency> {
        self.main_resident_mut().map(|resident| &mut resident.live)
    }

    fn install_active(&mut self, active: ActiveOperation) {
        let operation_id = active.machine.operation_id();
        let resident = self
            .operations
            .get_mut(&operation_id)
            .expect("installed operation residency exists");
        resident.active = active;
    }

    fn remove_operation(&mut self, operation_id: OperationId) -> Option<ResidentOperation> {
        self.operations.remove(&operation_id)
    }

    fn lane_branch_records(&self, lane_name: &str) -> Option<Vec<&EntryRecord>> {
        let lane = self.lanes.get(lane_name)?;
        let mut branch = Vec::new();
        let mut cursor = lane.durable.state.leaf;
        while let Some(entry_id) = cursor {
            let index = *self.entry_index.get(&entry_id)?;
            let record = self.tree_entries.get(index)?;
            branch.push(record);
            cursor = record.parent;
        }
        branch.reverse();
        Some(branch)
    }

    fn operation_branch_records(&self, operation_id: OperationId) -> Option<Vec<&EntryRecord>> {
        self.lane_branch_records(self.operation_lane_name(operation_id)?)
    }

    fn main_branch_records(&self) -> Vec<&EntryRecord> {
        self.lane_branch_records(crate::session::lane::MAIN)
            .expect("live main lane branch is complete")
    }

    fn lane_leaf(&self, lane_name: &str) -> Option<EntryId> {
        self.lane(lane_name).and_then(|lane| lane.state.leaf)
    }

    fn lane_pending_next_run(&self, lane_name: &str) -> Option<&crate::session::lane::NextRun> {
        self.lane(lane_name)?.state.pending_next_run.as_ref()
    }

    fn lane_pending_shell(&self, lane_name: &str) -> Option<&crate::session::lane::PendingShell> {
        self.lane(lane_name)?.state.pending_shell.as_ref()
    }

    /// Reopen recovery for a shell passthrough interrupted by process
    /// loss (§10): the command was user-initiated and its completion
    /// was never observed, so it never silently replays. Settle the
    /// marker durably as cancelled — the honest record — rather than
    /// leaving a busy marker or dropping the run.
    async fn recover_pending_shells(&mut self, loaded: &crate::store::LoadedSession) {
        for lane in &loaded.lanes {
            let Some(pending) = &lane.state.pending_shell else {
                continue;
            };
            let entry = SessionEntry::ShellExecution {
                command: pending.command.clone(),
                output: String::new(),
                exit_code: None,
                cancelled: true,
                exclude_from_context: pending.exclude_from_context,
            };
            let record = EntryRecord {
                id: pending.entry_id,
                parent: lane.state.leaf,
                seq: self.next_entry_seq,
                entry,
            };
            if let Err(err) = self
                .store
                .settle_shell_entry(self.session_id, &lane.name, record.clone())
                .await
            {
                error!(
                    session = %self.session_id,
                    lane = %lane.name,
                    %err,
                    "interrupted shell passthrough could not settle; lane stays marked"
                );
                continue;
            }
            self.next_entry_seq += 1;
            let lane_state = &mut self
                .lane_mut(&lane.name)
                .expect("recovered lane is resident")
                .state;
            lane_state.leaf = Some(record.id);
            lane_state.pending_shell = None;
            self.install_tree_entries(vec![record]);
            warn!(
                session = %self.session_id,
                lane = %lane.name,
                "reopened with an interrupted shell passthrough; settled as cancelled"
            );
        }
    }

    fn main_model_ref(&self) -> &str {
        &self.main_lane().config.model_ref
    }

    fn install_tree_entries(&mut self, entries: Vec<EntryRecord>) {
        for record in entries {
            let index = self.tree_entries.len();
            let previous = self.entry_index.insert(record.id, index);
            debug_assert!(previous.is_none(), "entry identity installed twice");
            self.tree_entries.push(record);
        }
    }

    async fn materialize_loaded_scope_grants(
        &self,
        loaded: &mut LoadedSession,
    ) -> Result<(), StoreError> {
        let published = self.tools.admission_scopes();
        for lane in &mut loaded.lanes {
            if lane.config.scopes.materialize(&published) {
                self.store
                    .set_lane_config(self.session_id, &lane.name, lane.config.clone())
                    .await?;
            }
        }
        Ok(())
    }

    async fn run(mut self) {
        let mut startup_command = None;
        if self.resumed {
            if self.defer_loaded_start {
                startup_command = self.commands.recv().await;
                if startup_command.is_none() {
                    return;
                }
                match self.store.load(self.session_id).await {
                    Ok(loaded) => self.loaded = Some(loaded),
                    Err(err) => {
                        error!(session = %self.session_id, %err, "could not reload deferred session");
                        return;
                    }
                }
            }
            let Some(mut loaded) = self.loaded.take() else {
                error!(session = %self.session_id, "resumed runtime has no loaded state");
                return;
            };
            if let Err(err) = self.materialize_loaded_scope_grants(&mut loaded).await {
                error!(session = %self.session_id, %err, "could not materialize structural scope grants");
                return;
            }
            self.restore_from(loaded);
            let loaded_for_recovery = self.store.load(self.session_id).await;
            match loaded_for_recovery {
                Ok(fresh) => self.recover_pending_shells(&fresh).await,
                Err(err) => {
                    error!(session = %self.session_id, %err, "could not reload lanes for shell recovery");
                }
            }
        } else {
            let published = self.tools.admission_scopes();
            self.lane_mut(crate::session::lane::MAIN)
                .expect("new runtime has a main lane")
                .config
                .scopes = crate::session::lane::ScopeGrant::from_published(published);
        }

        if !self.resumed && !self.closed {
            let record = SessionRecord {
                id: self.session_id,
                cwd: self.cwd.clone(),
                title: String::new(),
                initial_model_ref: self.main_model_ref().to_owned(),
                control_parent_session_id: self.control_parent_session_id,
                fork_source_session_id: self.fork_source_session_id,
                fork_source_entry_id: self.fork_source_entry_id,
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
            let config = self.main_lane().config.clone();
            if let Err(err) = self
                .store
                .set_lane_config(self.session_id, crate::session::lane::MAIN, config)
                .await
            {
                error!(session = %self.session_id, %err, "initial scope grant not durable");
                self.closed = true;
                return;
            }
        }
        info!(session = %self.session_id, "session opened");
        if !self.operations.is_empty() || !self.suspended_operations.is_empty() {
            self.recover_open_operation().await;
        }
        if !self.closed {
            let pending_lanes = self
                .lanes
                .iter()
                .filter(|(_, lane)| {
                    lane.durable.state.current_operation.is_none()
                        && lane.durable.state.pending_next_run.is_some()
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            for lane_name in pending_lanes {
                if let Some(operation_id) = self.promote_pending_next_run(&lane_name).await {
                    self.advance(operation_id).await;
                }
                if self.closed {
                    break;
                }
            }
        }
        if let Some(command) = startup_command
            && self.handle_command(command).await
        {
            return;
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
                signal = self.shell_rx.recv() => {
                    if let Some(signal) = signal {
                        self.handle_shell_signal(signal).await;
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
            SessionCommand::CreateLane { lane_name, reply } => {
                let _ = reply.send(self.create_lane(lane_name).await);
                false
            }
            SessionCommand::AdmitAgentLane {
                agent_id,
                control_parent_id,
                source_lane_name,
                reply,
            } => {
                let _ = reply.send(
                    self.admit_agent_lane(agent_id, control_parent_id, source_lane_name)
                        .await,
                );
                false
            }
            SessionCommand::AdmitStructuralScope { scope, reply } => {
                let _ = reply.send(self.admit_structural_scope(scope).await);
                false
            }
            SessionCommand::SubmitIfIdle {
                lane_name,
                prompt,
                reply,
            } => {
                let _ = reply.send(self.submit_if_idle_on_lane(lane_name, prompt).await);
                false
            }
            SessionCommand::NextRun {
                lane_name,
                prompt,
                reply,
            } => {
                let _ = reply.send(self.next_run_on_lane(lane_name, prompt).await);
                false
            }
            SessionCommand::DequeueNextRun { lane_name, reply } => {
                let _ = reply.send(self.dequeue_next_run(&lane_name).await);
                false
            }
            SessionCommand::RunShell {
                lane_name,
                command,
                exclude_from_context,
                reply,
            } => {
                self.run_shell_on_lane(&lane_name, command, exclude_from_context, reply)
                    .await;
                false
            }
            SessionCommand::CancelShell { reply } => {
                let _ = reply.send(Ok(self.shell_cancel.as_ref().is_some_and(|token| {
                    token.cancel();
                    true
                })));
                false
            }
            SessionCommand::Steer { text, reply } => {
                let _ = reply.send(self.enqueue_steer(text).await);
                false
            }
            SessionCommand::SendAgentMessage {
                from,
                lane_name,
                text,
                reply,
            } => {
                let _ = reply.send(self.send_agent_message(from, lane_name, text).await);
                false
            }
            SessionCommand::Cancel {
                operation_id,
                reply,
            } => {
                let _ = reply.send(self.cancel(operation_id).await);
                false
            }
            SessionCommand::DecideApproval {
                operation_id,
                allow,
                reply,
            } => {
                let _ = reply.send(self.decide_approval(operation_id, allow).await);
                false
            }
            SessionCommand::Compact {
                instructions,
                reply,
            } => {
                let requested = self.main_active().is_some();
                if requested {
                    // Consumed at the next continuation boundary by the
                    // harness-owned maintenance path.
                    self.main_live_mut()
                        .expect("main operation residency exists")
                        .pending_compact = Some(instructions);
                }
                let _ = reply.send(Ok(requested));
                false
            }
            SessionCommand::SwitchThinking {
                lane_name,
                thinking,
                reply,
            } => {
                let _ = reply.send(self.switch_thinking_on_lane(lane_name, thinking).await);
                false
            }
            SessionCommand::SwitchModel {
                lane_name,
                model_ref,
                reply,
            } => {
                let _ = reply.send(self.switch_model_on_lane(lane_name, model_ref).await);
                false
            }
            SessionCommand::Subscribe { reply } => {
                let _ = reply.send(self.subscribe());
                false
            }
            SessionCommand::SubscribeAll { reply } => {
                let _ = reply.send(self.subscribe_all());
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

    async fn create_lane(&mut self, lane_name: String) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let lane_name = lane_name.trim().to_owned();
        if lane_name.is_empty() {
            return Err(CommandError::InvalidLaneName);
        }
        if self.lanes.contains_key(&lane_name) {
            return Err(CommandError::LaneExists(lane_name));
        }
        let source_leaf = self.main_lane().state.leaf;
        let config = self.main_lane().config.clone();
        self.store
            .create_lane_with_config(
                self.session_id,
                lane_name.clone(),
                source_leaf,
                config.clone(),
            )
            .await
            .map_err(persistence_command_error)?;
        let previous = self.lanes.insert(
            lane_name.clone(),
            ResidentLane::new(crate::session::lane::Lane {
                name: lane_name,
                state: crate::session::lane::State {
                    leaf: source_leaf,
                    current_operation: None,
                    pending_next_run: None,
                    pending_shell: None,
                },
                config,
            }),
        );
        debug_assert!(previous.is_none(), "lane topology identity is unique");
        Ok(())
    }

    async fn admit_agent_lane(
        &mut self,
        agent_id: AgentId,
        control_parent_id: AgentId,
        source_lane_name: String,
    ) -> Result<String, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let source = self
            .lane(&source_lane_name)
            .ok_or_else(|| CommandError::LaneNotFound(source_lane_name.clone()))?;
        let source_leaf = source.state.leaf;
        let mut config = source.config.clone();
        config.tools = config.tools.narrowed_by(&ToolSelection::read_only());
        let lane_name = agent_id.to_string();
        if self.lanes.contains_key(&lane_name) {
            return Err(CommandError::LaneExists(lane_name));
        }
        self.store
            .admit_lane_agent(
                self.session_id,
                agent_id,
                control_parent_id,
                lane_name.clone(),
                source_leaf,
                config.clone(),
            )
            .await
            .map_err(persistence_command_error)?;
        let previous = self.lanes.insert(
            lane_name.clone(),
            ResidentLane::new(crate::session::lane::Lane {
                name: lane_name.clone(),
                state: crate::session::lane::State {
                    leaf: source_leaf,
                    current_operation: None,
                    pending_next_run: None,
                    pending_shell: None,
                },
                config,
            }),
        );
        debug_assert!(previous.is_none(), "agent lane identity is unique");
        Ok(lane_name)
    }

    async fn admit_structural_scope(&mut self, scope: String) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let lane_name = crate::session::lane::MAIN;
        let mut config = self
            .lane(lane_name)
            .expect("root structural scope admission requires the main lane")
            .config
            .clone();
        if !config.scopes.insert(scope) {
            return Ok(());
        }
        self.store
            .set_lane_config(self.session_id, lane_name, config.clone())
            .await
            .map_err(persistence_command_error)?;
        self.lane_mut(lane_name)
            .expect("persisted main lane remains resident")
            .config = config;
        Ok(())
    }

    async fn submit_if_idle_on_lane(
        &mut self,
        lane_name: String,
        prompt: String,
    ) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        if let Some(operation_id) = self.lane_resident_id(&lane_name) {
            return Err(CommandError::Busy { operation_id });
        }
        if let Some(pending) = self.lane_pending_next_run(&lane_name) {
            return Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            });
        }
        // A running shell passthrough owns the next branch position; an
        // operation accepted now would fork against its settlement.
        if self.lane_pending_shell(&lane_name).is_some() {
            return Err(CommandError::ShellPassthroughBusy);
        }
        let (active, _) = self
            .accept_operation_record(&lane_name, prompt, None)
            .await?;
        let operation_id = active.machine.operation_id();
        self.start_active(&lane_name, active);
        self.advance(operation_id).await;
        Ok(operation_id)
    }

    /// Persist one next-run input durably. A busy lane receives only a
    /// provisioned semantic entry identity; operation identity is created
    /// when the lane becomes idle and actually accepts the run.
    async fn next_run_on_lane(
        &mut self,
        lane_name: String,
        prompt: String,
    ) -> Result<crate::ids::EntryId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        if let Some(pending) = self.lane_pending_next_run(&lane_name) {
            return Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            });
        }
        if self.lane_pending_shell(&lane_name).is_some() {
            return Err(CommandError::ShellPassthroughBusy);
        }
        if self.lane_resident_id(&lane_name).is_none() {
            let (active, entry_id) = self
                .accept_operation_record(&lane_name, prompt, None)
                .await?;
            let operation_id = active.machine.operation_id();
            self.start_active(&lane_name, active);
            self.advance(operation_id).await;
            return Ok(entry_id);
        }

        let next_run = crate::session::lane::NextRun::reserve(prompt);
        let entry_id = next_run.entry_id;
        self.store
            .queue_next_run(self.session_id, &lane_name, next_run.clone())
            .await
            .map_err(persistence_command_error)?;
        self.wait_effect_boundary(EffectBoundary::PendingNextRunCommit)
            .await;
        self.lane_mut(&lane_name)
            .expect("queued lane remains resident")
            .state
            .pending_next_run = Some(next_run);
        Ok(entry_id)
    }

    /// Remove the lane's pending next-run input and return its prompt
    /// (pi parity: alt+up dequeues a queued prompt back to the editor).
    /// The cleared state is durable before the command returns; a busy
    /// lane keeps its queue because the active operation may finish and
    /// promote it at any boundary.
    async fn dequeue_next_run(&mut self, lane_name: &str) -> Result<Option<String>, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name.to_owned()));
        }
        let Some(pending) = self.lane_pending_next_run(lane_name) else {
            return Ok(None);
        };
        let prompt = pending.prompt.clone();
        self.store
            .clear_next_run(self.session_id, lane_name)
            .await
            .map_err(persistence_command_error)?;
        self.lane_mut(lane_name)
            .expect("dequeued lane remains resident")
            .state
            .pending_next_run = None;
        Ok(Some(prompt))
    }

    /// Create the durable operation only when the lane is free. A pending
    /// next run supplies its pre-provisioned semantic entry identity but does
    /// not pre-provision operation identity or freeze model/tool capability
    /// state before this acceptance boundary.
    async fn accept_operation_record(
        &mut self,
        lane_name: &str,
        prompt: String,
        reservation: Option<crate::session::lane::NextRun>,
    ) -> Result<(ActiveOperation, crate::ids::EntryId), CommandError> {
        self.accept_operation_input(
            lane_name,
            InboxItem {
                kind: InboxKind::Prompt,
                text: prompt,
            },
            reservation,
        )
        .await
    }

    async fn accept_operation_input(
        &mut self,
        lane_name: &str,
        input: InboxItem,
        reservation: Option<crate::session::lane::NextRun>,
    ) -> Result<(ActiveOperation, crate::ids::EntryId), CommandError> {
        let operation_id = OperationId::generate();
        let tool_registry = self
            .tool_registry_for_lane(lane_name)
            .expect("operation acceptance lane exists");
        let (machine, applied) = match &input.kind {
            InboxKind::Prompt => {
                OperationMachine::accept(operation_id, input.text.clone(), tool_registry.specs())
            }
            InboxKind::AgentMessage { from } => OperationMachine::accept_agent_message(
                operation_id,
                *from,
                input.text.clone(),
                tool_registry.specs(),
            ),
            InboxKind::Steer => unreachable!("steer cannot open a new operation"),
        };
        let capability_snapshot = tool_registry.capability_snapshot();
        let root_inbox = InboxRecord {
            id: InboxId::generate(),
            kind: input.kind,
            text: input.text,
            status: InboxStatus::Applied,
        };
        let entry = match reservation.as_ref() {
            Some(next_run) => EntryRecord {
                id: next_run.entry_id,
                seq: self.next_entry_seq,
                parent: self.lane_leaf(lane_name),
                entry: applied.entries[0].clone(),
            },
            None => EntryRecord::provision(self.next_entry_seq, applied.entries[0].clone())
                .after(self.lane_leaf(lane_name)),
        };
        let entry_id = entry.id;
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
        self.store
            .begin_operation(
                self.session_id,
                lane_name,
                operation_id,
                root_inbox,
                checkpoint,
                entry.clone(),
            )
            .await
            .map_err(persistence_command_error)?;

        let entry_leaf = entry.id;
        self.install_tree_entries(vec![entry]);
        self.next_entry_seq += 1;
        let lane = self
            .lane_mut(lane_name)
            .expect("accepted operation lane remains resident");
        lane.state.leaf = Some(entry_leaf);
        lane.state.current_operation = Some(operation_id);
        if reservation.is_some() {
            lane.state.pending_next_run = None;
        }
        Ok((
            ActiveOperation {
                machine,
                capability_snapshot,
                tool_registry,
                cancel: self.cancel_root.child_token(),
                state_seq: 1,
                open_effect: None,
                pending_inputs: Vec::new(),
            },
            entry_id,
        ))
    }

    fn start_active(&mut self, lane_name: &str, active: ActiveOperation) {
        let operation_id = active.machine.operation_id();
        let prompt = active.machine.prompt().to_owned();
        let previous = self
            .operations
            .insert(operation_id, ResidentOperation::new(lane_name, active));
        debug_assert!(previous.is_none(), "operation residency identity is unique");
        self.emit(RuntimeEvent::OperationStarted {
            cursor: RuntimeCursor::default(),
            operation_id,
            prompt,
        });
    }

    async fn promote_pending_next_run(&mut self, lane_name: &str) -> Option<OperationId> {
        if self.lane_resident_id(lane_name).is_some() {
            return None;
        }
        let next_run = self.lane_pending_next_run(lane_name)?.clone();
        match self
            .accept_operation_record(lane_name, next_run.prompt.clone(), Some(next_run.clone()))
            .await
        {
            Ok((active, _)) => {
                let operation_id = active.machine.operation_id();
                self.start_active(lane_name, active);
                Some(operation_id)
            }
            Err(err) => {
                error!(
                    session = %self.session_id,
                    lane = lane_name,
                    entry = %next_run.entry_id,
                    %err,
                    "could not promote the durable next run; fencing until reopen"
                );
                self.closed = true;
                None
            }
        }
    }

    async fn switch_model_on_lane(
        &mut self,
        lane_name: String,
        model_ref: String,
    ) -> Result<String, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        let model_ref = model_ref.trim().to_owned();
        if model_ref.is_empty() || !self.provider.supports_model(&model_ref) {
            return Err(CommandError::UnsupportedModel(model_ref));
        }
        let previous = self
            .lane(&lane_name)
            .expect("checked lane")
            .config
            .model_ref
            .clone();
        if model_ref == previous {
            return Ok(previous);
        }
        let mut config = self.lane(&lane_name).expect("checked lane").config.clone();
        config.model_ref = model_ref;
        self.store
            .set_lane_config(self.session_id, &lane_name, config.clone())
            .await
            .map_err(persistence_command_error)?;
        self.lane_mut(&lane_name)
            .expect("configured lane remains resident")
            .config = config;
        let live = self
            .lane_live_mut(&lane_name)
            .expect("configured lane residency remains live");
        live.context_window = None;
        live.model_capabilities = None;
        live.last_prefix_fingerprint = None;
        Ok(previous)
    }

    /// Set the lane's thinking level for future model steps (pi parity:
    /// /thinking, shift+tab). Mirrors `switch_model_on_lane`: durable
    /// before return, applied at the next step boundary. `None` clears
    /// the selection back to the adapter default. Returns the previous
    /// selection. The level is validated against the fixed pi vocabulary
    /// so a typo cannot persist an unusable configuration.
    async fn switch_thinking_on_lane(
        &mut self,
        lane_name: String,
        thinking: Option<String>,
    ) -> Result<Option<String>, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        if let Some(level) = &thinking {
            const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
            if !LEVELS.contains(&level.to_lowercase().as_str()) {
                return Err(CommandError::UnsupportedThinking(level.clone()));
            }
        }
        let normalized = thinking.map(|level| level.to_lowercase());
        let previous = self
            .lane(&lane_name)
            .expect("checked lane")
            .config
            .thinking
            .clone();
        if normalized == previous {
            return Ok(previous);
        }
        let mut config = self.lane(&lane_name).expect("checked lane").config.clone();
        config.thinking = normalized;
        self.store
            .set_lane_config(self.session_id, &lane_name, config.clone())
            .await
            .map_err(persistence_command_error)?;
        self.lane_mut(&lane_name)
            .expect("configured lane remains resident")
            .config = config;
        Ok(previous)
    }

    /// Run one user shell passthrough on an idle lane (pi parity:
    /// `!command`/`!!command`). The passthrough's intent — provisioned
    /// entry identity plus the exact command — is committed durably
    /// *before* the process spawns (DESIGN.md §10), and the reply
    /// returns once that intent is durable; the settled entry follows
    /// via the shell signal path and the pending reply is answered then.
    /// Execution reuses the native `bash` tool's executor so the sandbox
    /// policy, process-group teardown, and output bounding are identical
    /// to model-initiated shell. A lane with an active operation, a
    /// queued next-run, or another passthrough refuses.
    async fn run_shell_on_lane(
        &mut self,
        lane_name: &str,
        command: String,
        exclude_from_context: bool,
        reply: oneshot::Sender<Result<(), CommandError>>,
    ) {
        if self.closed {
            let _ = reply.send(Err(CommandError::Closed));
            return;
        }
        if self.lane(lane_name).is_none() {
            let _ = reply.send(Err(CommandError::LaneNotFound(lane_name.to_owned())));
            return;
        }
        if let Some(operation_id) = self.lane_resident_id(lane_name) {
            let _ = reply.send(Err(CommandError::Busy { operation_id }));
            return;
        }
        if let Some(pending) = self.lane_pending_next_run(lane_name) {
            let _ = reply.send(Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            }));
            return;
        }
        if self.lane_pending_shell(lane_name).is_some() || self.shell_cancel.is_some() {
            let _ = reply.send(Err(CommandError::ShellPassthroughBusy));
            return;
        }
        // Durable intent first (§10): provision the entry identity and
        // mark the lane busy before any process exists.
        let pending = crate::session::lane::PendingShell {
            entry_id: EntryId::generate(),
            command: command.clone(),
            exclude_from_context,
        };
        if let Err(err) = self
            .store
            .queue_pending_shell(self.session_id, lane_name, pending.clone())
            .await
        {
            let _ = reply.send(Err(persistence_command_error(err)));
            return;
        }
        self.lane_mut(lane_name)
            .expect("shell lane remains resident")
            .state
            .pending_shell = Some(pending);
        self.emit(RuntimeEvent::ShellStarted {
            cursor: RuntimeCursor::default(),
            lane_name: lane_name.to_owned(),
            command: command.clone(),
            exclude_from_context,
        });
        // Spawn through the native bash executor: one process path, one
        // sandbox policy, one bounding rule. The per-run token lets esc
        // cancel; close cancels through the child of cancel_root.
        let shell_cancel = self.cancel_root.child_token();
        self.shell_cancel = Some(shell_cancel.clone());
        let cancel = shell_cancel.clone();
        let tools = Arc::clone(&self.tools);
        let arguments = serde_json::json!({ "command": command });
        let (progress_tx, mut progress_rx) = mpsc::channel::<crate::tool::ToolProgress>(8);
        let shell_tx = self.shell_tx.clone();
        let lane = lane_name.to_owned();
        let settle_command = command.clone();
        self.tracker.spawn(async move {
            let forward = async {
                while let Some(progress) = progress_rx.recv().await {
                    let _ = shell_tx
                        .send(ShellSignal::Output {
                            lane_name: lane.clone(),
                            output: progress.output,
                        })
                        .await;
                }
            };
            let (outcome, ()) = tokio::join!(
                tools.execute_with_progress("bash", &arguments, cancel.clone(), Some(progress_tx)),
                forward
            );
            let _ = shell_tx
                .send(ShellSignal::Settled {
                    lane_name: lane,
                    command: settle_command,
                    outcome,
                    cancelled: cancel.is_cancelled(),
                    exclude_from_context,
                })
                .await;
        });
        // The reply is answered now: durable intent is committed and the
        // process is in flight. Outcome arrives via the settled entry
        // and its event, exactly like tool completion.
        let _ = reply.send(Ok(()));
    }

    /// Drain one shell passthrough signal. Output forwards as a
    /// display-only event; settlement makes the entry durable before
    /// the pending reply is answered.
    async fn handle_shell_signal(&mut self, signal: ShellSignal) {
        match signal {
            ShellSignal::Output { lane_name, output } => {
                self.emit(RuntimeEvent::ShellOutput {
                    cursor: RuntimeCursor::default(),
                    lane_name,
                    output,
                });
            }
            ShellSignal::Settled {
                lane_name,
                command,
                outcome,
                cancelled,
                exclude_from_context,
            } => {
                self.shell_cancel = None;
                // The bash executor reports nonzero exits (and
                // cancellation) as error outcomes whose text is the
                // combined output. pi's settlement shape: cancelled has
                // no exit status; failure records what was observed.
                let (exit_code, output) = if cancelled {
                    (None, String::new())
                } else if outcome.is_error {
                    (None, outcome.output)
                } else {
                    (Some(0), outcome.output)
                };
                let output_preview = crate::tool::ToolResult::Ok {
                    call_id: 0,
                    output: output.clone(),
                    artifact: None,
                    images: Vec::new(),
                }
                .display_preview();
                let entry = SessionEntry::ShellExecution {
                    command: command.clone(),
                    output,
                    exit_code,
                    cancelled,
                    exclude_from_context,
                };
                // The entry identity was provisioned with the durable
                // intent; settlement must use it or the marker can never
                // match (the store's WHERE clause binds the two).
                let pending_entry_id = self
                    .lane_pending_shell(&lane_name)
                    .expect("shell settlement has a pending marker")
                    .entry_id;
                let record = EntryRecord {
                    id: pending_entry_id,
                    seq: self.next_entry_seq,
                    parent: self.lane_leaf(&lane_name),
                    entry,
                };
                let append = self
                    .store
                    .settle_shell_entry(self.session_id, &lane_name, record.clone())
                    .await;
                match append {
                    Ok(()) => {
                        self.next_entry_seq += 1;
                        let lane_state = &mut self
                            .lane_mut(&lane_name)
                            .expect("shell lane remains resident")
                            .state;
                        lane_state.leaf = Some(record.id);
                        lane_state.pending_shell = None;
                        self.install_tree_entries(vec![record]);
                        self.emit(RuntimeEvent::ShellSettled {
                            cursor: RuntimeCursor::default(),
                            lane_name: lane_name.clone(),
                            command: command.clone(),
                            exit_code,
                            cancelled,
                            exclude_from_context,
                            output_preview,
                        });
                        info!(session = %self.session_id, command = %command, "shell passthrough settled")
                    }
                    Err(err) => {
                        // §15: persistence failure is a harness failure.
                        // Surface it; the entry is not durable.
                        error!(session = %self.session_id, %err, "shell settlement not durable")
                    }
                };
                // Outcome visibility is the settled entry plus its event;
                // there is no pending command reply to answer.
            }
        }
    }

    async fn enqueue_steer(&mut self, text: String) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let operation_id = self
            .main_active()
            .map(|active| active.machine.operation_id())
            .ok_or(CommandError::NoActiveOperation)?;
        self.enqueue_operation_input(
            operation_id,
            InboxItem {
                kind: InboxKind::Steer,
                text,
            },
        )
        .await
    }

    async fn send_agent_message(
        &mut self,
        from: AgentId,
        lane_name: String,
        text: String,
    ) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if self.lane(&lane_name).is_none() {
            return Err(CommandError::LaneNotFound(lane_name));
        }
        let input = InboxItem {
            kind: InboxKind::AgentMessage { from },
            text,
        };
        if let Some(operation_id) = self.lane_resident_id(&lane_name) {
            self.enqueue_operation_input(operation_id, input).await?;
            return Ok(operation_id);
        }
        if let Some(pending) = self.lane_pending_next_run(&lane_name) {
            return Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            });
        }
        let (active, _) = self.accept_operation_input(&lane_name, input, None).await?;
        let operation_id = active.machine.operation_id();
        self.start_active(&lane_name, active);
        self.advance(operation_id).await;
        Ok(operation_id)
    }

    async fn enqueue_operation_input(
        &mut self,
        operation_id: OperationId,
        item: InboxItem,
    ) -> Result<(), CommandError> {
        let inbox_id = InboxId::generate();
        // Stage on a full clone; a failed commit discards the clone and
        // never mutates live state (DESIGN.md §26.2).
        let mut staged = self
            .active(operation_id)
            .cloned()
            .ok_or(CommandError::NotActive { operation_id })?;
        let record_kind = item.kind.clone();
        let record_text = item.text.clone();
        let applied = staged
            .machine
            .apply(Transition::ApplyInbox { item })
            .expect("continuation input apply from an active operation");
        let applied_now = !applied.entries.is_empty();
        let record = InboxRecord {
            id: inbox_id,
            kind: record_kind,
            text: record_text,
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
        self.commit_transition(request)
            .await
            .map_err(persistence_command_error)?;

        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        if !applied_now {
            staged.pending_inputs.push(inbox_id);
        }
        self.install_active(staged);
        self.advance(operation_id).await;
        Ok(())
    }

    /// Request semantic cancellation (DESIGN.md §9.4): the request is
    /// durable before acknowledgment, then descendant effects are
    /// signalled.
    async fn cancel(&mut self, operation_id: OperationId) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let Some(active) = self.active(operation_id) else {
            return if self.operations.is_empty() {
                Err(CommandError::NoActiveOperation)
            } else {
                Err(CommandError::NotActive { operation_id })
            };
        };
        let mut staged = active.clone();
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
        self.commit_transition(request)
            .await
            .map_err(persistence_command_error)?;
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.install_active(staged);
        self.wait_effect_boundary(EffectBoundary::CancellationSignal)
            .await;
        self.active(operation_id)
            .expect("cancelled operation installed")
            .cancel
            .cancel();
        // A parked approval has no live effect to settle the cancellation;
        // the operation is terminal now, so surface it and idle the slot.
        if let Some(active) = self.active(operation_id)
            && matches!(active.machine.state(), OperationState::Finished(_))
        {
            let state = active.machine.state().clone();
            self.emit_terminal_state_for(operation_id, &state);
            self.advance(operation_id).await;
        }
        Ok(())
    }

    /// Drive the machine forward from quiescent states: drain queued
    /// inbox items at their boundaries, then start the next model step
    /// or admit the next planned tool. Each move commits durably before
    /// its effect starts (§12.1).
    async fn advance(&mut self, mut operation_id: OperationId) {
        loop {
            let Some(state) = self
                .active(operation_id)
                .map(|active| active.machine.state().clone())
            else {
                return;
            };
            match state {
                OperationState::Finished(_) => {
                    let lane_name = self.operation_lane_name(operation_id).map(str::to_owned);
                    self.remove_operation(operation_id);
                    if let Some(lane_name) = lane_name
                        && let Some(next_operation_id) =
                            self.promote_pending_next_run(&lane_name).await
                    {
                        operation_id = next_operation_id;
                        continue;
                    }
                    return;
                }
                OperationState::Accepted | OperationState::NeedAssistant => {
                    if self
                        .active(operation_id)
                        .is_some_and(|active| active.machine.has_queued_inputs())
                    {
                        if !self.drain_queued(operation_id).await {
                            return;
                        }
                        continue;
                    }
                    if let Some(request) = self
                        .live_mut(operation_id)
                        .expect("main operation residency exists")
                        .pending_compact
                        .take()
                    {
                        // The run has settled; compact now with the
                        // caller's preservation instructions.
                        if !self.start_compaction(operation_id, request).await {
                            return;
                        }
                        return;
                    }
                    if self.safety_net_compaction_due(operation_id) {
                        // §14.7.3: compact at the continuation boundary
                        // when the context nears the model's window.
                        if !self.start_compaction(operation_id, None).await {
                            return;
                        }
                        return;
                    }
                    if !self.start_model_step(operation_id).await {
                        return;
                    }
                    return;
                }
                OperationState::NeedContinuation => {
                    if self
                        .active(operation_id)
                        .is_some_and(|active| active.machine.has_queued_inputs())
                    {
                        if !self.drain_queued(operation_id).await {
                            return;
                        }
                        continue;
                    }
                    panic!("NeedContinuation without queued inbox is impossible state");
                }
                OperationState::ToolsPlanned { .. } => {
                    if !self.admit_next_tool(operation_id).await {
                        return;
                    }
                    return;
                }
                _ => return,
            }
        }
    }

    /// Drain queued continuation inputs as one durable transaction at a reasoning
    /// boundary. Returns false when persistence failed.
    async fn drain_queued(&mut self, operation_id: OperationId) -> bool {
        let (mut staged, request, new_entry_seq) = {
            let active = self
                .active(operation_id)
                .cloned()
                .expect("drain needs an operation");
            let mut staged = active.clone();
            let drained = staged.machine.drain_inputs().expect("input drain");
            let applied_ids = staged
                .pending_inputs
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
            (staged, request, new_entry_seq)
        };
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return false;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.install_active(staged);
        true
    }

    /// Project the model-step input from canonical entries and the current
    /// manifest. Compaction is harness-owned; no synthetic model message
    /// is injected into this projection.
    async fn current_model_config(&mut self, operation_id: OperationId) -> ModelConfig {
        let lane_name = self
            .operation_lane_name(operation_id)
            .expect("resident operation has an owning lane")
            .to_owned();
        let selected_model_ref = self
            .lanes
            .get(&lane_name)
            .expect("operation lane exists")
            .durable
            .config
            .model_ref
            .clone();
        let capabilities = match self
            .lane_live(&lane_name)
            .expect("operation lane residency exists")
            .model_capabilities
            .as_ref()
        {
            Some((model_ref, capabilities)) if model_ref == &selected_model_ref => *capabilities,
            _ => {
                let capabilities = self.provider.capabilities_for(&selected_model_ref).await;
                self.lane_live_mut(&lane_name)
                    .expect("operation lane residency exists")
                    .model_capabilities = Some((selected_model_ref.clone(), capabilities));
                capabilities
            }
        };
        // Pricing refreshes with the model selection, exactly like
        // capabilities; the footer cost reads this cache.
        let pricing = match self
            .lane_live(&lane_name)
            .expect("operation lane residency exists")
            .model_pricing
        {
            Some(pricing) => Some(pricing),
            None => {
                let pricing = self.provider.pricing_for(&selected_model_ref).await;
                if pricing.is_some() {
                    self.lane_live_mut(&lane_name)
                        .expect("operation lane residency exists")
                        .model_pricing = pricing;
                }
                pricing
            }
        };
        let context_window = match self
            .lane_live(&lane_name)
            .expect("operation lane residency exists")
            .context_window
        {
            Some(window) => Some(window),
            None => {
                let window = self.provider.context_window_for(&selected_model_ref).await;
                self.lane_live_mut(&lane_name)
                    .expect("operation lane residency exists")
                    .context_window = window;
                window
            }
        };
        let thinking = self
            .lanes
            .get(&lane_name)
            .expect("operation lane exists")
            .durable
            .config
            .thinking
            .clone();
        ModelConfig {
            model_ref: selected_model_ref,
            thinking,
            context_window,
            capabilities,
            pricing,
        }
    }

    fn current_context_manifest(
        &self,
        operation_id: OperationId,
    ) -> (ToolRegistry, CapabilitySnapshot, ContextManifest) {
        let registry = self
            .tool_registry_for_operation(operation_id)
            .expect("context manifest operation has an owning lane");
        let snapshot = registry.capability_snapshot();
        let manifest = ContextManifest::new(&snapshot, self.trusted_resources.clone());
        (registry, snapshot, manifest)
    }

    fn cache_expectation(
        &self,
        operation_id: OperationId,
        model: &ModelConfig,
        prefix_fingerprint: &str,
    ) -> CacheExpectation {
        if !model.capabilities.prompt_cache {
            return CacheExpectation::Unsupported;
        }
        match self
            .operation_lane_live(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint
            .as_deref()
        {
            None => CacheExpectation::ColdStart,
            Some(previous) if previous == prefix_fingerprint => {
                CacheExpectation::PrefixReuseExpected
            }
            Some(_) => CacheExpectation::PrefixChanged,
        }
    }

    async fn project_model_step_plan(
        &mut self,
        operation_id: OperationId,
        manifest: &ContextManifest,
    ) -> crate::context::ContextPlan {
        let model = self.current_model_config(operation_id).await;
        let branch = self
            .operation_branch_records(operation_id)
            .expect("resident operation lane branch is complete");
        project_with_manifest_for_model(
            branch.iter().map(|record| &record.entry),
            branch
                .first()
                .map_or(self.next_entry_seq, |record| record.seq),
            manifest,
            model.capabilities.images,
        )
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
    fn safety_net_compaction_due(&self, operation_id: OperationId) -> bool {
        const RESERVE_TOKENS: u64 = 16_000;
        if self
            .live(operation_id)
            .expect("main operation residency exists")
            .last_step_was_compaction
        {
            return false;
        }
        match self
            .operation_lane_live(operation_id)
            .expect("resident operation has an owning lane")
            .context_window
        {
            Some(window) => {
                self.operation_lane_live(operation_id)
                    .expect("resident operation has an owning lane")
                    .last_context_tokens
                    .unwrap_or(0)
                    > window.saturating_sub(RESERVE_TOKENS)
            }
            None => false,
        }
    }

    /// Commit the compaction effect intent, then spawn the provider
    /// effect that produces the readable summary. Returns false when
    /// persistence failed.
    async fn start_compaction(
        &mut self,
        operation_id: OperationId,
        instructions: Option<String>,
    ) -> bool {
        let model = self.current_model_config(operation_id).await;
        let (_, _, manifest) = self.current_context_manifest(operation_id);
        let branch = self
            .operation_branch_records(operation_id)
            .expect("resident operation lane branch is complete");
        let mut plan = project_with_manifest_for_model(
            branch.iter().map(|record| &record.entry),
            branch
                .first()
                .map_or(self.next_entry_seq, |record| record.seq),
            &manifest,
            model.capabilities.images,
        );
        let mut content = crate::context::SUMMARIZE_INSTRUCTION.to_owned();
        if let Some(instructions) = instructions {
            content.push_str("\n\nPreservation instructions from the caller: ");
            content.push_str(&instructions);
        }
        plan.messages
            .push(crate::context::ContextMessage::User { content });
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("compaction needs an operation");
        let applied = staged
            .machine
            .apply(Transition::StartCompaction { plan: plan.clone() })
            .expect("start compaction from a continuation boundary");
        let EffectIntent::Compaction { operation_id, .. } = applied.intents[0].clone() else {
            panic!("StartCompaction must yield a compaction intent");
        };
        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Compaction(CompactionInvocation {
                step: self
                    .live_mut(operation_id)
                    .expect("main operation residency exists")
                    .model_step
                    + 1,
                model: model.clone(),
                plan: plan.clone(),
                harness_profile: HarnessProfile::default_v1(),
            }),
            RecoveryClass::ReplaySafe,
            1,
        );
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
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return false;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.install_active(staged);
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .last_step_was_compaction = true;
        self.operation_lane_live_mut(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint = None;
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
    async fn fail_budgeted(&mut self, operation_id: OperationId, dimension: &str) {
        if self.active(operation_id).is_none() {
            return;
        }
        let message = format!("operation budget exceeded: {dimension}");
        warn!(session = %self.session_id, %message, "budget exhausted");
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("budget fail needs the addressed operation");
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
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        // Emits OperationFailed for the Failed outcome (terminal event
        // contract); idling here mirrors the approval-required path so
        // no command observes a Finished-but-open operation.
        self.emit_terminal_state_for(operation_id, &applied.state);
        self.remove_operation(operation_id);
    }

    async fn start_model_step(&mut self, operation_id: OperationId) -> bool {
        if self.budget.max_model_steps.is_some_and(|max| {
            self.live_mut(operation_id)
                .expect("main operation residency exists")
                .model_step
                >= u64::from(max)
        }) {
            // Budget bounds are runtime-enforced (§20.5): the
            // operation fails model-visibly instead of looping.
            self.fail_budgeted(operation_id, "model steps").await;
            return false;
        }
        let (step_registry, capability_snapshot, planning_manifest) =
            self.current_context_manifest(operation_id);
        let plan = self
            .project_model_step_plan(operation_id, &planning_manifest)
            .await;
        let model = self.current_model_config(operation_id).await;
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("step needs an operation");
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
        let cache_expectation = self.cache_expectation(operation_id, &model, &prefix_fingerprint);
        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::ModelStep(ModelStepPlan {
                step: self
                    .live_mut(operation_id)
                    .expect("main operation residency exists")
                    .model_step
                    + 1,
                model: model.clone(),
                plan: plan.clone(),
                capability_snapshot_id: capability_snapshot.id.clone(),
                context_manifest_id: context_manifest.id.clone(),
                harness_profile: HarnessProfile::default_v1(),
                prefix_fingerprint: prefix_fingerprint.clone(),
                cache_expectation,
            }),
            RecoveryClass::ReplaySafe,
            1,
        );
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
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return false;
        }
        self.wait_effect_boundary(EffectBoundary::ModelExecution)
            .await;
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.install_active(staged);
        self.operation_lane_live_mut(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint = Some(prefix_fingerprint);
        self.spawn_model_step(operation_id, model, plan, tools);
        true
    }

    fn resolve_tool_for_admission(
        &self,
        operation_id: OperationId,
        call: &ToolCall,
    ) -> (ToolRegistry, Result<ResolvedInvocation, String>) {
        let step_tools = self
            .active(operation_id)
            .map(|active| active.tool_registry.clone())
            .expect("tool admission needs the current step tool registry");
        let active_capability = self
            .active(operation_id)
            .and_then(|active| active.capability_snapshot.identity(&call.name));
        let current_registry = self
            .tool_registry_for_operation(operation_id)
            .expect("tool admission operation has an owning lane");
        let current_snapshot = current_registry.capability_snapshot();
        let current_capability = current_snapshot.identity(&call.name);
        let resolved = if active_capability == current_capability {
            step_tools.resolve_invocation(&call.name, &call.arguments)
        } else {
            Err(format!("capability `{}` is no longer available", call.name))
        };
        (step_tools, resolved)
    }

    fn tool_budget_denial(&self, operation_id: OperationId) -> Option<String> {
        self.budget.max_tool_calls.and_then(|max| {
            (self
                .live(operation_id)
                .expect("tool admission operation residency exists")
                .operation_tool_calls
                >= max)
                .then(|| "operation tool-call budget exhausted".to_owned())
        })
    }

    async fn prepare_tool_admission(
        call: &ToolCall,
        step_tools: &ToolRegistry,
        resolved: Result<ResolvedInvocation, String>,
        denial: Option<String>,
    ) -> PreparedToolAdmission {
        match resolved {
            Ok(resolved) => {
                if let Some(message) = denial {
                    return PreparedToolAdmission::Deny {
                        resolved: Some(resolved),
                        message,
                    };
                }
                match step_tools
                    .reconciliation_for(&call.name, &call.arguments)
                    .await
                {
                    Ok(reconciliation) => PreparedToolAdmission::Execute {
                        resolved,
                        reconciliation,
                    },
                    Err(message) => PreparedToolAdmission::Deny {
                        resolved: Some(resolved),
                        message,
                    },
                }
            }
            Err(message) => PreparedToolAdmission::Deny {
                resolved: None,
                message: denial.unwrap_or(message),
            },
        }
    }

    /// Commit a tool effect intent, then spawn the tool effect (or
    /// settle a validation denial through the normal path). Returns
    /// false when persistence failed.
    async fn admit_next_tool(&mut self, operation_id: OperationId) -> bool {
        // The policy gate runs before any effect intent is committed
        // (§17.3): peek the next call, canonicalize it, and decide.
        let Some(call) = self
            .active(operation_id)
            .and_then(|active| active.machine.next_planned_call().cloned())
        else {
            error!(session = %self.session_id, "admit with no planned call; fencing");
            self.closed = true;
            return false;
        };
        let (step_tools, resolved) = self.resolve_tool_for_admission(operation_id, &call);
        let decision = if let Some(message) = self.tool_budget_denial(operation_id) {
            // Budget denial wins before an approval can park the operation.
            PolicyDecision::Deny(message)
        } else {
            match &resolved {
                Ok(invocation) if invocation.policy_route == PolicyRoute::Structural => {
                    PolicyDecision::Allow
                }
                Ok(invocation) => self.policy.decide(&call.name, &invocation.canonical),
                // Resolution/validation failure is model-visible denial, not a
                // harness failure: the model produced an unusable input.
                Err(message) => PolicyDecision::Deny(message.clone()),
            }
        };
        if decision == PolicyDecision::ApprovalRequired {
            if self.interactive_approvals {
                // §17.4: park. The staged call is durable in the state
                // before acknowledgment; nothing executes and no effect
                // intent is committed until the decision arrives.
                let mut staged = self
                    .active(operation_id)
                    .cloned()
                    .expect("admit needs an operation");
                staged
                    .machine
                    .apply(Transition::RequestApproval)
                    .expect("request approval from ToolsPlanned");
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
                if let Err(err) = self.commit_transition(request).await {
                    self.fail_operation_on_persistence_for(operation_id, err)
                        .await;
                    return false;
                }
                self.next_entry_seq = new_entry_seq;
                staged.state_seq += 1;
                self.install_active(staged);
                let target = target_summary_registry(&step_tools, &call.name, &call.arguments);
                // Display-only proposed change for `edit` (bounded hunk
                // against current content); other tools park with target
                // alone, as before.
                let preview = if call.name == "edit" {
                    edit_approval_preview(&step_tools, &call.arguments).await
                } else {
                    None
                };
                self.emit(RuntimeEvent::ApprovalPending {
                    cursor: RuntimeCursor::default(),
                    operation_id: call.operation_id,
                    tool: call.name.clone(),
                    target,
                    preview,
                });
                info!(tool = %call.name, "approval requested; parking the operation");
                return true;
            }
            // Non-interactive: nothing may execute, so nothing is
            // committed as an effect intent; the operation terminates
            // with the durable ApprovalRequired outcome.
            let mut staged = self
                .active(operation_id)
                .cloned()
                .expect("admit needs an operation");
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
            if let Err(err) = self.commit_transition(request).await {
                self.fail_operation_on_persistence_for(operation_id, err)
                    .await;
                return false;
            }
            self.next_entry_seq = new_entry_seq;
            staged.state_seq += 1;
            self.install_active(staged);
            warn!(tool = %call.name, "approval required; terminating the operation");
            self.emit_terminal_state_for(operation_id, &applied.state);
            // Terminal: idle the operation here (the caller's arm
            // returns without re-reading state), synchronously so no
            // command can observe a Finished-but-open operation.
            self.remove_operation(operation_id);
            return true;
        }

        let denial = match decision {
            PolicyDecision::Deny(message) => Some(message),
            PolicyDecision::Allow => None,
            PolicyDecision::ApprovalRequired => unreachable!("handled above"),
        };
        let prepared = Self::prepare_tool_admission(&call, &step_tools, resolved, denial).await;
        self.commit_tool_admission(
            operation_id,
            Transition::AdmitNextTool,
            "admit next tool from ToolsPlanned",
            prepared,
        )
        .await
    }

    /// Apply a tool-admission transition, durably commit the effect
    /// intent with the exact canonical input and reconciliation
    /// evidence, then either spawn the tool effect or settle a
    /// model-visible denial through the normal tool-result path.
    /// Shared by the ordinary admit path and the approval path, so an
    /// approved call commits and executes exactly what the policy saw
    /// (§17.3). Returns false when persistence failed.
    async fn commit_tool_admission(
        &mut self,
        operation_id: OperationId,
        transition: Transition,
        expect: &'static str,
        prepared: PreparedToolAdmission,
    ) -> bool {
        let step_tools = self
            .active(operation_id)
            .map(|active| active.tool_registry.clone())
            .expect("tool admission needs the current step tool registry");
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("admit needs an operation");
        let applied = staged.machine.apply(transition).expect(expect);
        let EffectIntent::Tool { call } = applied.intents[0].clone() else {
            panic!("tool admission must yield a tool intent");
        };
        // The exact typed invocation the executor will use is part of the
        // durable intent (§17.3: never approve one string and execute a
        // materially different one). The prepared enum prevents execution
        // evidence from coexisting with a denial or an unresolved call.
        let (canonical, recovery_class, reconciliation, denial) = match prepared {
            PreparedToolAdmission::Execute {
                resolved,
                reconciliation,
            } => (
                Some(resolved.canonical),
                resolved.recovery_class,
                reconciliation,
                None,
            ),
            PreparedToolAdmission::Deny { resolved, message } => {
                let (canonical, recovery_class) = resolved
                    .map_or((None, RecoveryClass::NeverReplay), |resolved| {
                        (Some(resolved.canonical), resolved.recovery_class)
                    });
                (canonical, recovery_class, None, Some(message))
            }
        };
        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Tool(ToolInvocation {
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                call_id: call.call_id,
                canonical,
                reconciliation: reconciliation.clone(),
            }),
            recovery_class,
            1,
        );
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
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return false;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.install_active(staged);
        if let Some(message) = denial {
            // The denial settles through the normal tool-result path; the
            // tool never started, so no ToolStarted event is emitted.
            let _ = self.tool_tx.try_send(ToolSignal::Settled {
                operation_id: call.operation_id,
                effect_id,
                result: ToolResult::Err {
                    call_id: call.call_id,
                    error: message,
                    artifact: None,
                    images: Vec::new(),
                },
            });
        } else {
            self.wait_effect_boundary(EffectBoundary::ToolExecution)
                .await;
            self.live_mut(operation_id)
                .expect("main operation residency exists")
                .operation_tool_calls += 1;
            let target = target_summary_registry(&step_tools, &call.name, &call.arguments);
            self.emit_tool_started(call.operation_id, call.call_id, &call.name, target);
            let effect_id = self
                .active(operation_id)
                .and_then(|active| active.open_effect.as_ref().map(|effect| effect.id));
            self.spawn_tool_effect(effect_id, call, reconciliation, step_tools);
        }
        true
    }

    /// Durable approval decision for a parked operation (DESIGN.md
    /// §17.4). Approval re-runs the admission invariants — capability
    /// identity, canonical input, reconciliation evidence, budget — so
    /// the executor runs exactly what the policy saw; denial records a
    /// model-visible denial result and continues the operation.
    async fn decide_approval(
        &mut self,
        operation_id: OperationId,
        allow: bool,
    ) -> Result<(), CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        let Some(active) = self.active(operation_id) else {
            return if self.operations.is_empty() {
                Err(CommandError::NoActiveOperation)
            } else {
                Err(CommandError::NotActive { operation_id })
            };
        };
        let Some(call) = active.machine.state().staged_call().cloned() else {
            return Err(CommandError::NoPendingApproval { operation_id });
        };
        let mut staged = active.clone();
        if !allow {
            let applied = staged
                .machine
                .apply(Transition::DenyCall)
                .expect("deny a parked approval");
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
            if let Err(err) = self.commit_transition(request).await {
                let message = err.to_string();
                self.fail_operation_on_persistence_for(operation_id, err)
                    .await;
                return Err(CommandError::Persistence(message));
            }
            self.next_entry_seq = new_entry_seq;
            staged.state_seq += 1;
            let state = staged.machine.state().clone();
            self.install_active(staged);
            self.emit_terminal_state_for(operation_id, &state);
            info!(operation = %operation_id, "approval denied; the operation continues");
            self.advance(operation_id).await;
            return Ok(());
        }
        // Approval: the staged operation stays parked; the admission
        // commit below applies ApproveCall exactly once and commits the
        // effect intent. Re-run the admission invariants against the
        // staged call first.
        let (step_tools, resolved) = self.resolve_tool_for_admission(operation_id, &call);
        let denial = self.tool_budget_denial(operation_id);
        let prepared = Self::prepare_tool_admission(&call, &step_tools, resolved, denial).await;
        self.install_active(staged);
        let committed = self
            .commit_tool_admission(
                operation_id,
                Transition::ApproveCall,
                "approve a parked call",
                prepared,
            )
            .await;
        if committed {
            Ok(())
        } else {
            Err(CommandError::Persistence(
                "approval commit failed; the operation failed from its last checkpoint".to_owned(),
            ))
        }
    }

    async fn commit_transition(&mut self, mut request: CommitRequest) -> Result<(), StoreError> {
        let operation_id = request.operation_id;
        let lane_name = self
            .operation_lane_name(operation_id)
            .ok_or_else(|| {
                StoreError::Sqlite(format!(
                    "operation {operation_id} has no live lane ownership"
                ))
            })?
            .to_owned();
        let terminal = matches!(
            request.checkpoint.payload.state,
            OperationState::Finished(_)
        );
        let mut parent = self
            .lanes
            .get(&lane_name)
            .expect("operation lane exists while session runtime is live")
            .durable
            .state
            .leaf;
        for entry in &mut request.entries {
            entry.parent = parent;
            parent = Some(entry.id);
        }
        let entries = request.entries.clone();
        let new_leaf = entries.last().map(|entry| entry.id);
        self.store.commit(request).await?;
        self.install_tree_entries(entries);
        let lane = &mut self
            .lanes
            .get_mut(&lane_name)
            .expect("operation lane exists after durable commit")
            .durable;
        if let Some(new_leaf) = new_leaf {
            lane.state.leaf = Some(new_leaf);
        }
        lane.state.current_operation = if terminal { None } else { Some(operation_id) };
        Ok(())
    }

    fn emit_terminal_state_for(&mut self, operation_id: OperationId, state: &OperationState) {
        if let Some(live) = self.live_mut(operation_id) {
            live.live_tools.clear();
        }
        if let OperationState::Finished(outcome) = state {
            if self.operation_lane_name(operation_id) == Some(crate::session::lane::MAIN) {
                self.main_latest_settlement = Some(OperationSettlement {
                    operation_id,
                    outcome: outcome.clone(),
                });
                if matches!(outcome, OperationOutcome::Indeterminate) {
                    self.main_indeterminate_warning = Some(IndeterminateWarning {
                        operation_id,
                        message: INDETERMINATE_MESSAGE.to_owned(),
                    });
                }
            }
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
        let rx = self.main_events.subscribe();
        Ok((snapshot, EventSubscription { rx }))
    }

    fn subscribe_all(&mut self) -> SubscribeReply {
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
            indeterminate: self.main_indeterminate_warning.clone(),
            latest_settlement: self.main_latest_settlement.clone(),
            reopen_entry_count: self.reopen_entry_count,
            operation: match self.main_active() {
                None => OperationStatus::Idle,
                Some(active) => OperationStatus::Active {
                    operation_id: active.machine.operation_id(),
                    prompt: active.machine.prompt().to_owned(),
                    state: active.machine.state().clone(),
                },
            },
            entries: self
                .main_branch_records()
                .iter()
                .map(|record| record.entry.clone())
                .collect(),
            model_ref: self.main_model_ref().to_owned(),
            thinking: self.main_lane().config.thinking.clone(),
            pending_next_run: self
                .main_lane()
                .state
                .pending_next_run
                .as_ref()
                .map(|next_run| NextRunInput {
                    entry_id: next_run.entry_id,
                    prompt: next_run.prompt.clone(),
                }),
            latest_usage: self.main_lane_live().latest_usage,
            usage_totals: self.usage_totals,
            model_pricing: self.main_lane_live().model_pricing,
            context_window: self.main_lane_live().context_window,
            live: self.main_active().map(|_| LiveOperationState {
                draft_text: self
                    .main_live()
                    .expect("main operation residency exists")
                    .draft_text
                    .clone(),
                draft_thinking: self
                    .main_live()
                    .expect("main operation residency exists")
                    .draft_thinking
                    .clone(),
                pending_tools: self
                    .main_live()
                    .expect("main operation residency exists")
                    .live_tools
                    .clone(),
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
        let live = self
            .live_mut(operation_id)
            .expect("operation residency exists while starting tool effect");
        live.live_tools.retain(|pending| pending.call_id != call_id);
        live.live_tools.push(PendingTool {
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
        let operation_ids = self.operations.keys().copied().collect::<Vec<_>>();
        for operation_id in operation_ids {
            let Some(mut staged) = self.active(operation_id).cloned() else {
                continue;
            };
            let cancel = staged.cancel.clone();
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
            if let Some(gate) = &close_gate {
                gate.wait(EffectBoundary::CloseSuspendCommit).await;
            }
            match self.commit_transition(request).await {
                Ok(()) => {
                    self.next_entry_seq = new_entry_seq;
                    staged.state_seq += 1;
                    self.install_active(staged);
                }
                Err(err) => {
                    error!(
                        session = %self.session_id,
                        %operation_id,
                        %err,
                        "suspend checkpoint failed; durable operation stays open"
                    );
                }
            }
            cancel.cancel();
        }
        self.cancel_root.cancel();
        self.tracker.close();
        // Drain while joining: a provider blocked sending into a full
        // engine channel can only finish if someone keeps reading.
        let mut settled_shell: Vec<ShellSignal> = Vec::new();
        {
            let wait = self.tracker.wait();
            tokio::pin!(wait);
            let mut engine_open = true;
            let mut tool_open = true;
            let mut shell_open = true;
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
                    signal = self.shell_rx.recv(), if shell_open => {
                        match signal {
                            // A shell passthrough cancelled by close still
                            // settles durably: dropping it would lose the
                            // record of an external effect that ran. Collect
                            // here; settled after the join completes.
                            Some(signal) => settled_shell.push(signal),
                            None => shell_open = false,
                        }
                    }
                    else => break,
                }
            }
        }
        for signal in settled_shell {
            self.handle_shell_signal(signal).await;
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
        // Frontends project main, so sibling-lane traffic must not alter or
        // overflow their bounded event ring. Family waits retain a separate
        // session-wide stream and filter by exact operation identity.
        let is_main_event = event.operation_id().is_none_or(|operation_id| {
            self.operation_lane_name(operation_id) == Some(crate::session::lane::MAIN)
        });
        if is_main_event {
            let _ = self.main_events.send(event.clone());
        }
        // A full ring drops the oldest buffered events for that receiver; the
        // receiver detects the gap reliably. No receivers is the normal idle
        // case for either ring.
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
        | RuntimeEvent::ToolProgress { cursor: slot, .. }
        | RuntimeEvent::ToolSettled { cursor: slot, .. }
        | RuntimeEvent::UsageUpdate { cursor: slot, .. }
        | RuntimeEvent::OperationFinished { cursor: slot, .. }
        | RuntimeEvent::OperationFailed { cursor: slot, .. }
        | RuntimeEvent::OperationIndeterminate { cursor: slot, .. }
        | RuntimeEvent::OperationCancelled { cursor: slot, .. }
        | RuntimeEvent::OperationApprovalRequired { cursor: slot, .. }
        | RuntimeEvent::ApprovalPending { cursor: slot, .. }
        | RuntimeEvent::ShellStarted { cursor: slot, .. }
        | RuntimeEvent::ShellOutput { cursor: slot, .. }
        | RuntimeEvent::ShellSettled { cursor: slot, .. }
        | RuntimeEvent::SessionClosed { cursor: slot } => *slot = cursor,
    }
}

fn event_kind(event: &RuntimeEvent) -> &'static str {
    match event {
        RuntimeEvent::OperationStarted { .. } => "operation_started",
        RuntimeEvent::AssistantTextDelta { .. } => "assistant_text_delta",
        RuntimeEvent::ThinkingDelta { .. } => "thinking_delta",
        RuntimeEvent::ToolStarted { .. } => "tool_started",
        RuntimeEvent::ToolProgress { .. } => "tool_progress",
        RuntimeEvent::ToolSettled { .. } => "tool_settled",
        RuntimeEvent::UsageUpdate { .. } => "usage_update",
        RuntimeEvent::OperationFinished { .. } => "operation_finished",
        RuntimeEvent::OperationFailed { .. } => "operation_failed",
        RuntimeEvent::OperationIndeterminate { .. } => "operation_indeterminate",
        RuntimeEvent::OperationCancelled { .. } => "operation_cancelled",
        RuntimeEvent::OperationApprovalRequired { .. } => "operation_approval_required",
        RuntimeEvent::ApprovalPending { .. } => "approval_pending",
        RuntimeEvent::ShellStarted { .. } => "shell_started",
        RuntimeEvent::ShellOutput { .. } => "shell_output",
        RuntimeEvent::ShellSettled { .. } => "shell_settled",
        RuntimeEvent::SessionClosed { .. } => "session_closed",
    }
}

#[cfg(test)]
pub(crate) struct SaturatedHandle {
    handle: SessionHandle,
    _rx: mpsc::Receiver<SessionCommand>,
}

#[cfg(test)]
impl SaturatedHandle {
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::channel(1);
        let handle = SessionHandle { tx };
        handle
            .fill_queue()
            .expect("first fill occupies the bounded command queue");
        Self { handle, _rx: rx }
    }

    pub(crate) fn handle(&self) -> &SessionHandle {
        &self.handle
    }
}

#[cfg(test)]
impl SessionHandle {
    fn fill_queue(&self) -> Result<(), CommandError> {
        let (reply, _rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::SubmitIfIdle {
                lane_name: crate::session::lane::MAIN.to_owned(),
                prompt: String::from("fill"),
                reply,
            })
            .map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
                mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
            })
    }
}
