//! Width-aware assistant tables, sharing ordinary inline styles and safe wrapping.
use pulldown_cmark::{Alignment, Event, Tag, TagEnd};
use ratatui::{style::Modifier, text::Line};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{View, push_styled};

struct Cell {
    lines: Vec<Line<'static>>,
}

impl Cell {
    fn measure(&self) -> (usize, usize) {
        let natural = self.lines.iter().map(Line::width).max().unwrap_or(0).max(1);
        let widest = self
            .lines
            .iter()
            .map(|line| {
                line.to_string()
                    .graphemes(true)
                    .map(UnicodeWidthStr::width)
                    .max()
                    .unwrap_or(1)
            })
            .max()
            .unwrap_or(1);
        (natural, natural.min(3).max(widest))
    }

    fn wrap(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for line in &self.lines {
            push_styled(&mut lines, "", "", line.clone(), width);
        }
        lines
    }
}

impl View<'_> {
    pub(super) fn table<'e>(
        &mut self,
        events: &mut impl Iterator<Item = Event<'e>>,
        alignments: Vec<Alignment>,
    ) {
        self.flush();
        let mut rows = Vec::new();
        let mut row = Vec::new();
        let mut header = false;
        while let Some(event) = events.next() {
            match event {
                Event::Start(Tag::TableHead) => header = true,
                Event::Start(Tag::TableCell) => {
                    let mut lines = Vec::new();
                    let base = if header {
                        self.style().add_modifier(Modifier::BOLD)
                    } else {
                        self.style()
                    };
                    let mut cell = View::new(&mut lines, usize::MAX, base);
                    for event in events.by_ref() {
                        if event == Event::End(TagEnd::TableCell) {
                            break;
                        }
                        cell.event(event);
                    }
                    cell.flush();
                    row.push(Cell { lines });
                }
                Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                    rows.push(std::mem::take(&mut row));
                    header = false;
                }
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        let count = alignments.len();
        if count == 0 || rows.is_empty() {
            return;
        }
        // Match ordinary wrapping's handling of prefixes too wide for the viewport.
        let indent = self
            .prefixes
            .iter()
            .map(|p| UnicodeWidthStr::width(p.continuation.as_str()))
            .sum::<usize>();
        let width = self.width.max(1);
        let available = if indent < width {
            width - indent
        } else {
            width
        };
        let mut natural = vec![1; count];
        let mut minimum = vec![1; count];
        for row in &rows {
            for (index, cell) in row.iter().enumerate() {
                let (size, floor) = cell.measure();
                natural[index] = natural[index].max(size);
                minimum[index] = minimum[index].max(floor);
            }
        }
        let gutter = 3 * (count - 1);
        if available < gutter + minimum.iter().sum::<usize>() {
            self.stacked(&rows);
        } else {
            let widths = column_widths(&natural, &minimum, available - gutter);
            for (index, row) in rows.iter().enumerate() {
                let cells = row
                    .iter()
                    .zip(&widths)
                    .map(|(cell, width)| cell.wrap(*width))
                    .collect::<Vec<_>>();
                let height = cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
                for line in 0..height {
                    for (column, (cell, width)) in cells.iter().zip(&widths).enumerate() {
                        if column > 0 {
                            self.text(" │ ".into());
                        }
                        let value = cell.get(line);
                        let padding = width - value.map_or(0, Line::width);
                        let before = match alignments[column] {
                            Alignment::Right => padding,
                            Alignment::Center => padding / 2,
                            _ => 0,
                        };
                        self.text(" ".repeat(before));
                        if let Some(value) = value {
                            self.spans.extend(value.spans.iter().cloned());
                        }
                        self.text(" ".repeat(padding - before));
                    }
                    self.flush();
                }
                if index == 0 {
                    self.text(
                        widths
                            .iter()
                            .map(|width| "─".repeat(*width))
                            .collect::<Vec<_>>()
                            .join("─┼─"),
                    );
                    self.flush();
                }
            }
        }
        self.separate();
    }

    fn stacked(&mut self, rows: &[Vec<Cell>]) {
        let header = &rows[0];
        if rows.len() == 1 {
            for cell in header {
                for line in &cell.lines {
                    self.spans = line.spans.clone();
                    self.flush();
                }
            }
            return;
        }
        for (index, row) in rows[1..].iter().enumerate() {
            if index > 0 {
                self.text(String::new());
                self.flush();
            }
            for (column, cell) in row.iter().enumerate() {
                if header[column].lines.is_empty() {
                    self.text(format!("Column {}", column + 1));
                } else {
                    for (index, line) in header[column].lines.iter().enumerate() {
                        if index > 0 {
                            self.text(" ".into());
                        }
                        self.spans.extend(line.spans.iter().cloned());
                    }
                }
                self.text(": ".into());
                for (index, line) in cell.lines.iter().enumerate() {
                    if index > 0 {
                        self.flush();
                    }
                    self.spans.extend(line.spans.iter().cloned());
                }
                self.flush();
            }
        }
    }
}

fn column_widths(natural: &[usize], minimum: &[usize], budget: usize) -> Vec<usize> {
    // Water-fill in logarithmic steps, rather than shaving huge cells one column
    // at a time. Small columns keep their natural width; large columns share room.
    let mut low = 1;
    let mut high = *natural.iter().max().expect("nonempty table columns");
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if natural
            .iter()
            .zip(minimum)
            .map(|(width, floor)| (*width).min(mid).max(*floor))
            .sum::<usize>()
            <= budget
        {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let mut widths = natural
        .iter()
        .zip(minimum)
        .map(|(width, floor)| (*width).min(low).max(*floor))
        .collect::<Vec<_>>();
    let mut remaining = budget - widths.iter().sum::<usize>();
    for (width, natural) in widths.iter_mut().zip(natural) {
        if remaining > 0 && *width < *natural {
            *width += 1;
            remaining -= 1;
        }
    }
    widths
}
