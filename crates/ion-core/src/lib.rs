//! Ion's local coding loop, typed Session history, provider and host-tool boundaries.

mod agent;
mod credentials;
mod local_tools;
mod model_http;
mod session;
mod tool_set;
mod transcript;

pub use agent::{
    Agent as CodingAgent, AgentError as CodingAgentError, AgentEvent as CodingAgentEvent,
    AgentLimits, SteeringInbox,
};
pub use credentials::{CredentialResolutionError, CredentialResolver};
pub use local_tools::LocalTools;
pub use model_http::{HttpModelService, HttpWire};
pub use session::{
    ForkPoint, Session as CodingSession, SessionEntry, SessionError as CodingSessionError,
    SessionView, StoredToolActivity, TurnEndReason, TurnSummary, UserShellPermit,
};
pub use tool_set::{
    ToolActivity, ToolActivityKind, ToolCatalog, ToolDefinition, ToolHost as CodingToolHost,
    ToolOutput as CodingToolOutput, ToolPresentation, ToolPresentationTarget, ToolSet,
};

pub use transcript::{
    ActivityGroup, ActivityOutcome, ActivityResult, LiveTranscript, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection, UserShellActivity,
};
