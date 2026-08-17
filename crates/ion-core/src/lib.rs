//! Runtime contract for Ion.
//!
//! The controller owns live turn state. Frontends hold a [`RuntimeHandle`]
//! and subscribe to [`RuntimeEvent`]s. Persistence, tools, and TUI state are
//! out of scope for this crate's first slice.

mod error;
mod ids;
mod provider;
mod runtime;

pub use error::{CommandError, RuntimeError};
pub use ids::{AgentId, RuntimeCursor, TurnId};
pub use provider::{Provider, ScriptedChunk, ScriptedProvider};
pub use runtime::{
    EventSubscription, PrintFrontend, Runtime, RuntimeEvent, RuntimeHandle, RuntimeSnapshot,
    TurnStatus,
};

#[cfg(test)]
mod tests;
