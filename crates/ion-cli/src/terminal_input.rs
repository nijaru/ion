//! Literal editor bytes projected through the shared control-safe text layout.
use crate::display_text::{Wrapping, layout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VisualPosition {
    pub row: usize,
    pub col: usize,
}

pub(super) struct InputWindow {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

struct CursorPoint {
    byte: usize,
    position: VisualPosition,
}

/// Disposable, width-dependent layout. Source remains authoritative for every
/// edit, copy and submission. Rebuild after a source or width change.
///
/// A UTF-8 cursor inside a joined grapheme projects to that grapheme's start;
/// reverse mapping and vertical movement only return whole-grapheme boundaries.
pub(super) struct EditorVisualMap<'a> {
    source: &'a str,
    lines: Vec<String>,
    points: Vec<CursorPoint>,
    margin: usize,
}

impl<'a> EditorVisualMap<'a> {
    /// `width` includes the prompt rail, just like `input_window`.
    pub fn new(source: &'a str, width: usize) -> Self {
        let width = width.max(1);
        let (prompt, indent, margin) = match width {
            1 | 2 => ("", "", 0),
            3 => ("›", " ", 1),
            _ => ("› ", "  ", 2),
        };
        let mut map = Self {
            source,
            lines: Vec::new(),
            points: Vec::new(),
            margin,
        };
        for row in layout(source, width - margin, Wrapping::Words) {
            let index = map.lines.len();
            let mut line = if index == 0 { prompt } else { indent }.to_owned();
            let mut col = margin;
            for unit in row.units {
                if map
                    .points
                    .last()
                    .is_none_or(|point| point.byte != unit.source.start)
                {
                    map.points.push(CursorPoint {
                        byte: unit.source.start,
                        position: VisualPosition { row: index, col },
                    });
                }
                line.push_str(unit.text);
                col += unit.cells;
            }
            map.lines.push(line);
            if !row.soft_wrap {
                // A hardware caret cannot occupy the cell past the right edge.
                // Give a full hard line/end its own insertion row, so the byte
                // before a newline remains distinct from the byte after it.
                let position = if col == width {
                    map.lines.push(indent.to_owned());
                    VisualPosition {
                        row: index + 1,
                        col: margin,
                    }
                } else {
                    VisualPosition { row: index, col }
                };
                map.points.push(CursorPoint {
                    byte: row.end,
                    position,
                });
            }
        }
        map
    }

    pub fn row_count(&self) -> usize {
        self.lines.len()
    }

    pub fn caret(&self, cursor: usize) -> VisualPosition {
        assert!(cursor <= self.source.len() && self.source.is_char_boundary(cursor));
        let index = self.points.partition_point(|point| point.byte <= cursor);
        self.points[index - 1].position
    }

    /// Resolve a cell to the last legal insertion point not past that column.
    /// Tab interiors and wide/combined glyph interiors are not byte positions.
    pub fn byte_at(&self, position: VisualPosition) -> Option<usize> {
        let mut candidates = self
            .points
            .iter()
            .filter(|point| point.position.row == position.row);
        let first = candidates.next()?;
        Some(
            candidates
                .take_while(|point| point.position.col <= position.col)
                .last()
                .unwrap_or(first)
                .byte,
        )
    }

    /// `None` means the actual visual boundary: the caller may enter history.
    /// Reset `preferred_column` on nonvertical edits/moves and source replacement.
    /// Columns are content-relative, not UTF-8 bytes or scalar counts.
    pub fn movement(
        &self,
        cursor: usize,
        down: bool,
        preferred_column: &mut Option<usize>,
    ) -> Option<usize> {
        let current = self.caret(cursor);
        let preferred = *preferred_column.get_or_insert(current.col - self.margin);
        let mut row = current.row;
        loop {
            row = if down {
                row.checked_add(1)?
            } else {
                row.checked_sub(1)?
            };
            if row >= self.row_count() {
                return None;
            }
            if let Some(byte) = self.byte_at(VisualPosition {
                row,
                col: preferred + self.margin,
            }) {
                return Some(byte);
            }
            // A narrow row may contain only the middle of an expanded tab.
            // Skip it rather than manufacture an offset inside that raw byte.
        }
    }

    pub fn window(&self, cursor: usize, max_rows: usize) -> InputWindow {
        let position = self.caret(cursor);
        let count = max_rows.max(1).min(self.row_count());
        let start = (position.row + 1).saturating_sub(count);
        InputWindow {
            lines: self.lines[start..start + count].to_vec(),
            cursor_row: position.row - start,
            cursor_col: position.col,
        }
    }
}

pub(super) fn input_window(
    draft: &str,
    cursor: usize,
    width: usize,
    max_rows: usize,
) -> InputWindow {
    EditorVisualMap::new(draft, width).window(cursor, max_rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn wrapped_navigation_enters_history_only_at_visual_boundaries() {
        let map = EditorVisualMap::new("one two three", 10);
        assert_eq!(map.lines, ["› one two ", "  three"]);
        let mut preferred = None;
        assert_eq!(map.movement(4, true, &mut preferred), Some(12));
        assert_eq!(map.movement(12, true, &mut preferred), None);
        assert_eq!(map.movement(12, false, &mut preferred), Some(4));
        assert_eq!(map.movement(4, false, &mut preferred), None);
    }

    #[test]
    fn preferred_cell_column_survives_short_and_wide_lines() {
        let source = "abcdef\n界\nabcdef";
        let map = EditorVisualMap::new(source, 20);
        let mut preferred = None;
        let short = map.movement(5, true, &mut preferred).unwrap();
        assert_eq!(short, "abcdef\n界".len());
        assert_eq!(preferred, Some(5));
        assert_eq!(
            map.movement(short, true, &mut preferred),
            Some(source.len() - 1)
        );
        let wide = map.byte_at(VisualPosition { row: 1, col: 3 }).unwrap();
        assert_eq!(wide, "abcdef\n".len());
    }

    #[test]
    fn full_hard_lines_have_distinct_before_and_after_newline_carets() {
        let map = EditorVisualMap::new("abcd\r\nx\n", 6);
        assert_eq!(map.caret(4), VisualPosition { row: 1, col: 2 });
        assert_eq!(map.caret(5), map.caret(4));
        assert_eq!(map.caret(6), VisualPosition { row: 2, col: 2 });
        assert_eq!(map.caret(8), VisualPosition { row: 3, col: 2 });
        assert_eq!(input_window("abcd", 4, 6, 1).lines, ["  "]);
    }

    #[test]
    fn map_window_and_reverse_mapping_share_safe_grapheme_positions() {
        for source in [
            "",
            "a\tb",
            "e\u{301}界👩\u{200d}🦀",
            "\u{301}\u{1b}\r\nend\n",
            "a    long_token",
        ] {
            for width in 1..=12 {
                let map = EditorVisualMap::new(source, width);
                assert!(map.lines.iter().all(|line| line.width() <= width));
                assert!(
                    !map.lines
                        .iter()
                        .any(|line| line.chars().any(char::is_control))
                );
                for cursor in source
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .chain([source.len()])
                {
                    let position = map.caret(cursor);
                    assert!(position.col < width);
                    for budget in 1..=4 {
                        let window = map.window(cursor, budget);
                        assert!(window.lines.len() <= budget);
                        assert_eq!(window.lines[window.cursor_row], map.lines[position.row]);
                        assert_eq!(window.cursor_col, position.col);
                    }
                }
                for byte in source
                    .grapheme_indices(true)
                    .map(|(byte, _)| byte)
                    .chain([source.len()])
                {
                    assert_eq!(
                        map.byte_at(map.caret(byte)),
                        Some(byte),
                        "{source:?}, {width}, {byte}"
                    );
                }
            }
        }
        let map = EditorVisualMap::new("\t", 1);
        assert_eq!(map.lines, [" ", " ", " ", " ", ""]);
        assert_eq!(map.movement(0, true, &mut None), Some(1));
    }

    #[test]
    #[should_panic]
    fn invalid_internal_byte_cursor_is_not_silently_clamped() {
        EditorVisualMap::new("界", 8).caret(1);
    }
}
