//! Ion-specific TUI widgets built on top of `crates/tui`.
//!
//! These live in ion (not the library) because they know about ion's
//! rendering types, markdown, and content model.

pub mod streaming;

pub use streaming::StreamingText;
