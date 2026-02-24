//! ANSI styling helpers using crossterm's `ContentStyle`.
//!
//! Single rendering layer for all TUI output — replaces rnk.
//! The `apply_style` function is used by `StyledSpan`/`StyledLine` in `terminal.rs`.
//! The `Span` type and `render_spans` function are used by the bottom-UI render layer.

use crossterm::style::{Attribute, ContentStyle};

// Re-export so callers can `use crate::tui::ansi::Color` without importing crossterm directly.
pub(crate) use crossterm::style::Color;

use crate::tui::terminal::{Color as TermColor, TextStyle};

/// Map `terminal::Color` to the equivalent `crossterm::style::Color`.
///
/// Both enums follow the Windows Console API naming convention, so the mapping
/// is a direct name-to-name correspondence.
pub(crate) fn map_color(color: TermColor) -> Color {
    match color {
        TermColor::Reset => Color::Reset,
        TermColor::Black => Color::Black,
        TermColor::DarkGrey => Color::DarkGrey,
        TermColor::Red => Color::Red,
        TermColor::DarkRed => Color::DarkRed,
        TermColor::Green => Color::Green,
        TermColor::DarkGreen => Color::DarkGreen,
        TermColor::Yellow => Color::Yellow,
        TermColor::DarkYellow => Color::DarkYellow,
        TermColor::Blue => Color::Blue,
        TermColor::DarkBlue => Color::DarkBlue,
        TermColor::Magenta => Color::Magenta,
        TermColor::DarkMagenta => Color::DarkMagenta,
        TermColor::Cyan => Color::Cyan,
        TermColor::DarkCyan => Color::DarkCyan,
        TermColor::White => Color::White,
        TermColor::Grey => Color::Grey,
        TermColor::Rgb { r, g, b } => Color::Rgb { r, g, b },
        TermColor::AnsiValue(v) => Color::AnsiValue(v),
    }
}

fn to_content_style(style: &TextStyle) -> ContentStyle {
    let mut cs = ContentStyle::default();
    if let Some(fg) = style.foreground_color {
        cs.foreground_color = Some(map_color(fg));
    }
    if let Some(bg) = style.background_color {
        cs.background_color = Some(map_color(bg));
    }
    if style.bold {
        cs.attributes.set(Attribute::Bold);
    }
    if style.dim {
        cs.attributes.set(Attribute::Dim);
    }
    if style.italic {
        cs.attributes.set(Attribute::Italic);
    }
    if style.underlined {
        cs.attributes.set(Attribute::Underlined);
    }
    if style.crossed_out {
        cs.attributes.set(Attribute::CrossedOut);
    }
    if style.reverse {
        cs.attributes.set(Attribute::Reverse);
    }
    cs
}

/// Apply a `TextStyle` to a string and return the ANSI-escaped output.
///
/// Used by `StyledSpan::write_to` and `StyledLine::write_to` in `terminal.rs`.
pub(crate) fn apply_style(s: &str, style: &TextStyle) -> String {
    if s.is_empty() {
        return String::new();
    }
    format!("{}", to_content_style(style).apply(s))
}
