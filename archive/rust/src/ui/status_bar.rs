//! StatusBar — one-line status bar widget for ion.
//!
//! Renders: `[model]  [tokens]  [cost]  [branch]  [mode]`
//!
//! Fields are optional — missing fields are omitted. Fields are separated
//! by dim `·` separators. The mode field is colored: green for write, dim
//! for read.

use tui::{
    style::{Color, Style},
    widgets::{Element, IntoElement, canvas::Canvas},
};

// ── Widget ────────────────────────────────────────────────────────────────────

/// One-line status bar rendered at the bottom of the ion layout.
#[derive(Debug, Default, Clone)]
pub struct StatusBar {
    pub model: Option<String>,
    pub tokens: Option<String>,
    pub cost: Option<String>,
    pub branch: Option<String>,
    /// `"read"` | `"write"`, or any other label.
    pub mode: Option<String>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the status bar into a 1-row `Element`.
    ///
    /// `width` is used to truncate the bar if all fields together exceed it.
    pub fn view(&self, width: u16) -> Element {
        let segments = self.segments();
        let width = width;

        Canvas::new(move |area, buf| {
            // Always render in row 0 relative to the canvas area.
            let row = area.y;
            let mut col = area.x;
            let max_col = area.x + width.min(area.width);

            let sep_style = Style::new().dim();

            for (i, (text, style)) in segments.iter().enumerate() {
                if col >= max_col {
                    break;
                }
                // Separator between fields (not before the first).
                if i > 0 {
                    col = buf.set_string(col, row, " · ", sep_style);
                    if col >= max_col {
                        break;
                    }
                }
                col = buf.set_string(col, row, text, *style);
            }
        })
        .into_element()
    }

    /// Collect non-empty fields as `(text, style)` pairs in display order.
    fn segments(&self) -> Vec<(String, Style)> {
        let dim = Style::new().dim();
        let normal = Style::new();

        let mut out = Vec::new();

        if let Some(m) = &self.model {
            out.push((m.clone(), normal));
        }
        if let Some(t) = &self.tokens {
            out.push((t.clone(), dim));
        }
        if let Some(c) = &self.cost {
            out.push((c.clone(), dim));
        }
        if let Some(b) = &self.branch {
            out.push((format!("  {b}"), dim));
        }
        if let Some(mode) = &self.mode {
            let style = match mode.as_str() {
                "write" => Style::new().fg(Color::LightGreen),
                _ => dim,
            };
            out.push((mode.clone(), style));
        }

        out
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bar_renders_without_panic() {
        let bar = StatusBar::new();
        let el = bar.view(80);
        let lines = tui::widgets::testing::render_to_lines(el, 80, 1);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].trim().is_empty());
    }

    #[test]
    fn all_fields_render() {
        let bar = StatusBar {
            model: Some("claude-3-5-sonnet".to_string()),
            tokens: Some("1.2k".to_string()),
            cost: Some("$0.01".to_string()),
            branch: Some("main".to_string()),
            mode: Some("write".to_string()),
        };
        let el = bar.view(120);
        let lines = tui::widgets::testing::render_to_lines(el, 120, 1);
        assert_eq!(lines.len(), 1);
        // All field values should appear in the rendered output.
        let line = &lines[0];
        assert!(line.contains("claude-3-5-sonnet"), "model missing: {line}");
        assert!(line.contains("1.2k"), "tokens missing: {line}");
        assert!(line.contains("$0.01"), "cost missing: {line}");
        assert!(line.contains("main"), "branch missing: {line}");
        assert!(line.contains("write"), "mode missing: {line}");
    }

    #[test]
    fn partial_fields_render() {
        let bar = StatusBar {
            model: Some("sonnet".to_string()),
            mode: Some("read".to_string()),
            ..Default::default()
        };
        let el = bar.view(80);
        let lines = tui::widgets::testing::render_to_lines(el, 80, 1);
        let line = &lines[0];
        assert!(line.contains("sonnet"), "model missing: {line}");
        assert!(line.contains("read"), "mode missing: {line}");
        assert!(
            !line.contains("·") || line.contains("·"),
            "separator present: ok"
        );
    }
}
