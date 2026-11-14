//! A bounded observation of the edit's read/write snapshots, not a later disk diff.
use std::{
    fmt::{self, Write},
    time::Duration,
};

use serde::Serialize;
use similar::{DiffTag, TextDiff};

const CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Serialize)]
pub(crate) struct EditDiff {
    text: String,
    truncated: bool,
    added: usize,
    removed: usize,
}

pub(crate) fn capture(path: &str, before: &str, after: &str) -> EditDiff {
    // This limits diff search, not all parsing/formatting or host execution.
    let diff = TextDiff::configure()
        .timeout(Duration::from_millis(100))
        .diff_lines(before, after);
    // Escaping the filename prevents newlines/control bytes becoming patch headers.
    let header = format!("{path:?}");
    // Count the snapshot diff operations, not the bounded formatted patch:
    // capture may stop before later hunks or even the first added line.
    let (added, removed) = diff
        .ops()
        .iter()
        .filter(|op| op.tag() != DiffTag::Equal)
        .fold((0, 0), |(added, removed), op| {
            (added + op.new_range().len(), removed + op.old_range().len())
        });
    let mut capture = EditDiff {
        text: String::new(),
        truncated: false,
        added,
        removed,
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
    fn line_totals_describe_snapshot_changes_not_context_or_patch_headers() {
        for (before, after, added, removed) in [
            ("", "one\ntwo\n", 2, 0),
            ("one\ntwo\n", "", 0, 2),
            ("one\ntwo\nthree\n", "one\nnew\nextra\nthree\n", 2, 1),
            ("one", "one\n", 1, 1),
            ("same\r\n", "same\r\n", 0, 0),
        ] {
            let patch = serde_json::to_value(capture("file", before, after)).unwrap();
            assert_eq!(patch["added"], added, "{before:?} → {after:?}");
            assert_eq!(patch["removed"], removed, "{before:?} → {after:?}");
        }
    }

    #[test]
    fn large_changed_lines_have_an_explicit_utf8_safe_capture_bound() {
        let patch = capture("large", &"界".repeat(CAPTURE_BYTES), "one\ntwo\nthree\n");
        assert!(patch.truncated);
        assert!(patch.text.len() <= CAPTURE_BYTES);
        assert!(patch.text.is_char_boundary(patch.text.len()));
        // The removed line exhausts capture before any added line is formatted.
        assert!(!patch.text.contains("+one"));
        let patch = serde_json::to_value(patch).unwrap();
        assert_eq!(patch["added"], 3);
        assert_eq!(patch["removed"], 1);
    }
}
