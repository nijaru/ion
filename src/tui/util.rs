//! TUI utility functions.

use crate::provider::format_api_error;
use crate::tui::filter_input;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Format token count as human-readable (e.g., 1500 -> "1.5k")
#[allow(clippy::cast_precision_loss)] // Precision loss acceptable for display
pub(super) fn format_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Format context window as human-readable (e.g., 128000 -> "128K", 1000000 -> "1M")
#[allow(clippy::cast_precision_loss)] // Precision loss acceptable for display
pub(super) fn format_context_window(n: u32) -> String {
    if n == 0 {
        return String::new();
    }
    let n = n as f64;
    if n >= 1_000_000.0 {
        let m = n / 1_000_000.0;
        if (m - m.round()).abs() < 0.05 {
            format!("{}M", m.round() as u32)
        } else {
            format!("{:.1}M", m)
        }
    } else if n >= 1000.0 {
        let k = n / 1000.0;
        if (k - k.round()).abs() < 0.5 {
            format!("{}K", k.round() as u32)
        } else {
            format!("{:.0}K", k)
        }
    } else {
        format!("{}", n as u32)
    }
}

/// Format seconds as human-readable duration (e.g., "1m 30s" or "45s")
pub(super) fn format_elapsed(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
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
        format!("{mins}m ago")
    } else if diff < 86400 {
        let hours = diff / 3600;
        format!("{hours}h ago")
    } else if diff < 604_800 {
        let days = diff / 86400;
        format!("{days}d ago")
    } else {
        let weeks = diff / 604_800;
        format!("{weeks}w ago")
    }
}

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

    #[test]
    fn test_format_cost() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(0.00001), "$0.00");
        assert_eq!(format_cost(0.0042), "$0.0042");
        assert_eq!(format_cost(0.0100), "$0.01");
        assert_eq!(format_cost(1.234), "$1.23");
    }

    #[test]
    fn test_format_context_window() {
        // Zero returns empty
        assert_eq!(format_context_window(0), "");

        // Small values
        assert_eq!(format_context_window(512), "512");

        // Thousands (K)
        assert_eq!(format_context_window(4096), "4K");
        assert_eq!(format_context_window(8192), "8K");
        assert_eq!(format_context_window(32768), "33K");
        assert_eq!(format_context_window(128000), "128K");
        assert_eq!(format_context_window(200000), "200K");

        // Millions (M)
        assert_eq!(format_context_window(1_000_000), "1M");
        assert_eq!(format_context_window(1_100_000), "1.1M");
        assert_eq!(format_context_window(2_000_000), "2M");
    }
}
