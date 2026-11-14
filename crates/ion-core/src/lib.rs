//! Ion's shared agent loop, typed Session history, and abstract model/tool contracts.
//!
//! # Rust embedding
//!
//! Supply a configured [`ion_ai::ModelService`], a logical [`ion_ai::ModelRef`],
//! and your own [`ToolSource`]. Core does not load credentials, project
//! instructions, or the coding application's tool catalog. The same loop works
//! with domain-specific tools; the model chooses which calls to make. Create a
//! [`Session`] once with [`Session::create`] (`None` for no coding workspace),
//! then share it with the host and reuse it for later inputs.
//!
//! ```no_run
//! use std::sync::Arc;
//! use ion_ai::{ModelRef, ModelService};
//! use ion_core::{Agent, AgentError, Session, ToolSource};
//! use tokio_util::sync::CancellationToken;
//!
//! async fn answer_order_question(
//!     service: Arc<dyn ModelService>,
//!     order_tools: Arc<dyn ToolSource>,
//!     model: ModelRef,
//!     session: Arc<Session>,
//!     stop: CancellationToken,
//! ) -> Result<String, AgentError> {
//!     let agent = Agent::new(service, order_tools, model);
//!     let operation = agent.submit(
//!         session,
//!         "What is the status of order 42?".into(),
//!         "Use the supplied order tools; do not invent order data.".into(),
//!         stop,
//!         |_| {}, // Optionally consume AgentEvent streaming progress here.
//!     );
//!     operation.await
//! }
//! ```
//!
//! Call submission methods inside an active Tokio runtime. An [`Operation`]
//! retains execution, rather than merely describing a future. Dropping the
//! operation requests stop but does not prove that tools have finished. When
//! requesting cancellation through the token or [`Operation::request_stop`],
//! keep and await the operation. Errors and cancellation do not undo effects.
//! Executors must settle work they retain or explicitly transfer its ownership.
//!
//! Before destroying the Tokio runtime, complete [`Session::shutdown`] to close
//! execution admission and join retained work. Transferred background jobs
//! remain their destination's responsibility. [`Session::open`] can reopen a
//! journal once its previous writer has released it. Opening history is passive,
//! not permission to replay missing effects. Streaming events include
//! provisional deltas; the Session owns
//! committed facts. See [`Agent::submit_request`] for stable delivery keys and
//! reconciliation when a host may redeliver the same input.

mod agent;
mod code_gateway;
mod completion;
mod composition;
mod delegation;
mod generation;
mod input;
mod invocation;
mod json_size;
mod operation;
mod prompt_size;
mod request;
mod session;
mod session_usage;
mod summary;
mod tool_policy;
mod tool_result;
mod tool_set;
mod transcript;

pub use completion::{
    CompletionDelivery, CompletionInbox, CompletionNotice, CompletionPermit, CompletionReceipt,
    CompletionRevocation, CompletionSignal, CompletionStatus,
};
pub use operation::{IdleSession, Operation, OperationError, OperationFence};

pub use agent::{
    Agent, AgentError, AgentEvent, AgentLimits, CompletionSubmission, PromptCacheWarmingPolicy,
    RequestSubmission, SteeringInbox,
};
pub use delegation::{
    BackgroundJob, BackgroundSlot, BackgroundStartError, DelegationCompletion, DelegationFailure,
    DelegationOutcome, DelegationOwnership, DelegationPartialOutput, DelegationReceipt,
    DelegationRecord, DelegationSite, DelegationSpec, DelegationTarget, DelegationTask,
    MAX_DELEGATION_PARTIAL_BYTES,
};
pub use input::{
    CommittedInput, InputBudget, InputCommit, InputReceipt, InputReservation, InputStatus,
    PreparedInput,
};
pub use invocation::{ToolInvocation, ToolOccurrence, ToolOrigin};
pub use request::{ContextCapacity, ContextEstimateBasis, ContextReport};
pub use session::{
    ArtifactPage, ArtifactRef, ArtifactSink, ArtifactWriter, AuthorityScope, ContextPoint,
    ContextPosition, ForkPoint, ModelContextSnapshot, ModelUsagePurpose, PreparedNavigation,
    RecoveryBinding, RecoveryPoint, RecoveryState, RequestKey, RequestReceipt, Session,
    SessionEntry, SessionError, SessionInputPreview, SessionIo, SessionMetadata, SessionReader,
    SessionRetirement, SessionRetirements, SessionStatus, SessionView, StoredToolActivity,
    SubmissionRequest, TurnEndReason, TurnSummary, UnobservedUserShell, UserShellOutcome,
};
pub use session_usage::{ModelUsage, SessionUsage, TokenCount, UsageTotals};
pub use tool_policy::{ToolDecision, ToolPolicy, ToolPolicyRequest};
pub use tool_set::{
    ToolActivity, ToolActivityKind, ToolCatalog, ToolDefinition, ToolExecutor, ToolExposure,
    ToolPresentation, ToolPresentationTarget, ToolRegistration, ToolResultSchema, ToolSelection,
    ToolSet, ToolSource, tool_name_matches,
};

pub use composition::{
    ChildIntent, ChildOutcome, CodeLimits, CodeOutput, CodeReply, CodeRequest, CodeRequestKind,
    CodeResult, CodeRuntime, CodeTask,
};
pub use tool_result::{ToolOutcome, ToolOutput, ToolResultProjection, ToolSettlement};

pub use transcript::{
    ActivityGroup, ActivityResult, ActivityState, GroupThinking, LiveTranscript,
    TranscriptActivity, TranscriptActivityIdentity, TranscriptItem, TranscriptMessage,
    TranscriptPart, TranscriptProjection, TranscriptSource, UserShellActivity,
};
