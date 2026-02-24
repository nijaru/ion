//! TUI utility functions.

use crate::provider::format_api_error;
use crate::tui::filter_input;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

/// Format a dollar cost as human-readable (e.g., "$0.0042", "$1.23").
pub(super) fn format_cost(cost: f64) -> String {
    if cost < 0.0001 {
        "$0.00".to_string()
    } else if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

/// Normalize errors for status line display.
pub(super) fn format_status_error(msg: &str) -> String {
    // First extract message from JSON if present
    let mut out = format_api_error(msg.trim());

    // Strip common prefixes
    for prefix in ["Completion error: ", "Stream error: ", "API error: "] {
        if let Some(rest) = out.strip_prefix(prefix) {
            out = rest.to_string();
        }
    }

    // Friendly message for common errors
    if out.to_lowercase().contains("operation timed out") {
        return "Network timeout".to_string();
    }

    out
}

/// Handle key event for filter input. Returns true if text changed.
pub(super) fn handle_filter_input_event(
    state: &mut filter_input::FilterInputState,
    key: KeyEvent,
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Char('w') if ctrl => {
            state.delete_word();
            true
        }
        KeyCode::Char('u') if ctrl => {
            state.delete_line_left();
            true
        }
        KeyCode::Char(c) => {
            state.insert_char(c);
            true
        }
        KeyCode::Backspace => {
            state.delete_char_before();
            true
        }
        KeyCode::Delete => {
            state.delete_char_after();
            true
        }
        KeyCode::Left => {
            state.move_left();
            false
        }
        KeyCode::Right => {
            state.move_right();
            false
        }
        KeyCode::Home => {
            state.move_to_start();
            false
        }
        KeyCode::End => {
            state.move_to_end();
            false
        }
        _ => false,
    }
}

/// Shorten a path by replacing the home directory prefix with `~`.
pub(crate) fn shorten_home_prefix(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.display().to_string();
        if let Some(rest) = path.strip_prefix(&home_str)
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

/// Return the display width (terminal cells) of a string.
#[must_use]
pub(crate) fn display_width(s: &str) -> usize {
    s.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

/// Normalize user input for history/storage.
/// - Normalizes CRLF to LF
/// - Trims trailing whitespace (keeps leading indentation)
pub(crate) fn normalize_input(content: &str) -> String {
    let mut out = content.replace("\r\n", "\n").replace('\r', "\n");
    while out.ends_with(|c: char| c.is_whitespace()) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("界a"), 3);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(0.00001), "$0.00");
        assert_eq!(format_cost(0.0042), "$0.0042");
        assert_eq!(format_cost(0.0100), "$0.01");
        assert_eq!(format_cost(1.234), "$1.23");
    }

    #[test]
    fn test_shorten_home_prefix() {
        assert_eq!(shorten_home_prefix("/etc/config"), "/etc/config");
        assert_eq!(shorten_home_prefix("/tmp/foo"), "/tmp/foo");
        assert_eq!(shorten_home_prefix("relative/path"), "relative/path");
        assert_eq!(shorten_home_prefix(""), "");

        if let Some(home) = dirs::home_dir() {
            let home_str = home.display().to_string();

            assert_eq!(shorten_home_prefix(&home_str), "~");

            let sub = format!("{home_str}/projects/foo");
            assert_eq!(shorten_home_prefix(&sub), "~/projects/foo");

            let sibling = format!("{home_str}xxx/projects");
            assert_eq!(shorten_home_prefix(&sibling), sibling);
        }
    }
}
