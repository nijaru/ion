//! File path autocomplete for @ mentions in input.

use crate::tui::fuzzy;
use crossterm::{
    cursor::MoveTo,
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType},
};
use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Maximum number of candidates to show in the popup.
const MAX_VISIBLE: usize = 7;

/// A file candidate with cached directory status.
#[derive(Debug, Clone)]
pub struct FileCandidate {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// State for file path autocomplete.
#[derive(Debug, Default)]
pub struct FileCompleter {
    /// Whether completion is active (@ detected).
    active: bool,
    /// The query text after @.
    query: String,
    /// Character index where @ starts in the buffer.
    at_position: usize,
    /// Working directory for relative paths.
    working_dir: PathBuf,
    /// All candidates (unfiltered) with cached `is_dir`.
    candidates: Vec<FileCandidate>,
    /// Filtered candidates (after fuzzy match).
    filtered: Vec<FileCandidate>,
    /// Currently selected index in filtered list.
    selected: usize,
}

impl FileCompleter {
    /// Create a new file completer with the given working directory.
    #[must_use]
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            ..Default::default()
        }
    }

    /// Update the working directory (e.g., after session resume).
    pub fn set_working_dir(&mut self, working_dir: PathBuf) {
        self.working_dir = working_dir;
        // Clear cached candidates - they'll be refreshed on next activation
        self.candidates.clear();
        self.filtered.clear();
    }

    /// Check if completion is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the current query (text after @).
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Get the position where @ starts in the buffer.
    #[must_use]
    pub fn at_position(&self) -> usize {
        self.at_position
    }

    /// Get filtered candidates for display.
    #[must_use]
    pub fn visible_candidates(&self) -> &[FileCandidate] {
        let end = self.filtered.len().min(MAX_VISIBLE);
        &self.filtered[..end]
    }

    /// Get the currently selected index.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Get the selected path if any.
    #[must_use]
    pub fn selected_path(&self) -> Option<&PathBuf> {
        self.filtered.get(self.selected).map(|c| &c.path)
    }

    /// Activate completion at the given cursor position.
    pub fn activate(&mut self, at_position: usize) {
        self.active = true;
        self.at_position = at_position;
        self.query.clear();
        self.selected = 0;
        self.refresh_candidates();
    }

    /// Deactivate completion.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.query.clear();
        self.candidates.clear();
        self.filtered.clear();
        self.selected = 0;
    }

    /// Update the query and refresh filtering.
    pub fn set_query(&mut self, query: &str) {
        // If transitioning to/from hidden file query, refresh candidates
        let was_hidden = self.query.starts_with('.');
        let is_hidden = query.starts_with('.');
        self.query = query.to_string();
        if was_hidden == is_hidden {
            self.apply_filter();
        } else {
            self.refresh_candidates();
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            let max = self.filtered.len().min(MAX_VISIBLE).saturating_sub(1);
            self.selected = (self.selected + 1).min(max);
        }
    }

    /// Render the file completion popup above the input box.
    #[allow(clippy::cast_possible_truncation)]
    pub fn render<W: Write>(&self, w: &mut W, input_start: u16, width: u16) -> std::io::Result<()> {
        let candidates = self.visible_candidates();
        if candidates.is_empty() {
            return Ok(());
        }

        let popup_height = candidates.len() as u16;
        let popup_start = input_start.saturating_sub(popup_height);

        // Calculate popup width (max path length + padding)
        let max_label_len = candidates
            .iter()
            .map(|c| c.path.to_string_lossy().len())
            .max()
            .unwrap_or(20);
        let popup_width = (max_label_len + 4).min(width.saturating_sub(4) as usize) as u16;

        for (i, candidate) in candidates.iter().enumerate() {
            let row = popup_start + i as u16;
            let is_selected = i == self.selected;
            let path_str = candidate.path.to_string_lossy();

            execute!(w, MoveTo(1, row), Clear(ClearType::CurrentLine))?;

            if is_selected {
                execute!(w, SetAttribute(Attribute::Reverse))?;
            }

            // Use cached is_dir status
            let icon = if candidate.is_dir { "󰉋 " } else { "  " };

            // Truncate path if needed (use saturating_sub for safety)
            let display_width = popup_width.saturating_sub(4) as usize;
            let display: Cow<str> = if display_width > 1 && path_str.len() > display_width {
                Cow::Owned(format!(
                    "…{}",
                    &path_str[path_str
                        .len()
                        .saturating_sub(display_width.saturating_sub(1))..]
                ))
            } else {
                path_str
            };

            execute!(
                w,
                Print(" "),
                SetForegroundColor(if candidate.is_dir {
                    Color::Blue
                } else {
                    Color::Reset
                }),
                Print(icon),
                ResetColor,
                Print(display.as_ref()),
            )?;

            // Pad to popup width
            let padding = popup_width.saturating_sub(display.len() as u16 + 3) as usize;
            if padding > 0 {
                execute!(w, Print(" ".repeat(padding)))?;
            }

            if is_selected {
                execute!(w, SetAttribute(Attribute::NoReverse))?;
            }
        }

        Ok(())
    }

    /// Refresh candidates from the filesystem.
    fn refresh_candidates(&mut self) {
        self.candidates.clear();
        // Include hidden files if query starts with '.'
        let include_hidden = self.query.starts_with('.');
        self.collect_entries(&self.working_dir.clone(), "", 0, include_hidden);
        self.apply_filter();
    }

    /// Recursively collect directory entries up to a depth limit.
    fn collect_entries(&mut self, base: &Path, prefix: &str, depth: usize, include_hidden: bool) {
        // Limit depth to avoid scanning entire filesystem
        if depth > 2 {
            return;
        }

        let Ok(entries) = std::fs::read_dir(base) else {
            return;
        };

        for entry in entries.filter_map(Result::ok) {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();

            // Skip hidden files unless query starts with '.'
            if name.starts_with('.') && !include_hidden {
                continue;
            }
            // Always skip noise directories
            if name == "node_modules" || name == "target" {
                continue;
            }

            let rel_path = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };

            let path = entry.path();
            let is_dir = path.is_dir();
            self.candidates.push(FileCandidate {
                path: PathBuf::from(&rel_path),
                is_dir,
            });

            // Recurse into directories
            if is_dir {
                self.collect_entries(&path, &rel_path, depth + 1, include_hidden);
            }
        }
    }

    /// Apply fuzzy filter to candidates.
    fn apply_filter(&mut self) {
        if self.query.is_empty() {
            // Show top-level entries when no query
            self.filtered = self
                .candidates
                .iter()
                .filter(|c| !c.path.to_string_lossy().contains('/'))
                .take(MAX_VISIBLE * 2)
                .cloned()
                .collect();
        } else {
            // Fuzzy match on query
            let candidates: Vec<&str> = self
                .candidates
                .iter()
                .map(|c| c.path.to_str().unwrap_or(""))
                .collect();
            let matches = fuzzy::top_matches(&self.query, candidates, MAX_VISIBLE * 2);
            // Look up the full FileCandidate for each match
            self.filtered = matches
                .into_iter()
                .filter_map(|m| {
                    self.candidates
                        .iter()
                        .find(|c| c.path.to_str() == Some(m))
                        .cloned()
                })
                .collect();
        }

        // Sort: directories first (using cached is_dir), then alphabetically
        self.filtered
            .sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.path.cmp(&b.path)));

        // Clamp selection
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(dir.path().join("README.md"), "").unwrap();
        dir
    }

    #[test]
    fn test_activate_deactivate() {
        let dir = setup_test_dir();
        let mut completer = FileCompleter::new(dir.path().to_path_buf());

        assert!(!completer.is_active());
        completer.activate(5);
        assert!(completer.is_active());
        assert_eq!(completer.at_position(), 5);

        completer.deactivate();
        assert!(!completer.is_active());
    }

    #[test]
    fn test_candidates_collected() {
        let dir = setup_test_dir();
        let mut completer = FileCompleter::new(dir.path().to_path_buf());

        completer.activate(0);
        assert!(!completer.visible_candidates().is_empty());
    }

    #[test]
    fn test_fuzzy_filter() {
        let dir = setup_test_dir();
        let mut completer = FileCompleter::new(dir.path().to_path_buf());

        completer.activate(0);
        completer.set_query("main");

        // Should find src/main.rs
        let candidates = completer.visible_candidates();
        assert!(
            candidates
                .iter()
                .any(|c| c.path.to_string_lossy().contains("main")),
            "Expected to find main.rs in {:?}",
            candidates
        );
    }

    #[test]
    fn test_navigation() {
        let dir = setup_test_dir();
        let mut completer = FileCompleter::new(dir.path().to_path_buf());

        completer.activate(0);
        assert_eq!(completer.selected(), 0);

        completer.move_down();
        assert_eq!(completer.selected(), 1);

        completer.move_up();
        assert_eq!(completer.selected(), 0);

        // Should not go below 0
        completer.move_up();
        assert_eq!(completer.selected(), 0);
    }
}
