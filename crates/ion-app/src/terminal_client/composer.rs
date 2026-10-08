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
    let width = width.max(3);
    let mut lines = Vec::new();
    let mut line = "› ".to_owned();
    let mut col = 2;
    let mut position = (0, 2);
    for (byte, grapheme) in draft.grapheme_indices(true) {
        if grapheme == "\n" {
            if byte == cursor {
                position = (lines.len(), col);
            }
            lines.push(line);
            line = "  ".into();
            col = 2;
            continue;
        }
        let display = if grapheme == "\t" { "    " } else { grapheme };
        let size = UnicodeWidthStr::width(display).max(1);
        if col + size > width && col > 2 {
            lines.push(line);
            line = "  ".into();
            col = 2;
        }
        if byte == cursor {
            position = (lines.len(), col);
        }
        line.push_str(display);
        col += size;
    }
    if cursor == draft.len() {
        if col >= width {
            lines.push(line);
            line = "  ".into();
            col = 2;
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
    fn unicode_cursor_tracks_the_original_byte_position() {
        let draft = "ab🦀\nnext";
        let input = wrap_input(draft, draft.len(), 8);
        assert_eq!(input.lines, vec!["› ab🦀", "  next"]);
        assert_eq!((input.cursor_row, input.cursor_col), (1, 6));
    }
}
