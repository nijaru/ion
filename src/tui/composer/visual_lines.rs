use unicode_width::UnicodeWidthChar;

/// Build a list of visual lines as (`start_char_idx`, `end_char_idx`) pairs using word-wrap.
/// This matches Ratatui's `Paragraph::wrap(Wrap` { trim: false }) behavior.
/// `end_char_idx` is exclusive.
#[must_use]
pub fn build_visual_lines(content: &str, width: usize) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    if width == 0 {
        lines.push((0, content.chars().count()));
        return lines;
    }

    let chars: Vec<char> = content.chars().collect();
    let mut line_start = 0;
    let mut col = 0;
    let mut last_space_idx = None::<usize>; // char index AFTER the space

    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            lines.push((line_start, i + 1)); // Include newline in range
            line_start = i + 1;
            col = 0;
            last_space_idx = None;
        } else {
            let char_width = UnicodeWidthChar::width(c).unwrap_or(0);

            if col + char_width > width {
                // Need to wrap
                if let Some(space_idx) = last_space_idx {
                    // Wrap at the last space
                    lines.push((line_start, space_idx));
                    line_start = space_idx;
                    // Recalculate col from space_idx to i
                    col = 0;
                    for ch in chars.iter().take(i).skip(space_idx) {
                        col += UnicodeWidthChar::width(*ch).unwrap_or(0);
                    }
                    last_space_idx = None;
                } else {
                    // No space - wrap at character boundary
                    lines.push((line_start, i));
                    line_start = i;
                    col = 0;
                }
            }

            if c == ' ' {
                last_space_idx = Some(i + 1);
            }

            col += char_width;
        }
    }

    // Final line
    lines.push((line_start, chars.len()));
    lines
}

/// Find which visual line contains the given char index and the column within that line.
pub fn find_visual_line_and_col(lines: &[(usize, usize)], char_idx: usize) -> (usize, usize) {
    for (i, (start, end)) in lines.iter().enumerate() {
        if char_idx >= *start && char_idx < *end {
            return (i, char_idx - start);
        }
        // Handle cursor at end of line (at the boundary)
        if char_idx == *end && i == lines.len() - 1 {
            return (i, char_idx - start);
        }
    }
    // Cursor at very end
    let last = lines.len().saturating_sub(1);
    (last, char_idx.saturating_sub(lines[last].0))
}
