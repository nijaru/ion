//! Ion's local coding loop, typed Session history, and abstract model/tool contracts.

mod agent;
mod code_gateway;
mod composition;
mod generation;
mod input;
mod request;
mod session;
mod tool_result;
mod tool_set;
mod transcript;

pub use agent::{
    Agent as CodingAgent, AgentError as CodingAgentError, AgentEvent as CodingAgentEvent,
    AgentLimits, PromptCacheWarmingPolicy, SteeringInbox,
};
pub use input::{AcceptedInput, InputBudget, InputReservation};
pub use session::{
    ForkPoint, ModelContextSnapshot, Session as CodingSession, SessionEntry,
    SessionError as CodingSessionError, SessionView, StoredToolActivity, TurnEndReason,
    TurnSummary, UnobservedUserShell, UserShellOutcome, UserShellPermit,
};
pub use tool_set::{
    ToolActivity, ToolActivityKind, ToolCatalog, ToolDefinition, ToolExecutor, ToolExposure,
    ToolPresentation, ToolPresentationTarget, ToolRegistration, ToolSet,
    ToolSource as CodingToolSource,
};

pub use composition::{
    ChildIntent, ChildOutcome, CodeLimits, CodeReply, CodeRequest, CodeRequestKind, CodeRuntime,
    CodeTask, ToolOccurrence,
};
pub use tool_result::{ToolOutput as CodingToolOutput, ToolResultProjection};

pub use transcript::{
    ActivityGroup, ActivityResult, ActivityState, LiveTranscript, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection, UserShellActivity,
};
