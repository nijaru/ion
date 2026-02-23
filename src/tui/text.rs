//! Text measurement and wrapping utilities.
//!
//! Single source of truth for display-width calculations and word wrapping.
//! All TUI modules should use these functions instead of calling
//! `UnicodeWidthChar` directly.

use unicode_width::UnicodeWidthChar;

/// Display width of a string in terminal cells.
///
/// Wide chars (CJK) count as 2. Zero-width combining chars count as 0.
pub(crate) fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Truncate a string to at most `max_cells` display cells.
///
/// Never splits a wide char. When one display cell remains and the next
/// char is 2 cells wide, a space is appended instead (padding to avoid a
/// visible gap). Returns an owned `String` because the padding case
/// requires allocation.
pub(crate) fn truncate_to_width(s: &str, max_cells: usize) -> String {
    let mut width = 0usize;
    let mut end = 0usize;
    let mut needs_space_pad = false;

    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_cells {
            if cw == 2 && width + 1 == max_cells {
                needs_space_pad = true;
            }
            break;
        }
        width += cw;
        end += ch.len_utf8();
    }

    if needs_space_pad {
        let mut out = s[..end].to_string();
        out.push(' ');
        out
    } else {
        s[..end].to_string()
    }
}

/// Word-wrap a string to at most `width` display cells per line.
///
/// Returns owned `String`s; each line is its own allocation. Splits on
/// whitespace boundaries. When a word is wider than `width`, it is
/// character-broken across lines.
///
/// An empty input or `width == 0` returns a single empty string.
pub(crate) fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 || s.is_empty() {
        return vec![String::new()];
    }

    if display_width(s) <= width {
        return vec![s.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in split_words(s) {
        let word_width = display_width(word);
        let is_space = word.chars().next().is_some_and(|c| c == ' ');

        if is_space {
            // Space between words: add to current line if it fits, otherwise
            // drop it (spaces at line break boundaries are discarded).
            if current_width + word_width <= width && !current.is_empty() {
                current.push_str(word);
                current_width += word_width;
            }
            continue;
        }

        if current_width + word_width <= width {
            current.push_str(word);
            current_width += word_width;
        } else if word_width <= width {
            // Word fits on a fresh line — wrap.
            if !current.is_empty() {
                chunks.push(current.trim_end().to_string());
            }
            current = word.to_string();
            current_width = word_width;
        } else {
            // Word wider than line — char-break.
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_width + cw > width && !current.is_empty() {
                    chunks.push(current.trim_end().to_string());
                    current = String::new();
                    current_width = 0;
                }
                current.push(ch);
                current_width += cw;
            }
        }
    }

    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Safe terminal width: `terminal_width - 1` to prevent cursor autowrap at
/// the right edge.
pub(crate) fn safe_width(terminal_width: u16) -> usize {
    terminal_width.saturating_sub(1) as usize
}

/// Split text into alternating word and whitespace segments.
///
/// Whitespace segments (spaces) are kept as separate entries so that
/// `wrap_text` can decide whether to include them at line boundaries.
fn split_words(text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_space = text.starts_with(' ');

    for (i, ch) in text.char_indices() {
        let is_space = ch == ' ';
        if is_space != in_space {
            if start < i {
                segments.push(&text[start..i]);
            }
            start = i;
            in_space = is_space;
        }
    }
    if start < text.len() {
        segments.push(&text[start..]);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
        assert_eq!(display_width(" "), 1);
    }

    #[test]
    fn display_width_cjk() {
        assert_eq!(display_width("界"), 2);
        assert_eq!(display_width("界a"), 3);
        assert_eq!(display_width("界界"), 4);
    }

    #[test]
    fn truncate_ascii_exact() {
        assert_eq!(truncate_to_width("hello", 3), "hel");
        assert_eq!(truncate_to_width("hello", 5), "hello");
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    #[test]
    fn truncate_cjk_fits() {
        // 界 is 2 cells — fits exactly at max_cells=2
        assert_eq!(truncate_to_width("界a", 2), "界");
        assert_eq!(truncate_to_width("界a", 3), "界a");
    }

    #[test]
    fn truncate_cjk_pad() {
        // 1 cell left, next char is 2 cells wide — pad with space
        let result = truncate_to_width("a界", 2);
        assert_eq!(result, "a ");
        assert_eq!(display_width(&result), 2);
    }

    #[test]
    fn wrap_text_fits() {
        assert_eq!(wrap_text("hello", 20), vec!["hello"]);
        assert_eq!(wrap_text("hello world", 20), vec!["hello world"]);
    }

    #[test]
    fn wrap_text_empty() {
        assert_eq!(wrap_text("", 80), vec!["".to_string()]);
        assert_eq!(wrap_text("hello", 0), vec!["".to_string()]);
    }

    #[test]
    fn wrap_text_breaks_at_word() {
        let chunks = wrap_text("hello world", 5);
        assert_eq!(chunks, vec!["hello", "world"]);
    }

    #[test]
    fn wrap_text_multi_word() {
        let chunks = wrap_text("hello world foo bar", 10);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(display_width(chunk) <= 10, "chunk too wide: {chunk:?}");
        }
    }

    #[test]
    fn wrap_text_long_word_char_breaks() {
        let long = "abcdefghij";
        let chunks = wrap_text(long, 4);
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn safe_width_reserves_one_cell() {
        assert_eq!(safe_width(80), 79);
        assert_eq!(safe_width(1), 0);
        assert_eq!(safe_width(0), 0);
    }
}
