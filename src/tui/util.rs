//! TUI utility functions.

use crate::tui::filter_input;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;

/// Format token count as human-readable (e.g., 1500 -> "1.5k")
pub(super) fn format_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Format a Unix timestamp as a relative time string.
pub(super) fn format_relative_time(timestamp: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - timestamp;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let mins = diff / 60;
        format!("{}m ago", mins)
    } else if diff < 86400 {
        let hours = diff / 3600;
        format!("{}h ago", hours)
    } else if diff < 604800 {
        let days = diff / 86400;
        format!("{}d ago", days)
    } else {
        let weeks = diff / 604800;
        format!("{}w ago", weeks)
    }
}

/// Normalize errors for status line display.
pub(super) fn format_status_error(msg: &str) -> String {
    let mut out = msg.trim().to_string();
    for prefix in ["Completion error: ", "Stream error: ", "API error: "] {
        if let Some(rest) = out.strip_prefix(prefix) {
            out = rest.to_string();
        }
    }
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
    match key.code {
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

/// Strip ANSI escape sequences from a string.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            in_escape = true;
            chars.next(); // consume '['
            continue;
        }
        if in_escape {
            // End of escape sequence on 'm' or other command char
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        result.push(c);
    }
    result
}

/// Sanitize text for terminal display.
/// - Converts tabs to 4 spaces (consistent width)
/// - Strips carriage returns (prevents text overwrite)
/// - Strips other control characters except newlines
pub(crate) fn sanitize_for_display(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => result.push_str("    "), // Tab to 4 spaces
            '\r' => {}                       // Strip carriage returns
            '\n' => result.push(c),          // Keep newlines
            c if c.is_control() => {}        // Strip other control chars
            c => result.push(c),
        }
    }
    result
}

/// Convert a borrowed Line to an owned Line<'static>.
pub(crate) fn own_line(line: Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content.to_string(), span.style))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_for_display() {
        // Tabs converted to 4 spaces
        assert_eq!(sanitize_for_display("a\tb"), "a    b");

        // Carriage returns stripped
        assert_eq!(sanitize_for_display("a\r\nb"), "a\nb");
        assert_eq!(sanitize_for_display("a\rb"), "ab");

        // Newlines preserved
        assert_eq!(sanitize_for_display("a\nb"), "a\nb");

        // Control characters stripped
        assert_eq!(sanitize_for_display("a\x00b"), "ab");
        assert_eq!(sanitize_for_display("a\x1fb"), "ab");

        // Normal text unchanged
        assert_eq!(sanitize_for_display("hello world"), "hello world");

        // Mixed content
        assert_eq!(
            sanitize_for_display("line1\r\n\tindented\nline3"),
            "line1\n    indented\nline3"
        );
    }
}
