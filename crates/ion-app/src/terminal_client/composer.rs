use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) struct WrappedInput {
    pub(super) lines: Vec<String>,
    pub(super) cursor_row: usize,
    pub(super) cursor_col: usize,
}

impl WrappedInput {
    /// Select within the actual row budget, keeping the cursor's line visible.
    pub(super) fn visible_range(&self, max_rows: usize) -> Range<usize> {
        let rows = self.lines.len().min(max_rows);
        let start = self
            .cursor_row
            .saturating_sub(rows.saturating_sub(1))
            .min(self.lines.len() - rows);
        start..start + rows
    }
}

pub(super) fn wrap_input(draft: &str, cursor: usize, width: usize) -> WrappedInput {
    let width = width.max(1);
    let (prompt, indent) = match width {
        1 | 2 => ("", ""),
        3 => ("›", " "),
        _ => ("› ", "  "),
    };
    let margin = UnicodeWidthStr::width(prompt);
    let mut lines = Vec::new();
    let mut line = prompt.to_owned();
    let mut col = margin;
    let mut position = (0, margin);
    for (byte, grapheme) in draft.grapheme_indices(true) {
        if grapheme == "\n" {
            if byte == cursor {
                position = if col == width {
                    (lines.len() + 1, margin)
                } else {
                    (lines.len(), col)
                };
            }
            lines.push(line);
            line = indent.into();
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
                lines.push(line);
                line = indent.into();
                col = margin;
            }
            if part == 0 && (byte..byte + grapheme.len()).contains(&cursor) {
                position = (lines.len(), col);
            }
            line.push_str(unit);
            col += size;
        }
    }
    if cursor == draft.len() {
        if col >= width {
            lines.push(line);
            line = indent.into();
            col = margin;
        }
        position = (lines.len(), col);
    }
    lines.push(line);
    WrappedInput {
        lines,
        cursor_row: position.0,
        cursor_col: position.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_windows_contain_the_cursor_without_clipping_a_different_line() {
        let draft = "first\nsecond\nthird\nfourth";
        for cursor in [0, 6, 13, draft.len()] {
            let input = wrap_input(draft, cursor, 30);
            for rows in 1..=4 {
                let visible = input.visible_range(rows);
                assert_eq!(visible.len(), rows);
                assert!(visible.contains(&input.cursor_row));
                assert!(visible.end <= input.lines.len());
            }
        }
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
                    let input = wrap_input(draft, cursor, width);
                    assert!(input.cursor_col < width, "{width}: {draft:?}");
                    assert!(input.lines.iter().all(|line| line.width() <= width));
                    assert!(input.visible_range(1).contains(&input.cursor_row));
                }
            }
        }
        // Four display spaces survive wrapping; the source tab is never rewritten.
        assert_eq!(wrap_input("\t", 1, 2).lines, ["  ", "  ", ""]);
    }

    #[test]
    fn cursor_inside_a_joined_grapheme_stays_on_its_wrapped_row() {
        let draft = "first\n👩\u{200d}🦀";
        let input = wrap_input(draft, "first\n👩\u{200d}".len(), 30);
        assert_eq!((input.cursor_row, input.cursor_col), (1, 2));
    }

    #[test]
    fn unicode_cursor_tracks_the_original_byte_position() {
        let draft = "ab🦀\nnext";
        let input = wrap_input(draft, draft.len(), 8);
        assert_eq!(input.lines, vec!["› ab🦀", "  next"]);
        assert_eq!((input.cursor_row, input.cursor_col), (1, 6));
        let leading_combining = "\u{0301}a";
        let input = wrap_input(leading_combining, leading_combining.len(), 8);
        assert_eq!((input.cursor_row, input.cursor_col), (0, 3));
    }
}
