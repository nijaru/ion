//! Text widget — renders styled, optionally wrapped text.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    buffer::Buffer,
    geometry::Rect,
    layout::LayoutStyle,
    style::Style,
    widgets::{Element, IntoElement, Renderable, WidgetId},
};

// ── Public types ──────────────────────────────────────────────────────────────

/// A styled segment of text within a line.
#[derive(Debug, Clone)]
pub struct Span {
    pub content: String,
    pub style: Style,
}

impl Span {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            style: Style::default(),
        }
    }

    pub fn styled(content: impl Into<String>, style: Style) -> Self {
        Self {
            content: content.into(),
            style,
        }
    }
}

impl From<&str> for Span {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for Span {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Horizontal alignment of text within its area.
#[derive(Debug, Clone, Copy, Default)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

/// How text should be wrapped when it exceeds the available width.
#[derive(Debug, Clone, Copy, Default)]
pub enum WrapMode {
    /// Break at word boundaries (spaces). Default.
    #[default]
    Word,
    /// Break at any character (grapheme cluster).
    Char,
    /// No wrapping — clip to available width.
    Clip,
}

/// A text widget with optional spanning, alignment, and wrapping.
pub struct Text {
    spans: Vec<Span>,
    alignment: Alignment,
    wrap: WrapMode,
    style: Style,
}

impl Text {
    /// Plain unstyled text.
    pub fn new(s: impl Into<String>) -> Self {
        Self {
            spans: vec![Span::new(s)],
            alignment: Alignment::default(),
            wrap: WrapMode::default(),
            style: Style::default(),
        }
    }

    /// Multi-span text (each span can have a different style).
    pub fn spans(spans: Vec<Span>) -> Self {
        Self {
            spans,
            alignment: Alignment::default(),
            wrap: WrapMode::default(),
            style: Style::default(),
        }
    }

    /// Single span with an explicit style.
    pub fn styled(s: impl Into<String>, style: Style) -> Self {
        Self {
            spans: vec![Span::styled(s, style)],
            alignment: Alignment::default(),
            wrap: WrapMode::default(),
            style: Style::default(),
        }
    }

    pub fn alignment(mut self, a: Alignment) -> Self {
        self.alignment = a;
        self
    }

    pub fn wrap(mut self, w: WrapMode) -> Self {
        self.wrap = w;
        self
    }

    pub fn style(mut self, s: Style) -> Self {
        self.style = s;
        self
    }

    pub fn bold(mut self) -> Self {
        self.style = self.style.bold();
        self
    }

    pub fn dim(mut self) -> Self {
        self.style = self.style.dim();
        self
    }

    pub fn italic(mut self) -> Self {
        self.style = self.style.italic();
        self
    }

    pub fn underline(mut self) -> Self {
        self.style = self.style.underline();
        self
    }
}

impl IntoElement for Text {
    fn into_element(self) -> Element {
        let renderer = TextRenderer {
            spans: self.spans,
            alignment: self.alignment,
            wrap: self.wrap,
            base_style: self.style,
        };
        Element {
            id: WidgetId::new(),
            inner: Box::new(renderer),
            layout_style: LayoutStyle::default(),
            children: vec![],
        }
    }
}

// ── Internal renderer ─────────────────────────────────────────────────────────

struct TextRenderer {
    spans: Vec<Span>,
    alignment: Alignment,
    wrap: WrapMode,
    base_style: Style,
}

impl Renderable for TextRenderer {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let width = area.width as usize;
        let lines = self.wrap_spans(width);

        for (row, line) in lines.iter().enumerate() {
            if row as u16 >= area.height {
                break;
            }
            let y = area.y + row as u16;
            let line_width: usize = line
                .iter()
                .map(|(s, _)| UnicodeWidthStr::width(s.as_str()))
                .sum();

            // Compute x offset for alignment.
            let x_start = match self.alignment {
                Alignment::Left => area.x,
                Alignment::Center => {
                    let pad = (width.saturating_sub(line_width)) / 2;
                    area.x + pad as u16
                }
                Alignment::Right => {
                    let pad = width.saturating_sub(line_width);
                    area.x + pad as u16
                }
            };

            let mut x = x_start;
            for (segment, style) in line {
                let combined = self.base_style.patch(*style);
                x = buf.set_string_truncated(x, y, segment, area.x + area.width - x, combined);
                if x >= area.x + area.width {
                    break;
                }
            }
        }
    }
}

impl TextRenderer {
    /// Wrap spans into lines of at most `width` grapheme columns.
    fn wrap_spans(&self, width: usize) -> Vec<Vec<(String, Style)>> {
        if width == 0 {
            return vec![];
        }
        match self.wrap {
            WrapMode::Clip => self.clip_spans(width),
            WrapMode::Char => self.char_wrap(width),
            WrapMode::Word => self.word_wrap(width),
        }
    }

    /// No wrapping: each logical span becomes exactly one line, clipped.
    fn clip_spans(&self, width: usize) -> Vec<Vec<(String, Style)>> {
        let mut lines: Vec<Vec<(String, Style)>> = vec![vec![]];
        let mut col = 0usize;

        for span in &self.spans {
            let style = span.style;
            for grapheme in span.content.graphemes(true) {
                if grapheme == "\n" {
                    lines.push(vec![]);
                    col = 0;
                    continue;
                }
                let w = UnicodeWidthStr::width(grapheme);
                if col + w > width {
                    break;
                }
                if let Some(line) = lines.last_mut() {
                    if let Some(last) = line.last_mut().filter(|(_, s)| *s == style) {
                        last.0.push_str(grapheme);
                    } else {
                        line.push((grapheme.to_string(), style));
                    }
                }
                col += w;
            }
        }
        lines
    }

    /// Wrap at any grapheme boundary.
    fn char_wrap(&self, width: usize) -> Vec<Vec<(String, Style)>> {
        let mut lines: Vec<Vec<(String, Style)>> = vec![vec![]];
        let mut col = 0usize;

        for span in &self.spans {
            let style = span.style;
            for grapheme in span.content.graphemes(true) {
                if grapheme == "\n" {
                    lines.push(vec![]);
                    col = 0;
                    continue;
                }
                let w = UnicodeWidthStr::width(grapheme);
                if col + w > width {
                    lines.push(vec![]);
                    col = 0;
                }
                if let Some(line) = lines.last_mut() {
                    if let Some(last) = line.last_mut().filter(|(_, s)| *s == style) {
                        last.0.push_str(grapheme);
                    } else {
                        line.push((grapheme.to_string(), style));
                    }
                }
                col += w;
            }
        }
        lines
    }

    /// Wrap at word boundaries (spaces).
    fn word_wrap(&self, width: usize) -> Vec<Vec<(String, Style)>> {
        // Build flat word list: (word_str, style).
        // Words are grapheme sequences between whitespace runs.
        struct Word {
            text: String,
            style: Style,
            is_space: bool,
        }
        let mut words: Vec<Word> = vec![];

        for span in &self.spans {
            let style = span.style;
            // Handle explicit newlines first.
            for part in span.content.split('\n') {
                if !words.is_empty() {
                    // Record the newline as a sentinel.
                    words.push(Word {
                        text: "\n".to_string(),
                        style,
                        is_space: true,
                    });
                }
                // Split part into words and spaces.
                let mut current = String::new();
                let mut in_space = false;
                for g in part.graphemes(true) {
                    let is_ws = g.chars().all(|c| c.is_whitespace());
                    if is_ws != in_space && !current.is_empty() {
                        words.push(Word {
                            text: current.clone(),
                            style,
                            is_space: in_space,
                        });
                        current.clear();
                    }
                    current.push_str(g);
                    in_space = is_ws;
                }
                if !current.is_empty() {
                    words.push(Word {
                        text: current,
                        style,
                        is_space: in_space,
                    });
                }
            }
        }

        // Greedily pack words onto lines.
        let mut lines: Vec<Vec<(String, Style)>> = vec![vec![]];
        let mut col = 0usize;

        for word in &words {
            if word.text == "\n" {
                lines.push(vec![]);
                col = 0;
                continue;
            }
            let w = grapheme_width(&word.text);
            if word.is_space {
                // Only add trailing spaces if they fit; never start a line with them.
                if col == 0 {
                    continue;
                }
                if col + w <= width {
                    append_segment(lines.last_mut().unwrap(), &word.text, word.style);
                    col += w;
                }
            } else {
                if col > 0 && col + w > width {
                    // Start a new line.
                    lines.push(vec![]);
                    col = 0;
                }
                // If a single word is wider than the line, force char-wrap it.
                if w > width {
                    let mut remaining = word.text.as_str();
                    loop {
                        let mut taken = 0usize;
                        let mut split = remaining;
                        for g in remaining.graphemes(true) {
                            let gw = UnicodeWidthStr::width(g);
                            if col + taken + gw > width {
                                split = &remaining[..taken];
                                break;
                            }
                            taken += g.len();
                        }
                        if split.is_empty() {
                            // Edge: even one grapheme wider than width.
                            let g = remaining.graphemes(true).next().unwrap_or("");
                            append_segment(lines.last_mut().unwrap(), g, word.style);
                            col += UnicodeWidthStr::width(g);
                            remaining = &remaining[g.len()..];
                        } else {
                            append_segment(lines.last_mut().unwrap(), split, word.style);
                            col += grapheme_width(split);
                            remaining = &remaining[split.len()..];
                        }
                        if remaining.is_empty() {
                            break;
                        }
                        lines.push(vec![]);
                        col = 0;
                    }
                } else {
                    append_segment(lines.last_mut().unwrap(), &word.text, word.style);
                    col += w;
                }
            }
        }

        lines
    }
}

fn grapheme_width(s: &str) -> usize {
    s.graphemes(true).map(UnicodeWidthStr::width).sum()
}

fn append_segment(line: &mut Vec<(String, Style)>, text: &str, style: Style) {
    if let Some(last) = line.last_mut().filter(|(_, s)| *s == style) {
        last.0.push_str(text);
    } else {
        line.push((text.to_string(), style));
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testing::render_to_lines;

    #[test]
    fn text_single_line_fits() {
        let lines = render_to_lines(Text::new("Hello").into_element(), 10, 1);
        assert_eq!(lines[0].trim_end(), "Hello");
    }

    #[test]
    fn text_word_wrap() {
        let lines = render_to_lines(Text::new("Hello world this").into_element(), 12, 3);
        // "Hello world " fits 12, "this" on next line
        assert_eq!(lines[0].trim_end(), "Hello world");
        assert_eq!(lines[1].trim_end(), "this");
    }

    #[test]
    fn text_char_wrap() {
        let lines = render_to_lines(Text::new("abcde").wrap(WrapMode::Char).into_element(), 3, 3);
        assert_eq!(lines[0].trim_end(), "abc");
        assert_eq!(lines[1].trim_end(), "de");
    }

    #[test]
    fn text_clip_no_wrap() {
        let lines = render_to_lines(
            Text::new("Hello world").wrap(WrapMode::Clip).into_element(),
            7,
            1,
        );
        assert_eq!(lines[0].trim_end(), "Hello w");
    }

    #[test]
    fn text_center_alignment() {
        let lines = render_to_lines(
            Text::new("Hi").alignment(Alignment::Center).into_element(),
            6,
            1,
        );
        // "Hi" is 2 wide, in 6-wide area → 2 spaces + "Hi" + 2 spaces
        assert_eq!(&lines[0], "  Hi  ");
    }

    #[test]
    fn text_right_alignment() {
        let lines = render_to_lines(
            Text::new("Hi").alignment(Alignment::Right).into_element(),
            6,
            1,
        );
        assert_eq!(&lines[0], "    Hi");
    }
}
