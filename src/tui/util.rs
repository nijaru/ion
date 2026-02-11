//! TUI utility functions.

use crate::provider::format_api_error;
use crate::tui::filter_input;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

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

/// Truncate a string to a maximum display width (terminal cells).
/// Uses Unicode width rules and never appends a trailing newline.
pub(crate) fn truncate_to_display_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out
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
    fn test_truncate_to_display_width_ascii() {
        assert_eq!(truncate_to_display_width("hello", 10), "hello");
        assert_eq!(truncate_to_display_width("hello", 4), "hell");
        assert_eq!(truncate_to_display_width("hello", 0), "");
    }

    #[test]
    fn test_truncate_to_display_width_unicode() {
        // Full-width CJK char should count as width 2.
        assert_eq!(truncate_to_display_width("界a", 2), "界");
        assert_eq!(truncate_to_display_width("界a", 3), "界a");
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
        // Can only test the non-home paths deterministically (home dir varies)
        // but we can test that non-matching paths pass through unchanged
        assert_eq!(shorten_home_prefix("/etc/config"), "/etc/config");
        assert_eq!(shorten_home_prefix("/tmp/foo"), "/tmp/foo");
        assert_eq!(shorten_home_prefix("relative/path"), "relative/path");
        assert_eq!(shorten_home_prefix(""), "");

        // If home dir is available, test path-boundary safety
        if let Some(home) = dirs::home_dir() {
            let home_str = home.display().to_string();

            // Exact home dir
            assert_eq!(shorten_home_prefix(&home_str), "~");

            // Subpath of home
            let sub = format!("{home_str}/projects/foo");
            assert_eq!(shorten_home_prefix(&sub), "~/projects/foo");

            // Path sharing prefix but not a child (e.g. /Users/nicky vs /Users/nick)
            let sibling = format!("{home_str}xxx/projects");
            assert_eq!(shorten_home_prefix(&sibling), sibling);
        }
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
