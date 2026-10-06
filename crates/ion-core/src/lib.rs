//! Ion's local coding loop, typed Session history, and abstract model/tool contracts.

mod agent;
mod generation;
mod request;
mod session;
mod tool_result;
mod tool_set;
mod transcript;

pub use agent::{
    Agent as CodingAgent, AgentError as CodingAgentError, AgentEvent as CodingAgentEvent,
    AgentLimits, PromptCacheWarmingPolicy, SteeringInbox,
};
pub use session::{
    ForkPoint, ModelContextSnapshot, Session as CodingSession, SessionEntry,
    SessionError as CodingSessionError, SessionView, StoredToolActivity, TurnEndReason,
    TurnSummary, UserShellPermit,
};
pub use tool_set::{
    ToolActivity, ToolActivityKind, ToolCatalog, ToolDefinition, ToolExecutor, ToolExposure,
    ToolPresentation, ToolPresentationTarget, ToolRegistration, ToolSet,
    ToolSource as CodingToolSource,
};

pub use tool_result::{ToolOutput as CodingToolOutput, ToolResultProjection};

pub use transcript::{
    ActivityGroup, ActivityResult, ActivityState, LiveTranscript, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection, UserShellActivity,
};
