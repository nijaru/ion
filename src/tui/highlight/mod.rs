//! Syntax highlighting for tool output and markdown rendering.

mod diff;
pub mod markdown;
pub mod syntax;
#[cfg(test)]
mod tests;

pub use diff::highlight_diff_line;
pub use markdown::{highlight_markdown_with_width, render_markdown};
pub use syntax::{detect_syntax, highlight_code, highlight_line, syntax_from_fence};
