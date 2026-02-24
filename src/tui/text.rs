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
        assert_eq!(truncate_to_width("界a", 2), "界");
        assert_eq!(truncate_to_width("界a", 3), "界a");
    }

    #[test]
    fn truncate_cjk_pad() {
        let result = truncate_to_width("a界", 2);
        assert_eq!(result, "a ");
        assert_eq!(display_width(&result), 2);
    }
}
