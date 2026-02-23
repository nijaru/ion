use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{geometry::Rect, style::Style};

/// A single terminal cell.
///
/// For wide characters (CJK, some emoji), the main cell has `width = 2` and the
/// following cell has `skip = true` with `symbol = "\0"` as a sentinel. Sentinel
/// cells are skipped during diff rendering — the terminal advances the cursor
/// automatically when a wide char is printed.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// The grapheme cluster in this cell. Usually a single char.
    pub symbol: String,
    pub style: Style,
    /// Number of terminal columns this cell occupies. 1 = normal, 2 = wide.
    pub width: u8,
    /// True if this is the continuation (sentinel) cell of a wide char.
    pub skip: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            symbol: " ".to_string(),
            style: Style::default(),
            width: 1,
            skip: false,
        }
    }
}

impl Cell {
    pub fn reset(&mut self) {
        self.symbol = " ".to_string();
        self.style = Style::default();
        self.width = 1;
        self.skip = false;
    }

    pub fn set_symbol(&mut self, s: &str) -> &mut Self {
        self.symbol = s.to_string();
        self
    }

    pub fn set_style(&mut self, style: Style) -> &mut Self {
        self.style = style;
        self
    }
}

/// A 2D grid of cells — the output of a render pass.
///
/// Coordinates passed to all methods are **buffer-local** (0-indexed from the
/// top-left of `area`). `area.x` / `area.y` describe the terminal position
/// where this buffer will be rendered; they are added to coordinates when
/// producing [`DrawCommand::MoveTo`] in [`Buffer::diff`].
pub struct Buffer {
    pub area: Rect,
    cells: Vec<Cell>,
}

impl Buffer {
    /// Create a buffer for the given terminal region, filled with default cells.
    pub fn new(area: Rect) -> Self {
        let n = area.width as usize * area.height as usize;
        Self {
            area,
            cells: vec![Cell::default(); n],
        }
    }

    pub fn empty(area: Rect) -> Self {
        Self::new(area)
    }

    pub fn filled(area: Rect, cell: Cell) -> Self {
        let n = area.width as usize * area.height as usize;
        Self {
            area,
            cells: vec![cell; n],
        }
    }

    /// Map local (x, y) to a cell index.
    /// Panics in debug if out of bounds; clamps in release.
    pub fn index(&self, x: u16, y: u16) -> usize {
        debug_assert!(
            x < self.area.width,
            "x={x} out of bounds (width={})",
            self.area.width
        );
        debug_assert!(
            y < self.area.height,
            "y={y} out of bounds (height={})",
            self.area.height
        );
        let x = x.min(self.area.width.saturating_sub(1)) as usize;
        let y = y.min(self.area.height.saturating_sub(1)) as usize;
        y * self.area.width as usize + x
    }

    pub fn get(&self, x: u16, y: u16) -> &Cell {
        let idx = self.index(x, y);
        &self.cells[idx]
    }

    pub fn get_mut(&mut self, x: u16, y: u16) -> &mut Cell {
        let idx = self.index(x, y);
        &mut self.cells[idx]
    }

    /// Write a styled string starting at local (x, y), respecting grapheme width.
    ///
    /// Clips at the right edge of the buffer. Wide chars (width=2) write a
    /// sentinel cell. Returns the local x column after the last written grapheme.
    pub fn set_string(&mut self, x: u16, y: u16, s: &str, style: Style) -> u16 {
        let mut col = x;
        let max = self.area.width;

        for grapheme in s.graphemes(true) {
            if col >= max {
                break;
            }
            let w = UnicodeWidthStr::width(grapheme) as u16;
            if w == 0 {
                continue;
            }
            if col + w > max {
                // Wide char would overflow — replace with a space.
                let idx = self.index(col, y);
                self.cells[idx].symbol = " ".to_string();
                self.cells[idx].style = style;
                self.cells[idx].width = 1;
                self.cells[idx].skip = false;
                col += 1;
                break;
            }

            let idx = self.index(col, y);
            self.cells[idx].symbol = grapheme.to_string();
            self.cells[idx].style = style;
            self.cells[idx].width = w as u8;
            self.cells[idx].skip = false;
            col += 1;

            if w == 2 && col < max {
                // Sentinel for the second column of the wide char.
                let idx2 = self.index(col, y);
                self.cells[idx2].symbol = "\0".to_string();
                self.cells[idx2].style = style;
                self.cells[idx2].width = 0;
                self.cells[idx2].skip = true;
                col += 1;
            }
        }
        col
    }

    /// Write a single styled grapheme cluster at local (x, y).
    pub fn set_symbol(&mut self, x: u16, y: u16, symbol: &str, style: Style) {
        let idx = self.index(x, y);
        self.cells[idx].symbol = symbol.to_string();
        self.cells[idx].style = style;
    }

    /// Write a string truncated to `max_width` grapheme columns.
    pub fn set_string_truncated(
        &mut self,
        x: u16,
        y: u16,
        s: &str,
        max_width: u16,
        style: Style,
    ) -> u16 {
        let limit = (x + max_width).min(self.area.width);
        let mut col = x;

        for grapheme in s.graphemes(true) {
            if col >= limit {
                break;
            }
            let w = UnicodeWidthStr::width(grapheme) as u16;
            if w == 0 {
                continue;
            }
            if col + w > limit {
                break;
            }
            let idx = self.index(col, y);
            self.cells[idx].symbol = grapheme.to_string();
            self.cells[idx].style = style;
            self.cells[idx].width = w as u8;
            self.cells[idx].skip = false;
            col += 1;

            if w == 2 && col < limit {
                let idx2 = self.index(col, y);
                self.cells[idx2].symbol = "\0".to_string();
                self.cells[idx2].style = style;
                self.cells[idx2].width = 0;
                self.cells[idx2].skip = true;
                col += 1;
            }
        }
        col
    }

    /// Fill a rectangular region (local coords) with a cell value.
    pub fn fill_region(&mut self, area: Rect, cell: &Cell) {
        let buf_area = Rect::new(0, 0, self.area.width, self.area.height);
        let region = match buf_area.intersection(area) {
            Some(r) => r,
            None => return,
        };
        for row in region.y..region.y + region.height {
            for col in region.x..region.x + region.width {
                let idx = self.index(col, row);
                self.cells[idx] = cell.clone();
            }
        }
    }

    /// Merge another buffer into self, overwriting overlapping cells.
    /// Both buffers use local (0-based) coordinates; the merge aligns their
    /// top-left corners.
    pub fn merge(&mut self, other: &Buffer) {
        let self_area = Rect::new(0, 0, self.area.width, self.area.height);
        let other_area = Rect::new(0, 0, other.area.width, other.area.height);
        let overlap = match self_area.intersection(other_area) {
            Some(r) => r,
            None => return,
        };
        for row in overlap.y..overlap.y + overlap.height {
            for col in overlap.x..overlap.x + overlap.width {
                let sidx = self.index(col, row);
                let oidx = other.index(col, row);
                self.cells[sidx] = other.cells[oidx].clone();
            }
        }
    }

    /// Reset all cells to default.
    pub fn reset(&mut self) {
        for cell in &mut self.cells {
            cell.reset();
        }
    }

    /// Produce the minimal sequence of draw commands to transform `prev` into
    /// `self`. Coalesces adjacent same-style runs; skips wide-char sentinel cells.
    ///
    /// Cells beyond `prev`'s dimensions are compared against a default cell.
    pub(crate) fn diff(&self, prev: &Buffer) -> Vec<DrawCommand> {
        let mut commands = Vec::new();
        let default_cell = Cell::default();

        let mut last_style: Option<Style> = None;
        // Where the terminal cursor will be after the last Print command.
        let mut last_pos: Option<(u16, u16)> = None;

        for row in 0..self.area.height {
            for col in 0..self.area.width {
                let new_cell = self.get(col, row);
                let old_cell = if col < prev.area.width && row < prev.area.height {
                    prev.get(col, row)
                } else {
                    &default_cell
                };

                if new_cell == old_cell || new_cell.skip {
                    continue;
                }

                let abs_x = self.area.x + col;
                let abs_y = self.area.y + row;

                let needs_move = last_pos
                    .map(|(lx, ly)| lx != abs_x || ly != abs_y)
                    .unwrap_or(true);

                if needs_move {
                    commands.push(DrawCommand::MoveTo(abs_x, abs_y));
                }

                if Some(new_cell.style) != last_style {
                    if new_cell.style == Style::default() {
                        commands.push(DrawCommand::ResetStyle);
                    } else {
                        commands.push(DrawCommand::SetStyle(new_cell.style));
                    }
                    last_style = Some(new_cell.style);
                }

                commands.push(DrawCommand::Print(new_cell.symbol.clone()));
                last_pos = Some((abs_x + new_cell.width as u16, abs_y));
            }
        }

        // Leave terminal in clean state.
        if last_style.is_some_and(|s| s != Style::default()) {
            commands.push(DrawCommand::ResetStyle);
        }

        commands
    }

    /// Convert to plain strings, one per row (for snapshot tests).
    /// Wide-char sentinel cells are skipped (their column is covered by the
    /// previous grapheme's visual width).
    pub fn to_lines(&self) -> Vec<String> {
        (0..self.area.height)
            .map(|row| {
                (0..self.area.width)
                    .filter_map(|col| {
                        let cell = self.get(col, row);
                        if cell.skip {
                            None
                        } else {
                            Some(cell.symbol.as_str())
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

/// A minimal draw instruction produced by [`Buffer::diff`].
/// Consumed by [`crate::terminal::Terminal::flush_commands`].
#[derive(Debug)]
pub(crate) enum DrawCommand {
    MoveTo(u16, u16),
    SetStyle(Style),
    Print(String),
    ResetStyle,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Color;

    fn buf(w: u16, h: u16) -> Buffer {
        Buffer::new(Rect::new(0, 0, w, h))
    }

    // ── to_lines / basic writes ─────────────────────────────────────────────

    #[test]
    fn new_buffer_is_spaces() {
        let lines = buf(5, 2).to_lines();
        assert_eq!(lines, vec!["     ", "     "]);
    }

    #[test]
    fn set_string_basic() {
        let mut b = buf(10, 1);
        b.set_string(0, 0, "hello", Style::default());
        assert_eq!(b.to_lines()[0], "hello     ");
    }

    #[test]
    fn set_string_clips_at_edge() {
        let mut b = buf(5, 1);
        b.set_string(0, 0, "hello world", Style::default());
        assert_eq!(b.to_lines()[0], "hello");
    }

    #[test]
    fn set_string_offset() {
        let mut b = buf(10, 1);
        b.set_string(3, 0, "hi", Style::default());
        assert_eq!(b.to_lines()[0], "   hi     ");
    }

    #[test]
    fn set_string_returns_next_col() {
        let mut b = buf(10, 1);
        let next = b.set_string(2, 0, "abc", Style::default());
        assert_eq!(next, 5);
    }

    #[test]
    fn set_string_truncated_respects_max() {
        let mut b = buf(10, 1);
        b.set_string_truncated(0, 0, "hello world", 5, Style::default());
        assert_eq!(b.to_lines()[0], "hello     ");
    }

    // ── wide chars ──────────────────────────────────────────────────────────

    #[test]
    fn wide_char_sentinel_skipped_in_to_lines() {
        let mut b = buf(4, 1);
        // '大' is 2 columns wide
        b.set_string(0, 0, "大a", Style::default());
        let lines = b.to_lines();
        // col 0: '大', col 1: sentinel (skipped), col 2: 'a', col 3: ' '
        assert_eq!(lines[0], "大a ");
    }

    #[test]
    fn wide_char_at_edge_replaced_with_space() {
        let mut b = buf(3, 1);
        // '大' at col 2 would overflow (needs cols 2+3 but width=3)
        b.set_string(2, 0, "大", Style::default());
        assert_eq!(b.to_lines()[0], "   ");
    }

    // ── fill / reset ────────────────────────────────────────────────────────

    #[test]
    fn fill_region() {
        let mut b = buf(5, 3);
        let cell = Cell {
            symbol: "X".to_string(),
            style: Style::default(),
            width: 1,
            skip: false,
        };
        b.fill_region(Rect::new(1, 1, 3, 1), &cell);
        let lines = b.to_lines();
        assert_eq!(lines[0], "     ");
        assert_eq!(lines[1], " XXX ");
        assert_eq!(lines[2], "     ");
    }

    #[test]
    fn reset_fills_with_spaces() {
        let mut b = buf(4, 1);
        b.set_string(0, 0, "abcd", Style::default());
        b.reset();
        assert_eq!(b.to_lines()[0], "    ");
    }

    // ── diff ────────────────────────────────────────────────────────────────

    #[test]
    fn diff_identical_buffers_is_empty() {
        let b = buf(5, 1);
        let cmds = b.diff(&b);
        // Only possible command is a trailing ResetStyle, which won't appear
        // because default style is never set.
        assert!(cmds.is_empty());
    }

    #[test]
    fn diff_detects_single_change() {
        let prev = buf(5, 1);
        let mut curr = buf(5, 1);
        curr.set_string(0, 0, "X", Style::default());
        let cmds = curr.diff(&prev);
        // Expect MoveTo(0,0), Print("X") — default style needs no SetStyle/Reset
        let prints: Vec<_> = cmds
            .iter()
            .filter_map(|c| {
                if let DrawCommand::Print(s) = c {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(prints, vec!["X"]);
    }

    #[test]
    fn diff_styled_cell_emits_style_then_reset() {
        let prev = buf(5, 1);
        let mut curr = buf(5, 1);
        curr.set_string(0, 0, "R", Style::new().fg(Color::Red));
        let cmds = curr.diff(&prev);
        let has_set_style = cmds.iter().any(|c| matches!(c, DrawCommand::SetStyle(_)));
        let has_reset = cmds.iter().any(|c| matches!(c, DrawCommand::ResetStyle));
        assert!(has_set_style, "expected SetStyle command");
        assert!(has_reset, "expected trailing ResetStyle");
    }

    #[test]
    fn diff_against_empty_prev_rewrites_all_non_default() {
        let empty = Buffer::new(Rect::new(0, 0, 0, 0));
        let mut curr = buf(3, 1);
        curr.set_string(0, 0, "abc", Style::default());
        // All cells differ from the empty prev
        let cmds = curr.diff(&empty);
        let prints: Vec<_> = cmds
            .iter()
            .filter_map(|c| {
                if let DrawCommand::Print(s) = c {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(prints, vec!["a", "b", "c"]);
    }

    #[test]
    fn diff_coalesces_style_run() {
        let prev = buf(3, 1);
        let mut curr = buf(3, 1);
        let red = Style::new().fg(Color::Red);
        curr.set_string(0, 0, "abc", red);
        let cmds = curr.diff(&prev);
        // Only one SetStyle for the whole "abc" run
        let style_count = cmds
            .iter()
            .filter(|c| matches!(c, DrawCommand::SetStyle(_)))
            .count();
        assert_eq!(style_count, 1);
    }

    // ── merge ───────────────────────────────────────────────────────────────

    #[test]
    fn merge_overwrites_overlapping_cells() {
        let mut base = buf(5, 1);
        base.set_string(0, 0, "hello", Style::default());

        let mut overlay = buf(3, 1);
        overlay.set_string(0, 0, "ABC", Style::default());

        base.merge(&overlay);
        assert_eq!(base.to_lines()[0], "ABClo");
    }
}
