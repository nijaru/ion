//! Ion's local coding loop, typed Session history, and abstract model/tool contracts.

mod agent;
mod generation;
mod session;
mod tool_set;
mod transcript;

pub use agent::{
    Agent as CodingAgent, AgentError as CodingAgentError, AgentEvent as CodingAgentEvent,
    AgentLimits, SteeringInbox,
};
pub use session::{
    ForkPoint, ModelContextSnapshot, Session as CodingSession, SessionEntry,
    SessionError as CodingSessionError, SessionView, StoredToolActivity, TurnEndReason,
    TurnSummary, UserShellPermit,
};
pub use tool_set::{
    ToolActivity, ToolActivityKind, ToolCatalog, ToolDefinition, ToolExposure,
    ToolHost as CodingToolHost, ToolOutput as CodingToolOutput, ToolPresentation,
    ToolPresentationTarget, ToolSet,
};

pub use transcript::{
    ActivityGroup, ActivityOutcome, ActivityResult, LiveTranscript, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection, UserShellActivity,
};
