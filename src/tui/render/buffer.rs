//! Row-based buffer for the bottom UI area.
//!
//! `Buffer` stores one pre-rendered ANSI string per row, indexed from 0
//! (relative to the top of the UI region). It supports:
//! - `to_plain_lines()` for snapshot testing (strips ANSI escapes)
//! - `diff()` for efficient terminal updates (only changed rows)

use crossterm::{
    cursor::MoveTo,
    execute,
    terminal::{Clear, ClearType},
};
use std::io::Write;

/// A row-indexed buffer for the bottom UI area.
///
/// Each row stores a pre-rendered ANSI string (as produced by `ansi::render_line`
/// or `ansi::render_spans`). Row 0 is the topmost row of the UI region.
pub struct Buffer {
    width: usize,
    rows: Vec<String>,
}

/// A row whose content changed between frames.
pub struct DiffCommand {
    /// Row index relative to the buffer origin (0-based).
    pub row: usize,
    /// ANSI content to write at that row.
    pub content: String,
}

impl Buffer {
    /// Create a new buffer with `height` empty rows.
    pub fn new(width: usize, height: usize) -> Self {
        Buffer {
            width,
            rows: vec![String::new(); height],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    /// Set the pre-rendered ANSI content for a row.
    pub fn set_row(&mut self, row: usize, content: String) {
        if row < self.rows.len() {
            self.rows[row] = content;
        }
    }

    /// Get ANSI content of a row (for testing).
    pub fn row(&self, row: usize) -> &str {
        self.rows.get(row).map(String::as_str).unwrap_or("")
    }

    /// Extract plain text (ANSI-stripped) for each row.
    ///
    /// Suitable for snapshot assertions that don't care about colours or styles.
    pub fn to_plain_lines(&self) -> Vec<String> {
        self.rows.iter().map(|r| strip_ansi(r)).collect()
    }

    /// Return rows that differ from `prev`.
    ///
    /// Rows beyond `prev`'s height are always included (they are new rows).
    pub fn diff(&self, prev: &Buffer) -> Vec<DiffCommand> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(i, row)| prev.rows.get(*i).map_or(true, |p| p != *row))
            .map(|(i, row)| DiffCommand {
                row: i,
                content: row.clone(),
            })
            .collect()
    }
}

/// Write diff commands to the terminal, offset by `ui_start_row`.
///
/// Each changed row is moved to its absolute position, cleared, and rewritten.
pub fn flush_diff<W: Write>(
    w: &mut W,
    commands: &[DiffCommand],
    ui_start_row: u16,
) -> std::io::Result<()> {
    for cmd in commands {
        let abs_row = ui_start_row + cmd.row as u16;
        execute!(w, MoveTo(0, abs_row), Clear(ClearType::CurrentLine))?;
        write!(w, "{}", cmd.content)?;
    }
    Ok(())
}

/// Strip ANSI CSI escape sequences (e.g. colour codes) from `s`.
///
/// Handles `ESC [ ... <final>` sequences where the final byte is in `@–~` (0x40–0x7E).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
                          // consume until final byte (@–~ i.e. 0x40–0x7E)
            for ch in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&ch) {
                    break;
                }
            }
        } else if c == '\x1b' && chars.peek() == Some(&'(') {
            // Character set designations: ESC ( x — consume 2 chars
            chars.next();
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_plain_lines_strips_ansi() {
        let mut buf = Buffer::new(80, 2);
        buf.set_row(0, "\x1b[32mhello\x1b[0m world".to_string());
        buf.set_row(1, "plain text".to_string());
        let lines = buf.to_plain_lines();
        assert_eq!(lines[0], "hello world");
        assert_eq!(lines[1], "plain text");
    }

    #[test]
    fn test_diff_unchanged() {
        let mut prev = Buffer::new(80, 2);
        prev.set_row(0, "line 0".to_string());
        prev.set_row(1, "line 1".to_string());
        let mut next = Buffer::new(80, 2);
        next.set_row(0, "line 0".to_string());
        next.set_row(1, "line 1".to_string());
        let commands = next.diff(&prev);
        assert!(commands.is_empty(), "no changes -> no commands");
    }

    #[test]
    fn test_diff_single_change() {
        let mut prev = Buffer::new(80, 2);
        prev.set_row(0, "old".to_string());
        prev.set_row(1, "same".to_string());
        let mut next = Buffer::new(80, 2);
        next.set_row(0, "new".to_string());
        next.set_row(1, "same".to_string());
        let commands = next.diff(&prev);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].row, 0);
        assert_eq!(commands[0].content, "new");
    }

    #[test]
    fn test_diff_new_rows() {
        let prev = Buffer::new(80, 1);
        let mut next = Buffer::new(80, 2);
        next.set_row(0, "row 0".to_string());
        next.set_row(1, "row 1".to_string());
        // Row 0: prev is empty "", next is "row 0" -> changed
        // Row 1: beyond prev height -> changed
        let commands = next.diff(&prev);
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn test_strip_ansi_empty() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_strip_ansi_bold_reset() {
        let s = "\x1b[1mbold\x1b[0m";
        assert_eq!(strip_ansi(s), "bold");
    }

    #[test]
    fn test_strip_ansi_colour_and_text() {
        let s = "before \x1b[33myellow\x1b[0m after";
        assert_eq!(strip_ansi(s), "before yellow after");
    }
}
