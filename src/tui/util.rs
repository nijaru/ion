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

/// Convert a borrowed Line to an owned Line<'static>.
pub(crate) fn own_line(line: Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content.to_string(), span.style))
            .collect::<Vec<_>>(),
    )
}
