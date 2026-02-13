//! Diff syntax highlighting.

use crate::tui::terminal::{Color, StyledLine, StyledSpan};

/// Highlight a diff line with appropriate colors.
/// - `+` lines → Green (additions)
/// - `-` lines → Red (deletions)
/// - `@` lines → Cyan (hunk headers)
/// - Other lines → Dim (context)
pub fn highlight_diff_line(line: &str) -> StyledLine {
    if line.starts_with('+') && !line.starts_with("+++") {
        StyledLine::colored(line.to_string(), Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        StyledLine::colored(line.to_string(), Color::Red)
    } else if line.starts_with('@') {
        StyledLine::colored(line.to_string(), Color::Cyan)
    } else if line.starts_with("+++") || line.starts_with("---") {
        StyledLine::new(vec![StyledSpan::bold(line.to_string())])
    } else {
        StyledLine::dim(line.to_string())
    }
}
