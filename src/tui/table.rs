//! Markdown table rendering with full-width and narrow fallback modes.

use crate::tui::terminal::{StyledLine, StyledSpan};
use pulldown_cmark::Alignment;
use unicode_width::UnicodeWidthStr;

/// Minimum column width before switching to narrow mode.
const MIN_COLUMN_WIDTH: usize = 12;

/// Parsed markdown table ready for rendering.
#[derive(Debug, Default)]
pub struct Table {
    pub headers: Vec<String>,
    pub alignments: Vec<Alignment>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render table to styled lines, choosing full or narrow mode based on width.
    pub fn render(&self, available_width: usize) -> Vec<StyledLine> {
        if self.headers.is_empty() && self.rows.is_empty() {
            return vec![];
        }

        let num_cols = self.num_columns();
        if num_cols == 0 {
            return vec![];
        }

        // Calculate if we can fit full-width mode
        // Need: columns + separators + padding
        // Minimum: num_cols * MIN_COLUMN_WIDTH + (num_cols + 1) separators + 2*num_cols padding
        let min_full_width = num_cols * MIN_COLUMN_WIDTH + (num_cols + 1) + (num_cols * 2);

        if available_width >= min_full_width {
            self.render_full(available_width)
        } else {
            self.render_narrow(available_width)
        }
    }

    fn num_columns(&self) -> usize {
        self.headers
            .len()
            .max(self.rows.iter().map(|r| r.len()).max().unwrap_or(0))
    }

    /// Full-width table with box drawing and text wrapping.
    fn render_full(&self, available_width: usize) -> Vec<StyledLine> {
        let num_cols = self.num_columns();
        let col_widths = self.calculate_column_widths(available_width, num_cols);

        let mut lines = Vec::new();

        // Top border
        lines.push(self.render_border(&col_widths, BorderPosition::Top));

        // Header row
        if !self.headers.is_empty() {
            let header_lines = self.render_row(&self.headers, &col_widths, true);
            lines.extend(header_lines);
            // Header separator
            lines.push(self.render_border(&col_widths, BorderPosition::Middle));
        }

        // Data rows
        for (i, row) in self.rows.iter().enumerate() {
            let row_lines = self.render_row(row, &col_widths, false);
            lines.extend(row_lines);

            // Row separator (except after last row)
            if i < self.rows.len() - 1 {
                lines.push(self.render_border(&col_widths, BorderPosition::RowSep));
            }
        }

        // Bottom border
        lines.push(self.render_border(&col_widths, BorderPosition::Bottom));

        lines
    }

    /// Calculate column widths based on content and available space.
    fn calculate_column_widths(&self, available_width: usize, num_cols: usize) -> Vec<usize> {
        if num_cols == 0 {
            return vec![];
        }

        // Measure natural width of each column (max content width)
        let mut natural_widths: Vec<usize> = vec![0; num_cols];

        for (i, header) in self.headers.iter().enumerate() {
            if i < num_cols {
                natural_widths[i] = natural_widths[i].max(measure_width(header));
            }
        }

        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < num_cols {
                    natural_widths[i] = natural_widths[i].max(measure_width(cell));
                }
            }
        }

        // Available width for content (subtract borders and padding)
        // Format: │ cell │ cell │ = (num_cols + 1) borders + 2*num_cols padding
        let overhead = (num_cols + 1) + (num_cols * 2);
        let content_width = available_width.saturating_sub(overhead);

        let total_natural: usize = natural_widths.iter().sum();

        if total_natural <= content_width {
            // Everything fits - use natural widths
            natural_widths
        } else {
            // Need to shrink - distribute proportionally with minimum
            distribute_widths(&natural_widths, content_width, MIN_COLUMN_WIDTH)
        }
    }

    /// Render a single row (may produce multiple lines if cells wrap).
    fn render_row(
        &self,
        cells: &[String],
        col_widths: &[usize],
        is_header: bool,
    ) -> Vec<StyledLine> {
        let num_cols = col_widths.len();

        // Wrap each cell and determine row height
        let wrapped_cells: Vec<Vec<String>> = (0..num_cols)
            .map(|i| {
                let content = cells.get(i).map(|s| s.as_str()).unwrap_or("");
                let width = col_widths[i];
                wrap_text(content, width)
            })
            .collect();

        let row_height = wrapped_cells.iter().map(|c| c.len()).max().unwrap_or(1);

        // Render each line of the row
        let mut lines = Vec::new();
        for line_idx in 0..row_height {
            let mut spans = Vec::new();
            spans.push(StyledSpan::dim("│".to_string()));

            for (col_idx, wrapped) in wrapped_cells.iter().enumerate() {
                let cell_line = wrapped.get(line_idx).map(|s| s.as_str()).unwrap_or("");
                let width = col_widths[col_idx];
                let alignment = self
                    .alignments
                    .get(col_idx)
                    .copied()
                    .unwrap_or(Alignment::None);

                let padded = pad_cell(cell_line, width, alignment);

                spans.push(StyledSpan::raw(" ".to_string()));
                if is_header {
                    spans.push(StyledSpan::bold(padded));
                } else {
                    spans.push(StyledSpan::raw(padded));
                }
                spans.push(StyledSpan::raw(" ".to_string()));
                spans.push(StyledSpan::dim("│".to_string()));
            }

            lines.push(StyledLine::new(spans));
        }

        lines
    }

    fn render_border(&self, col_widths: &[usize], position: BorderPosition) -> StyledLine {
        let (left, mid, right, fill) = match position {
            BorderPosition::Top => ("┌", "┬", "┐", "─"),
            BorderPosition::Middle => ("├", "┼", "┤", "─"),
            BorderPosition::RowSep => ("├", "┼", "┤", "─"),
            BorderPosition::Bottom => ("└", "┴", "┘", "─"),
        };

        let mut s = String::new();
        s.push_str(left);
        for (i, &width) in col_widths.iter().enumerate() {
            // +2 for padding on each side
            s.push_str(&fill.repeat(width + 2));
            if i < col_widths.len() - 1 {
                s.push_str(mid);
            }
        }
        s.push_str(right);

        StyledLine::dim(s)
    }

    /// Narrow fallback: "Header: Value" format with separators.
    fn render_narrow(&self, available_width: usize) -> Vec<StyledLine> {
        let mut lines = Vec::new();
        let separator = "─".repeat(available_width.min(40));

        for (row_idx, row) in self.rows.iter().enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let header = self.headers.get(col_idx).map(|s| s.as_str()).unwrap_or("?");

                // Wrap the value if needed
                let label_width = measure_width(header) + 2; // ": "
                let value_width = available_width.saturating_sub(label_width).max(10);
                let wrapped = wrap_text(cell, value_width);

                for (i, line) in wrapped.iter().enumerate() {
                    if i == 0 {
                        lines.push(StyledLine::new(vec![
                            StyledSpan::bold(header.to_string()),
                            StyledSpan::dim(": ".to_string()),
                            StyledSpan::raw(line.clone()),
                        ]));
                    } else {
                        // Continuation lines indented
                        let indent = " ".repeat(label_width);
                        lines.push(StyledLine::new(vec![
                            StyledSpan::raw(indent),
                            StyledSpan::raw(line.clone()),
                        ]));
                    }
                }
            }

            // Separator between rows (not after last)
            if row_idx < self.rows.len() - 1 {
                lines.push(StyledLine::dim(separator.clone()));
            }
        }

        lines
    }
}

#[derive(Debug, Clone, Copy)]
enum BorderPosition {
    Top,
    Middle,
    RowSep,
    Bottom,
}

/// Measure display width of a string (handles unicode).
fn measure_width(s: &str) -> usize {
    // Take the longest line if multiline
    s.lines().map(|line| line.width()).max().unwrap_or(0)
}

/// Wrap text to fit within given width.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();

    for paragraph in text.lines() {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current_line = String::new();
        let mut current_width = 0;

        for word in paragraph.split_whitespace() {
            let word_width = word.width();

            if current_width == 0 {
                // First word on line
                if word_width <= max_width {
                    current_line = word.to_string();
                    current_width = word_width;
                } else {
                    // Word too long - break it
                    let broken = break_word(word, max_width);
                    for (i, part) in broken.into_iter().enumerate() {
                        if i > 0 || !current_line.is_empty() {
                            lines.push(std::mem::take(&mut current_line));
                        }
                        current_line = part;
                        current_width = current_line.width();
                    }
                }
            } else if current_width + 1 + word_width <= max_width {
                // Word fits on current line
                current_line.push(' ');
                current_line.push_str(word);
                current_width += 1 + word_width;
            } else {
                // Start new line
                lines.push(std::mem::take(&mut current_line));
                if word_width <= max_width {
                    current_line = word.to_string();
                    current_width = word_width;
                } else {
                    // Word too long - break it
                    let broken = break_word(word, max_width);
                    for part in broken {
                        if !current_line.is_empty() {
                            lines.push(std::mem::take(&mut current_line));
                        }
                        current_line = part;
                        current_width = current_line.width();
                    }
                }
            }
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

/// Break a single word that's too long to fit.
fn break_word(word: &str, max_width: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;

    for ch in word.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if current_width + ch_width > max_width && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

/// Distribute widths proportionally while respecting minimum.
fn distribute_widths(natural: &[usize], available: usize, min_width: usize) -> Vec<usize> {
    let n = natural.len();
    if n == 0 {
        return vec![];
    }

    let total_natural: usize = natural.iter().sum();
    if total_natural == 0 {
        // Equal distribution
        let per_col = (available / n).max(min_width);
        return vec![per_col; n];
    }

    // Proportional distribution
    let mut widths: Vec<usize> = natural
        .iter()
        .map(|&w| {
            let proportion = w as f64 / total_natural as f64;
            let target = (proportion * available as f64).round() as usize;
            target.max(min_width)
        })
        .collect();

    // Adjust to fit exactly (take from largest columns if over)
    let total: usize = widths.iter().sum();
    if total > available {
        let mut excess = total - available;
        while excess > 0 {
            if let Some((idx, _)) = widths
                .iter()
                .enumerate()
                .filter(|&(_, w)| *w > min_width)
                .max_by_key(|&(_, w)| *w)
            {
                widths[idx] -= 1;
                excess -= 1;
            } else {
                break;
            }
        }
    }

    widths
}

/// Pad cell content to width with alignment.
fn pad_cell(content: &str, width: usize, alignment: Alignment) -> String {
    let content_width = content.width();
    if content_width >= width {
        return content.to_string();
    }

    let padding = width - content_width;
    match alignment {
        Alignment::Left | Alignment::None => {
            format!("{}{}", content, " ".repeat(padding))
        }
        Alignment::Right => {
            format!("{}{}", " ".repeat(padding), content)
        }
        Alignment::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{}{}", " ".repeat(left), content, " ".repeat(right))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_measure_width() {
        assert_eq!(measure_width("hello"), 5);
        assert_eq!(measure_width("héllo"), 5);
        assert_eq!(measure_width("你好"), 4); // 2 chars, 2 width each
    }

    #[test]
    fn test_wrap_text() {
        let wrapped = wrap_text("hello world foo bar", 10);
        assert_eq!(wrapped, vec!["hello", "world foo", "bar"]);
    }

    #[test]
    fn test_wrap_long_word() {
        let wrapped = wrap_text("superlongword", 5);
        assert_eq!(wrapped, vec!["super", "longw", "ord"]);
    }

    #[test]
    fn test_pad_cell() {
        assert_eq!(pad_cell("hi", 5, Alignment::Left), "hi   ");
        assert_eq!(pad_cell("hi", 5, Alignment::Right), "   hi");
        assert_eq!(pad_cell("hi", 5, Alignment::Center), " hi  ");
    }

    #[test]
    fn test_table_narrow() {
        let mut table = Table::new();
        table.headers = vec!["Name".to_string(), "Value".to_string()];
        table.rows = vec![vec!["foo".to_string(), "bar".to_string()]];

        let lines = table.render_narrow(30);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_table_full() {
        let mut table = Table::new();
        table.headers = vec!["Name".to_string(), "Value".to_string()];
        table.alignments = vec![Alignment::Left, Alignment::Right];
        table.rows = vec![
            vec!["foo".to_string(), "123".to_string()],
            vec!["bar".to_string(), "456".to_string()],
        ];

        let lines = table.render_full(60);
        assert!(!lines.is_empty());
        // Should have borders
        let first = &lines[0];
        assert!(first.spans.iter().any(|s| s.content.contains("┌")));
    }
}
