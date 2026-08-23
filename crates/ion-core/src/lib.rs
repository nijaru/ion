//! Runtime contract for Ion: one agent loop inside a durable,
//! single-writer session runtime (DESIGN.md).
//!
//! The process-level [`Runtime`] composes one or more sessions; each
//! loaded session has exactly one mutation authority: its private
//! `SessionRuntime` task driving the pure [`OperationMachine`] transition
//! core. Frontends hold a
//! [`SessionHandle`] and subscribe to [`RuntimeEvent`]s. Persistence,
//! tools, and TUI state are out of scope until their owning slices.

mod context;
mod delegate;
mod error;
mod extensions;
mod ids;
mod mcp;
mod policy;
mod provider;
mod rpc;
mod runtime;
mod session;
mod store;
mod tool;

pub use context::{ContextMessage, ContextPlan, SYSTEM_SECTION, project};
pub use delegate::{ChildSpec, DelegateConfig, DelegateTool, child_budget_default};
pub use error::{CommandError, RuntimeError};
pub use extensions::{ExtensionDef, ExtensionService};
pub use ids::{OperationId, RuntimeCursor, SessionId};
pub use mcp::{McpService, ServerDef};
pub use policy::{AllowlistPolicy, DefaultPolicy, PolicyDecision, PolicyEngine};
pub use provider::{
    EngineSignal, ModelConfig, Provider, ProviderRequest, ScriptedMessage, ScriptedProvider,
    SwitchingProvider, TokenUsage,
};
pub use runtime::{
    EventSubscription, LiveOperationState, OperationStatus, PendingTool, Runtime, RuntimeBudget,
    RuntimeEvent, RuntimeHandle, SessionHandle, SessionSnapshot,
};
pub use session::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition, TransitionError,
};
pub use store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    InboxStatus, LoadedOperation, LoadedSession, SessionRecord, SessionStore, StoreError,
    default_db_path,
};
pub use tool::{
    BashTool, CanonicalTarget, EditTool, FindTool, ReadTool, RecoveryClass, SearchTool, Tool,
    ToolArtifact, ToolCall, ToolCallId, ToolOutcome, ToolRegistry, ToolResult, ToolSpec, WriteTool,
};
pub use tool::{ToolCatalog, target_from_arguments};

#[cfg(test)]
mod tests;
