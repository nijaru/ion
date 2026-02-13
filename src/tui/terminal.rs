//! Styled text primitives for TUI rendering.
//!
//! Provides `StyledSpan`, `StyledLine`, and `LineBuilder` for building
//! styled terminal output.

use crate::tui::rnk_text::render_no_wrap_text_line;
use crossterm::style::{Attribute, Color, ContentStyle};
use rnk::components::{Span as RnkSpan, Text};
use rnk::core::Color as RnkColor;
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

fn map_color(color: Color) -> RnkColor {
    match color {
        Color::Reset => RnkColor::Reset,
        Color::Black => RnkColor::Black,
        Color::DarkGrey => RnkColor::BrightBlack,
        Color::Red => RnkColor::BrightRed,
        Color::DarkRed => RnkColor::Red,
        Color::Green => RnkColor::BrightGreen,
        Color::DarkGreen => RnkColor::Green,
        Color::Yellow => RnkColor::BrightYellow,
        Color::DarkYellow => RnkColor::Yellow,
        Color::Blue => RnkColor::BrightBlue,
        Color::DarkBlue => RnkColor::Blue,
        Color::Magenta => RnkColor::BrightMagenta,
        Color::DarkMagenta => RnkColor::Magenta,
        Color::Cyan => RnkColor::BrightCyan,
        Color::DarkCyan => RnkColor::Cyan,
        Color::White => RnkColor::BrightWhite,
        Color::Grey => RnkColor::White,
        Color::Rgb { r, g, b } => RnkColor::Rgb(r, g, b),
        Color::AnsiValue(v) => RnkColor::Ansi256(v),
    }
}

fn display_width(text: &str) -> usize {
    text.chars().filter_map(UnicodeWidthChar::width).sum()
}

fn to_rnk_span(span: &StyledSpan) -> RnkSpan {
    let mut out = RnkSpan::new(span.content.clone());
    if let Some(color) = span.style.foreground_color {
        out = out.color(map_color(color));
    }
    if let Some(bg) = span.style.background_color {
        out = out.background(map_color(bg));
    }
    if span.style.attributes.has(Attribute::Bold) {
        out = out.bold();
    }
    if span.style.attributes.has(Attribute::Dim) {
        out = out.dim();
    }
    if span.style.attributes.has(Attribute::Italic) {
        out = out.italic();
    }
    if span.style.attributes.has(Attribute::Underlined) {
        out = out.underline();
    }
    if span.style.attributes.has(Attribute::CrossedOut) {
        out = out.strikethrough();
    }
    if span.style.attributes.has(Attribute::Reverse) {
        out = out.inverse();
    }
    out
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
        let span = to_rnk_span(self);
        let width = display_width(&self.content).max(1);
        let rendered = render_no_wrap_text_line(Text::spans(vec![span]), width);
        write!(w, "{rendered}")
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
        if self.spans.is_empty() {
            return Ok(());
        }

        let mut spans = Vec::with_capacity(self.spans.len());
        let mut line_width = 0usize;

        for span in &self.spans {
            line_width += display_width(&span.content);
            spans.push(to_rnk_span(span));
        }

        let rendered = render_no_wrap_text_line(Text::spans(spans), line_width.max(1));
        write!(w, "{rendered}")?;
        Ok(())
    }

    /// Write this line to a writer with a trailing `\r\n`.
    pub fn writeln<W: Write>(&self, w: &mut W) -> io::Result<()> {
        self.write_to(w)?;
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
    fn test_color_mapping_white_grey_semantics() {
        assert_eq!(map_color(Color::White), RnkColor::BrightWhite);
        assert_eq!(map_color(Color::Grey), RnkColor::White);
        assert_eq!(map_color(Color::DarkGrey), RnkColor::BrightBlack);
    }
}
