//! Syntax highlighting for tool output and markdown rendering.

mod diff;
pub(crate) mod markdown;
mod syntax;
#[cfg(test)]
mod tests;

pub use diff::highlight_diff_line;
pub use markdown::{highlight_markdown_with_width, render_markdown};
pub use syntax::{detect_syntax, highlight_line};
