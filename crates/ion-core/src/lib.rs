//! Ion's local coding loop, typed Session history, provider and host-tool boundaries.

mod agent;
mod credentials;
mod local_tools;
mod model_http;
mod session;

pub use agent::{
    Agent as CodingAgent, AgentError as CodingAgentError, AgentEvent as CodingAgentEvent,
    AgentLimits, SteeringInbox, ToolHost as CodingToolHost, ToolOutput as CodingToolOutput,
};
pub use credentials::{CredentialResolutionError, CredentialResolver};
pub use local_tools::LocalTools;
pub use model_http::{HttpModelService, HttpWire};
pub use session::{
    Session as CodingSession, SessionEntry, SessionError as CodingSessionError, SessionView,
    TurnEndReason,
};
