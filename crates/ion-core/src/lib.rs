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
mod error;
mod ids;
mod policy;
mod provider;
mod runtime;
mod session;
mod store;
mod tool;

pub use context::{ContextMessage, ContextPlan, SYSTEM_SECTION, project};
pub use error::{CommandError, RuntimeError};
pub use ids::{OperationId, RuntimeCursor, SessionId};
pub use policy::{AllowlistPolicy, DefaultPolicy, PolicyDecision, PolicyEngine};
pub use provider::{
    EngineSignal, Provider, ProviderRequest, ScriptedMessage, ScriptedProvider, TokenUsage,
};
pub use runtime::{
    EventSubscription, OperationStatus, Runtime, RuntimeEvent, RuntimeHandle, SessionHandle,
    SessionSnapshot,
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
pub use tool::target_from_arguments;
pub use tool::{
    BashTool, CanonicalTarget, EditTool, FindTool, ReadTool, RecoveryClass, SearchTool, Tool,
    ToolCall, ToolCallId, ToolOutcome, ToolRegistry, ToolResult, ToolSpec, WriteTool,
};

#[cfg(test)]
mod tests;
