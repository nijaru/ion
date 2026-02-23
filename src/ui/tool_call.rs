//! ToolCallView — collapsible tool execution widget.
//!
//! Shows a compact header while the tool is running, then either stays
//! expanded (on error / explicit) or collapses to a one-line summary
//! when complete.

use tui::{
    geometry::Rect,
    widgets::{canvas::Canvas, Element, IntoElement},
};

use crate::tui::highlight::markdown::render_markdown_with_width;
use crate::tui::terminal::{Color as IonColor, StyledLine, StyledSpan, TextStyle};
use crate::ui::style::text_style_to_tui;

// ── State ─────────────────────────────────────────────────────────────────────

/// Tool execution state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    /// Tool is currently executing.
    Running,
    /// Tool completed successfully.
    Done,
    /// Tool completed with an error.
    Error,
}

/// Widget state for a single tool call — store in your app model.
///
/// ```no_run
/// # use ion::ui::ToolCallView;
/// let mut tc = ToolCallView::new("read", "src/main.rs");
/// // ... tool runs ...
/// tc.complete("contents of the file", false);
/// ```
pub struct ToolCallView {
    /// Tool name (e.g. "read", "bash").
    pub name: String,
    /// Short description or primary argument (e.g. file path, command).
    pub label: String,
    /// Tool execution state.
    pub state: ToolState,
    /// Output content (populated on completion).
    output: String,
    /// Whether the view is expanded to show full output.
    pub expanded: bool,
    /// Cached rendered lines: (width, lines).
    rendered: Option<(u16, Vec<StyledLine>)>,
}

impl ToolCallView {
    /// Create a new tool call in the Running state.
    pub fn new(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            state: ToolState::Running,
            output: String::new(),
            expanded: false,
            rendered: None,
        }
    }

    /// Mark the tool call as complete.
    ///
    /// `is_error`: if true, auto-expands to show the error output.
    pub fn complete(&mut self, output: impl Into<String>, is_error: bool) {
        self.output = output.into();
        self.state = if is_error {
            ToolState::Error
        } else {
            ToolState::Done
        };
        // Auto-expand errors so they're visible without user action.
        if is_error {
            self.expanded = true;
        }
        self.rendered = None;
    }

    /// Toggle between expanded and collapsed output view.
    pub fn toggle_expand(&mut self) {
        self.expanded = !self.expanded;
        self.rendered = None;
    }

    pub fn is_running(&self) -> bool {
        self.state == ToolState::Running
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

        // ── Header line ──────────────────────────────────────────────────────
        let (icon, icon_style) = match self.state {
            ToolState::Running => (
                "⠿ ",
                TextStyle {
                    foreground_color: Some(IonColor::DarkCyan),
                    ..Default::default()
                },
            ),
            ToolState::Done => (
                "✓ ",
                TextStyle {
                    foreground_color: Some(IonColor::DarkGreen),
                    ..Default::default()
                },
            ),
            ToolState::Error => (
                "✗ ",
                TextStyle {
                    foreground_color: Some(IonColor::DarkRed),
                    bold: true,
                    ..Default::default()
                },
            ),
        };

        let name_style = TextStyle {
            foreground_color: Some(IonColor::DarkGrey),
            bold: true,
            ..Default::default()
        };
        let label_style = TextStyle {
            foreground_color: Some(IonColor::DarkGrey),
            ..Default::default()
        };

        // Truncate label to fit on the header line
        let overhead = icon.len() + self.name.len() + 1; // "✓ name "
        let label_max = w.saturating_sub(overhead);
        let label = if self.label.len() > label_max && label_max > 3 {
            format!("{}…", &self.label[..label_max - 1])
        } else {
            self.label.clone()
        };

        result.push(StyledLine::new(vec![
            StyledSpan::new(icon, icon_style),
            StyledSpan::new(&self.name, name_style),
            StyledSpan::new(" ", TextStyle::new()),
            StyledSpan::new(label, label_style),
        ]));

        // ── Output (when expanded and complete) ──────────────────────────────
        if self.expanded && !self.output.is_empty() {
            let indent = "  ";
            let content_width = w.saturating_sub(indent.len()).max(1);
            // Trim output to avoid rendering enormous blobs (first 50 lines)
            let display_content = truncate_lines(&self.output, 50);
            let content_lines = render_markdown_with_width(&display_content, content_width);
            for line in content_lines {
                let mut spans = vec![StyledSpan::raw(indent)];
                spans.extend(line.spans);
                result.push(StyledLine::new(spans));
            }
            // Hint if truncated
            let raw_lines = self.output.lines().count();
            if raw_lines > 50 {
                result.push(StyledLine::new(vec![StyledSpan::dim(format!(
                    "  … {} more lines",
                    raw_lines - 50
                ))]));
            }
        }

        result
    }

    /// Build a renderable element.
    pub fn view(&mut self, width: u16) -> Element {
        self.lines_at_width(width); // warm cache
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

fn truncate_lines(s: &str, max_lines: usize) -> String {
    s.lines().take(max_lines).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_shows_single_header_line() {
        let mut tc = ToolCallView::new("read", "src/main.rs");
        assert_eq!(tc.row_count(40), 1);
    }

    #[test]
    fn complete_collapsed_stays_one_line() {
        let mut tc = ToolCallView::new("read", "src/main.rs");
        tc.complete("file contents here", false);
        assert!(!tc.expanded);
        assert_eq!(tc.row_count(40), 1);
    }

    #[test]
    fn error_auto_expands() {
        let mut tc = ToolCallView::new("bash", "rm -rf /");
        tc.complete("permission denied", true);
        assert!(tc.expanded);
        assert!(tc.row_count(40) > 1);
    }

    #[test]
    fn toggle_expand_shows_output() {
        let mut tc = ToolCallView::new("read", "src/main.rs");
        tc.complete("fn main() {}", false);
        let collapsed_rows = tc.row_count(40);
        tc.toggle_expand();
        let expanded_rows = tc.row_count(40);
        assert!(expanded_rows > collapsed_rows);
    }

    #[test]
    fn view_renders_without_panic() {
        let mut tc = ToolCallView::new("glob", "**/*.rs");
        tc.complete("src/main.rs\nsrc/lib.rs", false);
        tc.toggle_expand();
        let element = tc.view(40);
        let lines = tui::widgets::testing::render_to_lines(element, 40, 10);
        assert!(!lines.is_empty());
    }
}
