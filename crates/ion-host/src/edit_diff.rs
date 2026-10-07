//! A bounded observation of the edit's read/write snapshots, not a later disk diff.
use std::{
    fmt::{self, Write},
    time::Duration,
};

use serde::Serialize;
use similar::TextDiff;

const CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Serialize)]
pub(crate) struct EditDiff {
    text: String,
    truncated: bool,
}

pub(crate) fn capture(path: &str, before: &str, after: &str) -> EditDiff {
    // This limits diff search, not all parsing/formatting or host execution.
    let diff = TextDiff::configure()
        .timeout(Duration::from_millis(100))
        .diff_lines(before, after);
    // Escaping the filename prevents newlines/control bytes becoming patch headers.
    let header = format!("{path:?}");
    let mut capture = EditDiff {
        text: String::new(),
        truncated: false,
    };
    let result = write!(
        capture,
        "{}",
        diff.unified_diff()
            .context_radius(3)
            .header(&header, &header)
    );
    // Our writer's only error is the intentional capture bound.
    debug_assert!(result.is_ok() || capture.truncated);
    capture
}

impl Write for EditDiff {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let available = CAPTURE_BYTES - self.text.len();
        if text.len() <= available {
            self.text.push_str(text);
            return Ok(());
        }
        let mut end = available;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&text[..end]);
        self.truncated = true;
        Err(fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_preserve_context_and_missing_newlines_not_requested_anchors() {
        let patch = capture("a\npath", "before\nold\nafter", "before\nnew\nafter");
        assert!(!patch.truncated);
        assert!(
            patch
                .text
                .contains(" before\n-old\n+new\n after\n\\ No newline at end of file"),
            "{:?}",
            patch.text
        );
        assert!(
            patch
                .text
                .starts_with("--- \"a\\npath\"\n+++ \"a\\npath\"\n")
        );
        assert!(capture("same", "unchanged", "unchanged").text.is_empty());
    }

    #[test]
    fn large_changed_lines_have_an_explicit_utf8_safe_capture_bound() {
        let patch = capture("large", &"界".repeat(CAPTURE_BYTES), "replacement\n");
        assert!(patch.truncated);
        assert!(patch.text.len() <= CAPTURE_BYTES);
        assert!(patch.text.is_char_boundary(patch.text.len()));
    }
}
