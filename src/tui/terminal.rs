//! Direct crossterm terminal wrapper for TUI v2.
//!
//! This module provides a minimal terminal abstraction that:
//! - Uses native scrollback for chat history (println!)
//! - Manages a dynamic-height bottom area for UI (progress, input, status)
//! - Uses synchronized output to prevent flicker
//!
//! Design: See ai/design/tui-v2.md

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
    #[must_use] 
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Get terminal height.
    #[must_use] 
    pub fn height(&self) -> u16 {
        self.height
    }

    /// Get the UI area height.
    #[must_use] 
    pub fn ui_height(&self) -> u16 {
        self.ui_height
    }

    /// Print text to native scrollback (chat history).
    /// This will scroll the terminal naturally.
    pub fn print_to_scrollback(&self, text: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        write!(stdout, "{text}\r\n")?;
        stdout.flush()
    }

    /// Print styled text to native scrollback.
    pub fn print_styled_to_scrollback(&self, lines: &[StyledLine]) -> io::Result<()> {
        let mut stdout = io::stdout();
        for line in lines {
            line.write_to(&mut stdout)?;
            write!(stdout, "\r\n")?;
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

    /// Create a bold span.
    pub fn bold(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: ContentStyle {
                attributes: Attribute::Bold.into(),
                ..ContentStyle::default()
            },
        }
    }

    /// Create an italic span.
    pub fn italic(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: ContentStyle {
                attributes: Attribute::Italic.into(),
                ..ContentStyle::default()
            },
        }
    }

    /// Create a colored bold span.
    pub fn colored_bold(content: impl Into<String>, color: Color) -> Self {
        let mut style = ContentStyle {
            foreground_color: Some(color),
            ..ContentStyle::default()
        };
        style.attributes.set(Attribute::Bold);
        Self {
            content: content.into(),
            style,
        }
    }

    /// Add bold modifier to this span.
    #[must_use] 
    pub fn with_bold(mut self) -> Self {
        self.style.attributes.set(Attribute::Bold);
        self
    }

    /// Add dim modifier to this span.
    #[must_use] 
    pub fn with_dim(mut self) -> Self {
        self.style.attributes.set(Attribute::Dim);
        self
    }

    /// Add italic modifier to this span.
    #[must_use] 
    pub fn with_italic(mut self) -> Self {
        self.style.attributes.set(Attribute::Italic);
        self
    }

    /// Write this span to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let styled = StyledContent::new(self.style, &self.content);
        write!(w, "{styled}")
    }
}

/// A line of styled text (crossterm equivalent of ratatui Line).
#[derive(Clone, Debug, Default)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl StyledLine {
    /// Create a new line from spans.
    #[must_use] 
    pub fn new(spans: Vec<StyledSpan>) -> Self {
        Self { spans }
    }

    /// Create an empty line.
    #[must_use] 
    pub fn empty() -> Self {
        Self { spans: Vec::new() }
    }

    /// Create a line from a single raw string.
    pub fn raw(content: impl Into<String>) -> Self {
        Self {
            spans: vec![StyledSpan::raw(content)],
        }
    }

    /// Create a line from a single colored span.
    pub fn colored(content: impl Into<String>, color: Color) -> Self {
        Self {
            spans: vec![StyledSpan::colored(content, color)],
        }
    }

    /// Create a line from a single dim span.
    pub fn dim(content: impl Into<String>) -> Self {
        Self {
            spans: vec![StyledSpan::dim(content)],
        }
    }

    /// Write this line to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        for span in &self.spans {
            span.write_to(w)?;
        }
        Ok(())
    }

    /// Print this line to stdout with a trailing newline.
    pub fn println(&self) -> io::Result<()> {
        let mut stdout = io::stdout();
        self.write_to(&mut stdout)?;
        write!(stdout, "\r\n")?;
        stdout.flush()
    }

    /// Push a span to this line.
    pub fn push(&mut self, span: StyledSpan) {
        self.spans.push(span);
    }

    /// Extend this line with spans from another line.
    pub fn extend(&mut self, other: StyledLine) {
        self.spans.extend(other.spans);
    }

    /// Check if this line is empty (no spans or only empty spans).
    #[must_use] 
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty() || self.spans.iter().all(|s| s.content.is_empty())
    }

    /// Prepend a span to the beginning of this line.
    pub fn prepend(&mut self, span: StyledSpan) {
        self.spans.insert(0, span);
    }
}

/// Builder for creating styled lines.
pub struct LineBuilder {
    line: StyledLine,
}

impl LineBuilder {
    /// Create a new line builder.
    #[must_use] 
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

    /// Add a bold span.
    pub fn bold(mut self, content: impl Into<String>) -> Self {
        self.line.push(StyledSpan::bold(content));
        self
    }

    /// Add an italic span.
    pub fn italic(mut self, content: impl Into<String>) -> Self {
        self.line.push(StyledSpan::italic(content));
        self
    }

    /// Add a colored bold span.
    pub fn colored_bold(mut self, content: impl Into<String>, color: Color) -> Self {
        self.line.push(StyledSpan::colored_bold(content, color));
        self
    }

    /// Add a styled span.
    #[must_use] 
    pub fn styled(mut self, span: StyledSpan) -> Self {
        self.line.push(span);
        self
    }

    /// Build the line.
    #[must_use] 
    pub fn build(self) -> StyledLine {
        self.line
    }
}

impl Default for LineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Print `StyledLines` directly to stdout (for v2 scrollback rendering).
pub fn print_styled_lines_to_scrollback(lines: &[StyledLine]) -> io::Result<()> {
    let mut stdout = io::stdout();
    for line in lines {
        line.write_to(&mut stdout)?;
        write!(stdout, "\r\n")?;
    }
    stdout.flush()
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
    fn test_styled_span_modifiers() {
        let span = StyledSpan::bold("bold text");
        assert_eq!(span.content, "bold text");

        let span = StyledSpan::colored("colored", Color::Red).with_bold();
        assert_eq!(span.content, "colored");
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
    fn test_styled_line_methods() {
        let mut line = StyledLine::raw("hello");
        assert!(!line.is_empty());

        line.push(StyledSpan::raw(" world"));
        assert_eq!(line.spans.len(), 2);

        line.prepend(StyledSpan::colored("> ", Color::Cyan));
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "> ");
    }

    #[test]
    fn test_line_builder() {
        let line = LineBuilder::new()
            .colored("> ", Color::Cyan)
            .raw("hello world")
            .build();
        assert_eq!(line.spans.len(), 2);
    }
}
