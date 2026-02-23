//! Ion-specific TUI widgets built on top of `crates/tui`.
//!
//! These live in ion (not the library) because they know about ion's
//! rendering types, markdown, and content model.

pub mod conversation;
pub mod streaming;
pub mod tool_call;

pub use conversation::{ConversationEntry, ConversationView, EntryRole};
pub use streaming::StreamingText;
pub use tool_call::{ToolCallView, ToolState};
