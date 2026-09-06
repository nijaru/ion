//! /changelog (pi parity): parse `## [x.y.z]` sections from the
//! changelog file shipped with the source tree and render the latest
//! entries as markdown scrollback. Ion is a source-distributed v0
//! binary, so the changelog is embedded via `include_str!` — no
//! installed-package path resolution.

/// One parsed release section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntry {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// The section's lines, trimmed, excluding the header itself.
    pub content: String,
}

/// The changelog shipped with the source tree. Embedded: ion is a
/// source-distributed v0 binary, and `cargo install` carries the file
/// through `include_str!` where package managers would not.
pub const SOURCE: &str = include_str!("../../../CHANGELOG.md");

/// Scan `## [x.y.z] ...` headers and collect each section's body
/// until the next `##` or EOF (pi's `parseChangelog` grammar).
#[must_use]
pub fn parse(markdown: &str) -> Vec<ChangelogEntry> {
    let mut entries: Vec<ChangelogEntry> = Vec::new();
    let mut current: Option<(u32, u32, u32, Vec<&str>)> = None;
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some((major, minor, patch, lines)) = current.take()
                && !lines.is_empty()
            {
                entries.push(ChangelogEntry {
                    major,
                    minor,
                    patch,
                    content: lines.join("\n").trim().to_owned(),
                });
            }
            // pi's regex is unanchored: `##\s+\[?(\d+)\.(\d+)\.(\d+)\]?`
            // accepts suffixes like ` - 2026-01-01` after the triple.
            if let Some((major, minor, patch)) = leading_version(rest) {
                current = Some((major, minor, patch, Vec::new()));
            } else {
                current = None;
            }
        } else if let Some((_, _, _, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    if let Some((major, minor, patch, lines)) = current.take()
        && !lines.is_empty()
    {
        entries.push(ChangelogEntry {
            major,
            minor,
            patch,
            content: lines.join("\n").trim().to_owned(),
        });
    }
    entries
}

/// Extract a leading `x.y.z` from a header, skipping one `[` and
/// stopping at the first non-version character (mirrors pi's regex,
/// which ignores anything after the triple).
fn leading_version(rest: &str) -> Option<(u32, u32, u32)> {
    let mut parts: [String; 3] = [String::new(), String::new(), String::new()];
    let mut part = 0;
    for ch in rest.chars() {
        match ch {
            '[' if part == 0 && parts[0].is_empty() => {}
            '0'..='9' => parts[part].push(ch),
            '.' => {
                part += 1;
                if part > 2 {
                    return None;
                }
            }
            _ => break,
        }
    }
    if part == 2 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versioned_sections_and_ignores_unversioned_ones() {
        let markdown = "# Changelog\n\n## [1.2.3] - 2026-01-01\n\n- First thing.\n\n## Unreleased\n\n- Not a release.\n\n## 0.1.0\n\n- Initial.\n";
        let entries = parse(markdown);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            (entries[0].major, entries[0].minor, entries[0].patch),
            (1, 2, 3)
        );
        assert_eq!(entries[0].content, "- First thing.");
        assert_eq!(
            (entries[1].major, entries[1].minor, entries[1].patch),
            (0, 1, 0)
        );
        assert_eq!(entries[1].content, "- Initial.");
    }

    #[test]
    fn empty_file_yields_no_entries() {
        assert!(parse("").is_empty());
        assert!(parse("## Unreleased\n- no version\n").is_empty());
    }
}
