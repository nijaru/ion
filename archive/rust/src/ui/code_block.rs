//! CodeBlock — syntax-highlighted code widget.
//!
//! Renders a fenced code block with an optional language label and
//! syntect highlighting. Used when ion receives tool results or
//! assistant messages that contain standalone code blocks.

use tui::{
    geometry::Rect,
    widgets::{Element, IntoElement, canvas::Canvas},
};

use crate::tui::highlight::syntax::{highlight_code, syntax_from_fence};
use crate::tui::terminal::{Color as IonColor, StyledLine, StyledSpan, TextStyle};
use crate::ui::style::text_style_to_tui;

// ── State ─────────────────────────────────────────────────────────────────────

/// Widget state for a syntax-highlighted code block.
///
/// ```no_run
/// # use ion::ui::CodeBlock;
/// let cb = CodeBlock::new("fn main() {}", Some("rust"));
/// ```
pub struct CodeBlock {
    /// Raw code content.
    code: String,
    /// Language name as it appears in a markdown fence (e.g. "rust", "python").
    /// None → no syntax highlighting, no label.
    lang: Option<String>,
    /// Max lines to render before truncating.
    max_lines: usize,
    /// Cached rendered lines: (width, lines).
    rendered: Option<(u16, Vec<StyledLine>)>,
}

impl CodeBlock {
    /// Create a new code block.
    ///
    /// `lang` is the markdown fence identifier (e.g. `"rust"`, `"python"`).
    /// Pass `None` for plain code with no highlighting.
    pub fn new(code: impl Into<String>, lang: Option<&str>) -> Self {
        Self {
            code: code.into(),
            lang: lang.map(|s| s.to_owned()),
            max_lines: 80,
            rendered: None,
        }
    }

    /// Set the maximum number of code lines to display before truncating.
    pub fn max_lines(mut self, n: usize) -> Self {
        self.max_lines = n;
        self
    }

    /// Render (or return cached) lines for the given width.
    fn lines_at_width(&mut self, width: u16) -> &[StyledLine] {
        if self.rendered.as_ref().map_or(true, |(w, _)| *w != width) {
            let lines = self.render_lines(width);
            self.rendered = Some((width, lines));
        }
        &self.rendered.as_ref().unwrap().1
    }

    fn render_lines(&self, width: u16) -> Vec<StyledLine> {
        let w = width as usize;
        let mut result = Vec::new();

        // ── Language label ────────────────────────────────────────────────────
        if let Some(lang) = &self.lang {
            let label_style = TextStyle {
                foreground_color: Some(IonColor::DarkGrey),
                italic: true,
                ..Default::default()
            };
            result.push(StyledLine::new(vec![StyledSpan::new(
                lang.as_str(),
                label_style,
            )]));
        }

        // ── Code lines ────────────────────────────────────────────────────────
        let indent = "  ";
        let content_width = w.saturating_sub(indent.len()).max(1);
        let _ = content_width; // syntax highlighter doesn't wrap; width used for clipping only

        let raw_lines: Vec<&str> = self.code.lines().collect();
        let display_lines = &raw_lines[..raw_lines.len().min(self.max_lines)];

        let highlighted: Vec<StyledLine> =
            if let Some(lang_str) = self.lang.as_deref().and_then(syntax_from_fence) {
                highlight_code(&display_lines.join("\n"), lang_str)
            } else {
                display_lines
                    .iter()
                    .map(|l| StyledLine::raw(l.to_string()))
                    .collect()
            };

        for line in highlighted {
            let mut spans = vec![StyledSpan::raw(indent)];
            spans.extend(line.spans);
            result.push(StyledLine::new(spans));
        }

        // Truncation hint
        if raw_lines.len() > self.max_lines {
            let dim_style = TextStyle {
                dim: true,
                ..Default::default()
            };
            result.push(StyledLine::new(vec![StyledSpan::new(
                format!("  … {} more lines", raw_lines.len() - self.max_lines),
                dim_style,
            )]));
        }

        result
    }

    /// Build a renderable element.
    pub fn view(&mut self, width: u16) -> Element {
        self.lines_at_width(width);
        let flat: Vec<StyledLine> = self.rendered.as_ref().unwrap().1.iter().cloned().collect();

        Canvas::new(move |area: Rect, buf: &mut tui::buffer::Buffer| {
            for (row_offset, line) in flat.iter().take(area.height as usize).enumerate() {
                let row = area.y + row_offset as u16;
                let mut col = area.x;
                let max_col = area.x + area.width;
                for span in &line.spans {
                    if col >= max_col {
                        break;
                    }
                    let style = text_style_to_tui(&span.style);
                    col = buf.set_string(col, row, &span.content, style);
                }
            }
        })
        .into_element()
    }

    /// Number of rendered rows at the given width.
    pub fn row_count(&mut self, width: u16) -> u16 {
        self.lines_at_width(width).len() as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lang_renders_plain() {
        let mut cb = CodeBlock::new("hello world", None);
        // No language label: 1 line of code
        assert_eq!(cb.row_count(40), 1);
    }

    #[test]
    fn lang_label_adds_one_row() {
        let mut cb = CodeBlock::new("fn main() {}", Some("rust"));
        // label + 1 code line = 2
        assert_eq!(cb.row_count(40), 2);
    }

    #[test]
    fn truncation_hint_appears() {
        let code: String = (0..10).map(|i| format!("line {i}\n")).collect();
        let mut cb = CodeBlock::new(code, None).max_lines(5);
        // 5 lines + truncation hint = 6
        assert_eq!(cb.row_count(40), 6);
    }

    #[test]
    fn view_renders_without_panic() {
        let mut cb = CodeBlock::new("fn main() {\n    println!(\"hi\");\n}", Some("rust"));
        let element = cb.view(60);
        let lines = tui::widgets::testing::render_to_lines(element, 60, 10);
        assert!(!lines.is_empty());
        // First line is the language label
        assert!(lines[0].contains("rust"));
    }

    #[test]
    fn unknown_lang_falls_back_to_plain() {
        let mut cb = CodeBlock::new("some code", Some("brainfuck"));
        let element = cb.view(40);
        let lines = tui::widgets::testing::render_to_lines(element, 40, 5);
        assert!(!lines.is_empty());
    }
}
