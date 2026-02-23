//! Ion-specific TUI widgets built on top of `crates/tui`.
//!
//! These live in ion (not the library) because they know about ion's
//! rendering types, markdown, and content model.

pub mod code_block;
pub mod conversation;
pub mod diff_view;
pub mod streaming;
pub mod tool_call;

pub use code_block::CodeBlock;
pub use conversation::{ConversationEntry, ConversationView, EntryRole};
pub use diff_view::DiffView;
pub use streaming::StreamingText;
pub use tool_call::{ToolCallView, ToolState};
