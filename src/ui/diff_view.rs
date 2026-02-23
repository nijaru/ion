//! DiffView — unified diff renderer widget.
//!
//! Renders a unified diff with color-coded additions (green), deletions
//! (red), hunk headers (cyan), and context lines (dim). Uses the
//! existing `highlight_diff_line` function from ion's highlight module.

use tui::{
    geometry::Rect,
    widgets::{canvas::Canvas, Element, IntoElement},
};

use crate::tui::highlight::highlight_diff_line;
use crate::tui::terminal::{StyledLine, StyledSpan, TextStyle};
use crate::ui::streaming::ion_color_to_tui;
use tui::style::{Style, StyleModifiers};

// ── State ─────────────────────────────────────────────────────────────────────

/// Widget state for a unified diff view.
///
/// ```no_run
/// # use ion::ui::DiffView;
/// let dv = DiffView::new("--- a/foo.rs\n+++ b/foo.rs\n@@ -1 +1 @@\n-old\n+new");
/// ```
pub struct DiffView {
    /// Raw unified diff text.
    diff: String,
    /// Max lines to show before truncating.
    max_lines: usize,
    /// Cached rendered lines: (width, lines).
    rendered: Option<(u16, Vec<StyledLine>)>,
}

impl DiffView {
    pub fn new(diff: impl Into<String>) -> Self {
        Self {
            diff: diff.into(),
            max_lines: 200,
            rendered: None,
        }
    }

    pub fn max_lines(mut self, n: usize) -> Self {
        self.max_lines = n;
        self
    }

    fn lines_at_width(&mut self, width: u16) -> &[StyledLine] {
        if self.rendered.as_ref().map_or(true, |(w, _)| *w != width) {
            let lines = self.render_lines(width);
            self.rendered = Some((width, lines));
        }
        &self.rendered.as_ref().unwrap().1
    }

    fn render_lines(&self, _width: u16) -> Vec<StyledLine> {
        let raw: Vec<&str> = self.diff.lines().collect();
        let display = &raw[..raw.len().min(self.max_lines)];

        let mut result: Vec<StyledLine> = display
            .iter()
            .map(|line| highlight_diff_line(line))
            .collect();

        if raw.len() > self.max_lines {
            let dim_style = TextStyle {
                dim: true,
                ..Default::default()
            };
            result.push(StyledLine::new(vec![StyledSpan::new(
                format!("… {} more lines", raw.len() - self.max_lines),
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
                    let style = text_style_to_tui_style(&span.style);
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

fn text_style_to_tui_style(s: &TextStyle) -> Style {
    let mut style = Style::new();
    if let Some(fg) = s.foreground_color {
        style = style.fg(ion_color_to_tui(fg));
    }
    if let Some(bg) = s.background_color {
        style = style.bg(ion_color_to_tui(bg));
    }
    if s.bold {
        style = style.bold();
    }
    if s.dim {
        style = style.dim();
    }
    if s.italic {
        style = style.italic();
    }
    if s.underlined {
        style = style.underline();
    }
    if s.crossed_out {
        style = style.strikethrough();
    }
    if s.reverse {
        style.modifiers |= StyleModifiers::REVERSED;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn foo() {
-    return 1;
+    return 2;
 }";

    #[test]
    fn renders_correct_line_count() {
        let mut dv = DiffView::new(SAMPLE_DIFF);
        assert_eq!(dv.row_count(80), 7);
    }

    #[test]
    fn truncation_adds_hint() {
        let long_diff: String = (0..10).map(|i| format!("+line {i}\n")).collect();
        let mut dv = DiffView::new(long_diff).max_lines(5);
        // 5 shown + 1 hint
        assert_eq!(dv.row_count(80), 6);
    }

    #[test]
    fn view_renders_without_panic() {
        let mut dv = DiffView::new(SAMPLE_DIFF);
        let element = dv.view(80);
        let lines = tui::widgets::testing::render_to_lines(element, 80, 20);
        assert!(!lines.is_empty());
    }

    #[test]
    fn addition_line_present() {
        let mut dv = DiffView::new(SAMPLE_DIFF);
        let element = dv.view(80);
        let lines = tui::widgets::testing::render_to_lines(element, 80, 20);
        assert!(lines.iter().any(|l| l.contains("+    return 2;")));
    }
}
