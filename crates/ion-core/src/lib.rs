//! Durable core for Ion's provider-neutral coding harness.
//!
//! Authoritative session state has one mutation owner; provider, tool, and
//! agent effects execute concurrently outside that mutation line and become
//! authoritative only through durable transitions. Durable conversation state
//! is a tree addressed through independent lane cursors.

mod agent;
mod agent_host;
mod context;
mod effect;
mod error;
mod extensions;
mod harness;
mod ids;
mod mcp;
mod operation;
mod policy;
mod process;
mod provider;
mod rpc;
mod runtime;
mod session;
mod store;
mod tool;

pub use agent::{
    Error as AgentError, Family as AgentFamily, Observation as AgentObservation,
    Status as AgentStatus,
};
pub use agent_host::{
    HostedAgentConfig, HostedAgentRuntimes, agent_host_tools, hosted_agent_budget_default,
    hosted_agent_runtimes, install_agent_host_tools,
};
pub use context::{
    CapabilitySnapshot, ContextManifest, ContextMessage, ContextPlan, SYSTEM_SECTION,
    TrustedResource, load_trusted_resources, project, project_with_manifest,
};
pub use error::{CommandError, RuntimeError};
pub use extensions::{ExtensionDef, ExtensionService};
pub use ids::{AgentId, EntryId, OperationId, RuntimeCursor, RuntimeInstanceId, SessionId};
pub use mcp::{McpService, ServerDef};
pub use operation::{OperationOutcome, OperationState, SessionEntry};
pub use policy::{AllowlistPolicy, DefaultPolicy, PolicyDecision, PolicyEngine};
pub use process::SandboxMode;
pub use provider::{
    EngineSignal, ModelCapabilities, ModelConfig, Provider, ProviderRequest, ScriptedMessage,
    ScriptedProvider, SwitchingProvider, TokenUsage,
};
pub use runtime::{
    EventSubscription, HostedRuntimeConfig, IndeterminateWarning, LiveOperationState,
    OperationStatus, PendingTool, Runtime, RuntimeBudget, RuntimeEvent, SessionHandle,
    SessionSnapshot,
};
pub use store::{
    EntryRecord, LoadedSession, SessionRecord, SessionStore, StoreError, default_db_path,
};
pub use tool::{
    BashTool, CanonicalTarget, EditTool, FindTool, ReadTool, RecoveryClass, SearchTool, Tool,
    ToolArtifact, ToolCall, ToolCallId, ToolCatalogError, ToolOutcome, ToolRegistry, ToolResult,
    ToolSpec, WriteTool,
};
pub use tool::{ToolCatalog, target_from_arguments};

#[cfg(test)]
mod tests;
