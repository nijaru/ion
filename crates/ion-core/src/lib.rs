//! Runtime contract for Ion.
//!
//! The controller owns live turn state. Frontends hold a [`RuntimeHandle`]
//! and subscribe to [`RuntimeEvent`]s. Persistence, tools, and TUI state are
//! out of scope for this crate's first slice.

mod error;
mod ids;
mod provider;
mod runtime;
mod tool;

pub use error::{CommandError, RuntimeError};
pub use ids::{AgentId, RuntimeCursor, TurnId};
pub use provider::{Provider, ScriptedMessage, ScriptedProvider};
pub use runtime::{
    EventSubscription, PrintFrontend, Runtime, RuntimeEvent, RuntimeHandle, RuntimeSnapshot,
    TurnStatus,
};
pub use tool::{
    BashTool, EditTool, FindTool, ReadTool, SearchTool, Tool, ToolCall, ToolCallId, ToolOutcome,
    ToolRegistry, ToolResult, ToolSpec, WriteTool,
};

#[cfg(test)]
mod tests;
