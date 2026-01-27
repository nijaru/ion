//! Direct crossterm terminal wrapper for TUI v2.
//!
//! This module provides a minimal terminal abstraction that:
//! - Uses native scrollback for chat history (println!)
//! - Manages a dynamic-height bottom area for UI (progress, input, status)
//! - Uses synchronized output to prevent flicker
//!
//! Design: See ai/design/tui-v2.md

#![allow(dead_code)] // Module is new, not yet integrated

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute,
    style::{Attribute, Color, ContentStyle, StyledContent},
    terminal::{self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate},
};
use std::io::{self, Write};

/// Terminal wrapper for direct crossterm rendering.
pub struct Terminal {
    /// Last known terminal width
    width: u16,
    /// Last known terminal height
    height: u16,
    /// Height of the managed bottom UI area
    ui_height: u16,
}

impl Terminal {
    /// Create a new terminal wrapper.
    pub fn new() -> io::Result<Self> {
        let (width, height) = terminal::size()?;
        Ok(Self {
            width,
            height,
            ui_height: 4, // Default: progress(1) + input(1+2 borders) + status(1)
        })
    }

    /// Update terminal size (call on resize event).
    pub fn update_size(&mut self) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Set the height of the managed bottom UI area.
    pub fn set_ui_height(&mut self, height: u16) {
        self.ui_height = height;
    }

    /// Get terminal width.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Get terminal height.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Get the UI area height.
    pub fn ui_height(&self) -> u16 {
        self.ui_height
    }

    /// Print text to native scrollback (chat history).
    /// This will scroll the terminal naturally.
    pub fn print_to_scrollback(&self, text: &str) -> io::Result<()> {
        println!("{}", text);
        Ok(())
    }

    /// Print styled text to native scrollback.
    pub fn print_styled_to_scrollback(&self, lines: &[StyledLine]) -> io::Result<()> {
        let mut stdout = io::stdout();
        for line in lines {
            line.write_to(&mut stdout)?;
            writeln!(stdout)?;
        }
        stdout.flush()
    }

    /// Begin rendering the bottom UI area.
    /// Call this before rendering progress/input/status.
    pub fn begin_ui_render(&self) -> io::Result<()> {
        let mut stdout = io::stdout();
        execute!(stdout, BeginSynchronizedUpdate)?;
        // Move to start of UI area and clear to end
        let ui_start = self.height.saturating_sub(self.ui_height);
        execute!(
            stdout,
            MoveTo(0, ui_start),
            Clear(ClearType::FromCursorDown)
        )?;
        Ok(())
    }

    /// End rendering the bottom UI area.
    pub fn end_ui_render(&self) -> io::Result<()> {
        execute!(io::stdout(), EndSynchronizedUpdate)
    }

    /// Render a single line at a specific row in the UI area.
    pub fn render_line(&self, row: u16, line: &StyledLine) -> io::Result<()> {
        let mut stdout = io::stdout();
        execute!(stdout, MoveTo(0, row))?;
        line.write_to(&mut stdout)?;
        stdout.flush()
    }

    /// Position the cursor at a specific location.
    pub fn move_cursor(&self, x: u16, y: u16) -> io::Result<()> {
        execute!(io::stdout(), MoveTo(x, y))
    }

    /// Hide the cursor.
    pub fn hide_cursor(&self) -> io::Result<()> {
        execute!(io::stdout(), Hide)
    }

    /// Show the cursor.
    pub fn show_cursor(&self) -> io::Result<()> {
        execute!(io::stdout(), Show)
    }

    /// Clear the entire screen and scrollback (for width changes).
    pub fn full_clear(&self) -> io::Result<()> {
        // CSI 3J - clear scrollback, CSI 2J - clear screen, CSI H - home
        print!("\x1b[3J\x1b[2J\x1b[H");
        io::stdout().flush()
    }
}

/// A styled span of text (crossterm equivalent of ratatui Span).
#[derive(Clone, Debug)]
pub struct StyledSpan {
    pub content: String,
    pub style: ContentStyle,
}

impl StyledSpan {
    /// Create a new styled span.
    pub fn new(content: impl Into<String>, style: ContentStyle) -> Self {
        Self {
            content: content.into(),
            style,
        }
    }

    /// Create an unstyled span.
    pub fn raw(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: ContentStyle::new(),
        }
    }

    /// Create a span with foreground color.
    pub fn colored(content: impl Into<String>, color: Color) -> Self {
        Self {
            content: content.into(),
            style: ContentStyle {
                foreground_color: Some(color),
                ..ContentStyle::default()
            },
        }
    }

    /// Create a dim span.
    pub fn dim(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: ContentStyle {
                attributes: Attribute::Dim.into(),
                ..ContentStyle::default()
            },
        }
    }

    /// Write this span to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let styled = StyledContent::new(self.style, &self.content);
        write!(w, "{}", styled)
    }
}

/// A line of styled text (crossterm equivalent of ratatui Line).
#[derive(Clone, Debug, Default)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl StyledLine {
    /// Create a new line from spans.
    pub fn new(spans: Vec<StyledSpan>) -> Self {
        Self { spans }
    }

    /// Create an empty line.
    pub fn empty() -> Self {
        Self { spans: Vec::new() }
    }

    /// Create a line from a single raw string.
    pub fn raw(content: impl Into<String>) -> Self {
        Self {
            spans: vec![StyledSpan::raw(content)],
        }
    }

    /// Write this line to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        for span in &self.spans {
            span.write_to(w)?;
        }
        Ok(())
    }

    /// Push a span to this line.
    pub fn push(&mut self, span: StyledSpan) {
        self.spans.push(span);
    }
}

/// Builder for creating styled lines.
pub struct LineBuilder {
    line: StyledLine,
}

impl LineBuilder {
    /// Create a new line builder.
    pub fn new() -> Self {
        Self {
            line: StyledLine::empty(),
        }
    }

    /// Add a raw (unstyled) span.
    pub fn raw(mut self, content: impl Into<String>) -> Self {
        self.line.push(StyledSpan::raw(content));
        self
    }

    /// Add a colored span.
    pub fn colored(mut self, content: impl Into<String>, color: Color) -> Self {
        self.line.push(StyledSpan::colored(content, color));
        self
    }

    /// Add a dim span.
    pub fn dim(mut self, content: impl Into<String>) -> Self {
        self.line.push(StyledSpan::dim(content));
        self
    }

    /// Add a styled span.
    pub fn styled(mut self, span: StyledSpan) -> Self {
        self.line.push(span);
        self
    }

    /// Build the line.
    pub fn build(self) -> StyledLine {
        self.line
    }
}

impl Default for LineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a ratatui Style to crossterm ContentStyle.
pub fn convert_style(style: &ratatui::style::Style) -> ContentStyle {
    let mut cs = ContentStyle::default();

    // Convert foreground color
    if let Some(fg) = style.fg {
        cs.foreground_color = Some(convert_color(fg));
    }

    // Convert background color
    if let Some(bg) = style.bg {
        cs.background_color = Some(convert_color(bg));
    }

    // Convert modifiers to attributes
    let mods = style.add_modifier;
    if mods.contains(ratatui::style::Modifier::BOLD) {
        cs.attributes.set(Attribute::Bold);
    }
    if mods.contains(ratatui::style::Modifier::DIM) {
        cs.attributes.set(Attribute::Dim);
    }
    if mods.contains(ratatui::style::Modifier::ITALIC) {
        cs.attributes.set(Attribute::Italic);
    }
    if mods.contains(ratatui::style::Modifier::UNDERLINED) {
        cs.attributes.set(Attribute::Underlined);
    }
    if mods.contains(ratatui::style::Modifier::REVERSED) {
        cs.attributes.set(Attribute::Reverse);
    }
    if mods.contains(ratatui::style::Modifier::CROSSED_OUT) {
        cs.attributes.set(Attribute::CrossedOut);
    }

    cs
}

/// Convert a ratatui Color to crossterm Color.
fn convert_color(color: ratatui::style::Color) -> Color {
    match color {
        ratatui::style::Color::Reset => Color::Reset,
        ratatui::style::Color::Black => Color::Black,
        ratatui::style::Color::Red => Color::DarkRed,
        ratatui::style::Color::Green => Color::DarkGreen,
        ratatui::style::Color::Yellow => Color::DarkYellow,
        ratatui::style::Color::Blue => Color::DarkBlue,
        ratatui::style::Color::Magenta => Color::DarkMagenta,
        ratatui::style::Color::Cyan => Color::DarkCyan,
        ratatui::style::Color::Gray => Color::Grey,
        ratatui::style::Color::DarkGray => Color::DarkGrey,
        ratatui::style::Color::LightRed => Color::Red,
        ratatui::style::Color::LightGreen => Color::Green,
        ratatui::style::Color::LightYellow => Color::Yellow,
        ratatui::style::Color::LightBlue => Color::Blue,
        ratatui::style::Color::LightMagenta => Color::Magenta,
        ratatui::style::Color::LightCyan => Color::Cyan,
        ratatui::style::Color::White => Color::White,
        ratatui::style::Color::Rgb(r, g, b) => Color::Rgb { r, g, b },
        ratatui::style::Color::Indexed(i) => Color::AnsiValue(i),
    }
}

/// Convert a ratatui Span to StyledSpan.
pub fn convert_span(span: &ratatui::text::Span) -> StyledSpan {
    StyledSpan {
        content: span.content.to_string(),
        style: convert_style(&span.style),
    }
}

/// Convert a ratatui Line to StyledLine.
pub fn convert_line(line: &ratatui::text::Line) -> StyledLine {
    StyledLine {
        spans: line.spans.iter().map(convert_span).collect(),
    }
}

/// Convert a Vec of ratatui Lines to Vec of StyledLines.
pub fn convert_lines(lines: &[ratatui::text::Line]) -> Vec<StyledLine> {
    lines.iter().map(convert_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_styled_span() {
        let span = StyledSpan::colored("hello", Color::Green);
        assert_eq!(span.content, "hello");
    }

    #[test]
    fn test_styled_line() {
        let line = LineBuilder::new()
            .raw("prefix: ")
            .colored("colored", Color::Blue)
            .dim(" (dim)")
            .build();
        assert_eq!(line.spans.len(), 3);
    }

    #[test]
    fn test_convert_ratatui_line() {
        use ratatui::style::{Color as RColor, Modifier, Style};
        use ratatui::text::{Line, Span};

        let ratatui_line = Line::from(vec![
            Span::raw("hello "),
            Span::styled(
                "world",
                Style::default()
                    .fg(RColor::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        let styled_line = convert_line(&ratatui_line);
        assert_eq!(styled_line.spans.len(), 2);
        assert_eq!(styled_line.spans[0].content, "hello ");
        assert_eq!(styled_line.spans[1].content, "world");
    }
}
