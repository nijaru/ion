//! Styled text primitives for TUI rendering.
//!
//! Provides `StyledSpan`, `StyledLine`, and `LineBuilder` for building
//! styled terminal output.

use std::io::{self, Write};

/// Internal color model for TUI styled spans.
///
/// Uses the same naming convention as `crossterm::style::Color`; `ansi::map_color`
/// provides the direct conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Reset,
    Black,
    DarkGrey,
    Red,
    DarkRed,
    Green,
    DarkGreen,
    Yellow,
    DarkYellow,
    Blue,
    DarkBlue,
    Magenta,
    DarkMagenta,
    Cyan,
    DarkCyan,
    White,
    Grey,
    Rgb { r: u8, g: u8, b: u8 },
    AnsiValue(u8),
}

/// Lightweight text style model used throughout TUI chat rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TextStyle {
    pub foreground_color: Option<Color>,
    pub background_color: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underlined: bool,
    pub crossed_out: bool,
    pub reverse: bool,
}

impl TextStyle {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            foreground_color: None,
            background_color: None,
            bold: false,
            dim: false,
            italic: false,
            underlined: false,
            crossed_out: false,
            reverse: false,
        }
    }
}

/// A styled span of text.
#[derive(Clone, Debug)]
pub struct StyledSpan {
    pub content: String,
    pub style: TextStyle,
}

impl StyledSpan {
    /// Create a new styled span.
    pub fn new(content: impl Into<String>, style: TextStyle) -> Self {
        Self {
            content: content.into(),
            style,
        }
    }

    /// Create an unstyled span.
    pub fn raw(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: TextStyle::new(),
        }
    }

    /// Create a span with foreground color.
    pub fn colored(content: impl Into<String>, color: Color) -> Self {
        Self {
            content: content.into(),
            style: TextStyle {
                foreground_color: Some(color),
                ..TextStyle::new()
            },
        }
    }

    /// Create a dim span.
    pub fn dim(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: TextStyle {
                dim: true,
                ..TextStyle::new()
            },
        }
    }

    /// Create a bold span.
    pub fn bold(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: TextStyle {
                bold: true,
                ..TextStyle::new()
            },
        }
    }

    /// Create an italic span.
    pub fn italic(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: TextStyle {
                italic: true,
                ..TextStyle::new()
            },
        }
    }

    /// Add bold modifier to this span.
    #[must_use]
    pub fn with_bold(mut self) -> Self {
        self.style.bold = true;
        self
    }

    /// Add dim modifier to this span.
    #[must_use]
    pub fn with_dim(mut self) -> Self {
        self.style.dim = true;
        self
    }

    /// Add italic modifier to this span.
    #[must_use]
    pub fn with_italic(mut self) -> Self {
        self.style.italic = true;
        self
    }

    /// Add strikethrough modifier to this span.
    #[must_use]
    pub fn with_strikethrough(mut self) -> Self {
        self.style.crossed_out = true;
        self
    }

    /// Plain text content without ANSI escapes.
    #[must_use]
    pub fn plain_text(&self) -> &str {
        &self.content
    }

    /// Write this span to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let rendered = crate::tui::ansi::apply_style(&self.content, &self.style);
        write!(w, "{rendered}")
    }
}

/// A line of styled text.
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
        if self.spans.is_empty() {
            return Ok(());
        }
        for span in &self.spans {
            let rendered = crate::tui::ansi::apply_style(&span.content, &span.style);
            write!(w, "{rendered}")?;
        }
        Ok(())
    }

    /// Write this line to a writer, constrained to the terminal width.
    ///
    /// Uses one-cell right padding reservation (`width - 1`) to avoid
    /// accidental terminal autowrap at the far-right edge.
    pub fn write_to_width<W: Write>(&self, w: &mut W, width: u16) -> io::Result<()> {
        if self.spans.is_empty() {
            return Ok(());
        }
        let max_cells = width.saturating_sub(1) as usize;
        if max_cells == 0 {
            return Ok(());
        }
        let mut remaining = max_cells;
        for span in &self.spans {
            if remaining == 0 {
                break;
            }
            let clipped = crate::tui::text::truncate_to_width(&span.content, remaining);
            let used = crate::tui::text::display_width(&clipped);
            if !clipped.is_empty() {
                let rendered = crate::tui::ansi::apply_style(&clipped, &span.style);
                write!(w, "{rendered}")?;
            }
            remaining = remaining.saturating_sub(used);
        }
        Ok(())
    }

    /// Write this line to a writer with a trailing `\r\n`.
    pub fn writeln<W: Write>(&self, w: &mut W) -> io::Result<()> {
        self.write_to(w)?;
        write!(w, "\r\n")
    }

    /// Width-constrained variant of `writeln`.
    pub fn writeln_with_width<W: Write>(&self, w: &mut W, width: u16) -> io::Result<()> {
        self.write_to_width(w, width)?;
        write!(w, "\r\n")
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

    /// All spans' plain text concatenated (no ANSI escapes).
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.spans.iter().map(|s| s.content.as_str()).collect()
    }

    /// Render this line to a string with ANSI escape codes.
    #[must_use]
    pub fn to_ansi_string(&self) -> String {
        let mut buf = Vec::new();
        let _ = self.write_to(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Render this line to a width-constrained string with ANSI escape codes.
    #[must_use]
    pub fn to_ansi_string_with_width(&self, width: u16) -> String {
        let mut buf = Vec::new();
        let _ = self.write_to_width(&mut buf, width);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Total display width of all spans in terminal cells.
    #[must_use]
    pub fn display_width(&self) -> usize {
        self.spans
            .iter()
            .map(|s| crate::tui::text::display_width(&s.content))
            .sum()
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
    #[must_use]
    pub fn raw(mut self, content: impl Into<String>) -> Self {
        self.line.push(StyledSpan::raw(content));
        self
    }

    /// Add a colored span.
    #[must_use]
    pub fn colored(mut self, content: impl Into<String>, color: Color) -> Self {
        self.line.push(StyledSpan::colored(content, color));
        self
    }

    /// Add a dim span.
    #[must_use]
    pub fn dim(mut self, content: impl Into<String>) -> Self {
        self.line.push(StyledSpan::dim(content));
        self
    }

    /// Add a bold span.
    #[must_use]
    pub fn bold(mut self, content: impl Into<String>) -> Self {
        self.line.push(StyledSpan::bold(content));
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

    #[test]
    fn test_strikethrough_style() {
        let span = StyledSpan::raw("test").with_strikethrough();
        assert!(span.style.crossed_out);
    }

    #[test]
    fn test_color_enum_variants() {
        let _ = Color::White;
        let _ = Color::Grey;
        let _ = Color::DarkGrey;
    }

    #[test]
    fn styled_span_plain_text() {
        let span = StyledSpan::colored("hello", Color::Green);
        assert_eq!(span.plain_text(), "hello");
    }

    #[test]
    fn styled_line_plain_text_concatenates_spans() {
        let line = LineBuilder::new()
            .colored("> ", Color::Cyan)
            .raw("hello world")
            .build();
        assert_eq!(line.plain_text(), "> hello world");
    }

    #[test]
    fn styled_line_plain_text_empty() {
        assert_eq!(StyledLine::empty().plain_text(), "");
    }

    #[test]
    fn styled_line_display_width_ascii() {
        let line = StyledLine::raw("hello");
        assert_eq!(line.display_width(), 5);
    }

    #[test]
    fn styled_line_display_width_multi_span() {
        let line = LineBuilder::new().raw("hi").raw(" there").build();
        assert_eq!(line.display_width(), 8);
    }

    #[test]
    fn styled_line_display_width_cjk() {
        let line = StyledLine::raw("界a");
        assert_eq!(line.display_width(), 3);
    }
}
