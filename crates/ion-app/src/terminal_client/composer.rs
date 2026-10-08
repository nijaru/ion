use std::collections::VecDeque;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct InputWindow {
    pub(super) lines: Vec<String>,
    pub(super) cursor_row: usize,
    pub(super) cursor_col: usize,
}

struct Rows {
    lines: VecDeque<String>,
    count: usize,
    limit: usize,
}

impl Rows {
    // Recycle rows outside the viewport rather than allocating every wrapped
    // line of a recovered aggregate draft. No editor text is changed or lost.
    fn push(&mut self, line: &mut String, cursor: Option<(usize, usize)>) -> bool {
        let mut reusable = if self.lines.len() == self.limit {
            self.lines.pop_front().expect("full composer window")
        } else {
            String::new()
        };
        reusable.clear();
        self.lines.push_back(std::mem::replace(line, reusable));
        self.count += 1;
        cursor.is_some_and(|(row, _)| self.count >= self.limit.max(row + 1))
    }

    fn window(self, cursor: (usize, usize)) -> InputWindow {
        InputWindow {
            cursor_row: cursor.0 - (self.count - self.lines.len()),
            cursor_col: cursor.1,
            lines: self.lines.into_iter().collect(),
        }
    }
}

// Each standalone LF contributes at least one physical row. Earlier logical
// lines therefore cannot be in a cursor window of this size. CRLF is one
// grapheme, not the standalone newline handled by the layout below.
fn window_start(draft: &str, cursor: usize, max_rows: usize) -> usize {
    draft[..cursor]
        .rmatch_indices('\n')
        .filter(|(byte, _)| *byte == 0 || draft.as_bytes()[byte - 1] != b'\r')
        .nth(max_rows - 1)
        .map_or(0, |(byte, _)| byte + 1)
}

/// Select only the actual row budget, with the cursor's line always visible.
/// Neither earlier logical lines nor text after the completed window is laid out.
pub(super) fn input_window(
    draft: &str,
    cursor: usize,
    width: usize,
    max_rows: usize,
) -> InputWindow {
    assert!(cursor <= draft.len() && draft.is_char_boundary(cursor));
    let width = width.max(1);
    let (prompt, indent) = match width {
        1 | 2 => ("", ""),
        3 => ("›", " "),
        _ => ("› ", "  "),
    };
    let margin = UnicodeWidthStr::width(prompt);
    let limit = max_rows.max(1);
    let start = window_start(draft, cursor, limit);
    let cursor = cursor - start;
    let draft = &draft[start..];
    let mut rows = Rows {
        lines: VecDeque::new(),
        count: 0,
        limit,
    };
    let mut line = if start == 0 { prompt } else { indent }.to_owned();
    let mut col = margin;
    let mut position = None;
    for (byte, grapheme) in draft.grapheme_indices(true) {
        if grapheme == "\n" {
            if byte == cursor {
                position = Some(if col == width {
                    (rows.count + 1, margin)
                } else {
                    (rows.count, col)
                });
            }
            if rows.push(&mut line, position) {
                return rows.window(position.expect("completed cursor window"));
            }
            line.push_str(indent);
            col = margin;
            continue;
        }
        let (unit, repetitions) = if grapheme == "\t" {
            (" ", 4)
        } else {
            (grapheme, 1)
        };
        // Expand tabs across rows. A glyph wider than the entire content area
        // needs a display-only placeholder; the literal and byte cursor stay intact.
        for (part, unit) in std::iter::repeat_n(unit, repetitions).enumerate() {
            let size = UnicodeWidthStr::width(unit);
            let (unit, size) = if size > width - margin {
                ("�", 1)
            } else {
                (unit, size)
            };
            if col >= width || col + size > width {
                if rows.push(&mut line, position) {
                    return rows.window(position.expect("completed cursor window"));
                }
                line.push_str(indent);
                col = margin;
            }
            if part == 0 && (byte..byte + grapheme.len()).contains(&cursor) {
                position = Some((rows.count, col));
            }
            line.push_str(unit);
            col += size;
        }
    }
    if cursor == draft.len() {
        if col >= width {
            rows.push(&mut line, position);
            line.push_str(indent);
            col = margin;
        }
        position = Some((rows.count, col));
    }
    rows.push(&mut line, position);
    rows.window(position.expect("valid byte cursor belongs to a composer row"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_windows_contain_the_cursor_without_clipping_a_different_line() {
        let draft = "first\nsecond\nthird\nfourth";
        for cursor in [0, 6, 13, draft.len()] {
            for rows in 1..=4 {
                let input = input_window(draft, cursor, 30, rows);
                assert_eq!(input.lines.len(), rows);
                assert!(input.cursor_row < rows);
            }
        }
        for (cursor, lines, position) in [
            (0, vec!["› first", "  second"], (0, 2)),
            (13, vec!["  second", "  third"], (1, 2)),
            (draft.len(), vec!["  third", "  fourth"], (1, 8)),
        ] {
            let input = input_window(draft, cursor, 30, 2);
            assert_eq!(input.lines, lines);
            assert_eq!((input.cursor_row, input.cursor_col), position);
        }
        let input = input_window("abcdef\nz", 6, 8, 1);
        assert_eq!(input.lines, ["  z"]);
        assert_eq!((input.cursor_row, input.cursor_col), (0, 2));
    }

    #[test]
    fn narrow_rows_and_tabs_keep_cells_and_cursor_inside_the_surface() {
        for width in 1..=8 {
            for draft in ["a", "🦀", "a\tb", "\u{0301}a", "one\ntwo"] {
                for cursor in draft
                    .char_indices()
                    .map(|(byte, _)| byte)
                    .chain([draft.len()])
                {
                    for rows in 1..=4 {
                        let input = input_window(draft, cursor, width, rows);
                        assert!(input.cursor_col < width, "{width}: {draft:?}");
                        assert!(input.lines.iter().all(|line| line.width() <= width));
                        assert!(input.cursor_row < input.lines.len());
                        assert!(input.lines.len() <= rows);
                    }
                }
            }
        }
        // Four display spaces survive wrapping; the source tab is never rewritten.
        assert_eq!(input_window("\t", 1, 2, 3).lines, ["  ", "  ", ""]);
    }

    #[test]
    fn cursor_inside_a_joined_grapheme_stays_on_its_wrapped_row() {
        let draft = "first\n👩\u{200d}🦀";
        let input = input_window(draft, "first\n👩\u{200d}".len(), 30, 2);
        assert_eq!((input.cursor_row, input.cursor_col), (1, 2));
    }

    #[test]
    fn unicode_cursor_tracks_the_original_byte_position() {
        let draft = "ab🦀\nnext";
        let input = input_window(draft, draft.len(), 8, 2);
        assert_eq!(input.lines, vec!["› ab🦀", "  next"]);
        assert_eq!((input.cursor_row, input.cursor_col), (1, 6));
        let leading_combining = "\u{0301}a";
        let input = input_window(leading_combining, leading_combining.len(), 8, 2);
        assert_eq!((input.cursor_row, input.cursor_col), (0, 3));
    }
}
