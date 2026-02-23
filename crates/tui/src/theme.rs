//! Application theme — a named palette of [`Style`] values.
//!
//! Widgets query the theme for semantic colors rather than hard-coding
//! specific terminal colors, making it easy to swap themes at runtime.
//!
//! The default theme uses standard ANSI colors that render well on both
//! light and dark terminals.

use crate::style::{Color, Style};

/// A named palette of [`Style`] values passed through the render tree.
///
/// Applications create a `Theme` (or use [`Theme::default`]) and may later
/// apply it through a `RenderContext`. Widgets that accept a theme use it
/// rather than hard-coded colors.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Normal foreground text.
    pub text: Style,
    /// Dimmed / secondary text.
    pub text_dim: Style,
    /// Border lines.
    pub border: Style,
    /// Border lines when the widget has focus.
    pub border_focused: Style,
    /// Highlighted / selected item.
    pub selected: Style,
    /// Widget title text (e.g. block title).
    pub title: Style,
    /// Text input area.
    pub input: Style,
    /// Input cursor cell.
    pub input_cursor: Style,
    /// Scrollbar track and thumb.
    pub scrollbar: Style,
    /// Error messages.
    pub error: Style,
    /// Warning messages.
    pub warning: Style,
    /// Success / confirmation messages.
    pub success: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            text: Style::new(),
            text_dim: Style::new().fg(Color::DarkGray),
            border: Style::new().fg(Color::DarkGray),
            border_focused: Style::new().fg(Color::Cyan),
            selected: Style::new().fg(Color::Black).bg(Color::Cyan),
            title: Style::new().fg(Color::White).bold(),
            input: Style::new(),
            input_cursor: Style::new().fg(Color::Black).bg(Color::White),
            scrollbar: Style::new().fg(Color::DarkGray),
            error: Style::new().fg(Color::Red),
            warning: Style::new().fg(Color::Yellow),
            success: Style::new().fg(Color::Green),
        }
    }
}

impl Theme {
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the normal text style.
    pub fn text(mut self, style: Style) -> Self {
        self.text = style;
        self
    }

    /// Override the border style.
    pub fn border(mut self, style: Style) -> Self {
        self.border = style;
        self
    }

    /// Override the focused border style.
    pub fn border_focused(mut self, style: Style) -> Self {
        self.border_focused = style;
        self
    }

    /// Override the selected/highlighted item style.
    pub fn selected(mut self, style: Style) -> Self {
        self.selected = style;
        self
    }
}
