//! Ion's shared agent loop, typed Session history, and abstract model/tool contracts.

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
mod tool_result;
mod tool_set;
mod transcript;

pub use completion::{
    CompletionDelivery, CompletionInbox, CompletionNotice, CompletionPermit, CompletionReceipt,
    CompletionRevocation, CompletionSignal, CompletionStatus,
};
pub use operation::{IdleSession, Operation, OperationError};

pub use agent::{
    Agent as CodingAgent, AgentError as CodingAgentError, AgentEvent as CodingAgentEvent,
    AgentLimits, CompletionSubmission, PromptCacheWarmingPolicy, RequestSubmission, SteeringInbox,
};
pub use delegation::{
    BackgroundJob, BackgroundSlot, BackgroundStartError, DelegationCompletion, DelegationFailure,
    DelegationOutcome, DelegationOwnership, DelegationPartialOutput, DelegationReceipt,
    DelegationRecord, DelegationSite, DelegationSpec, DelegationTarget, DelegationTask,
    MAX_DELEGATION_PARTIAL_BYTES,
};
pub use input::{AcceptedInput, InputBudget, InputReservation};
pub use invocation::{ToolInvocation, ToolOccurrence, ToolOrigin};
pub use session::{
    AuthorityScope, ContextPoint, ContextPosition, ForkPoint, ModelContextSnapshot,
    PreparedNavigation, RecoveryBinding, RecoveryPoint, RecoveryState, RequestKey, RequestReceipt,
    Session as CodingSession, SessionEntry, SessionError as CodingSessionError,
    SessionInputPreview, SessionMetadata, SessionView, StoredToolActivity, SubmissionRequest,
    TurnEndReason, TurnSummary, UnobservedUserShell, UserShellOutcome,
};
pub use session_usage::{ModelUsage, SessionUsage, TokenCount, UsageTotals};
pub use tool_set::{
    ToolActivity, ToolActivityKind, ToolCatalog, ToolDefinition, ToolExecutor, ToolExposure,
    ToolPresentation, ToolPresentationTarget, ToolRegistration, ToolSelection, ToolSet,
    ToolSource as CodingToolSource,
};

pub use composition::{
    ChildIntent, ChildOutcome, CodeLimits, CodeOutput, CodeReply, CodeRequest, CodeRequestKind,
    CodeResult, CodeRuntime, CodeTask,
};
pub use tool_result::{
    ToolOutcome, ToolOutput as CodingToolOutput, ToolResultProjection, ToolSettlement,
};

pub use transcript::{
    ActivityGroup, ActivityResult, ActivityState, LiveTranscript, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection, UserShellActivity,
};
