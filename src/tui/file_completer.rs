//! File path autocomplete for @ mentions in input.

use crate::tui::fuzzy;
use std::path::{Path, PathBuf};

/// Maximum number of candidates to show in the popup.
const MAX_VISIBLE: usize = 7;

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
    /// All candidates (unfiltered).
    candidates: Vec<PathBuf>,
    /// Filtered candidates (after fuzzy match).
    filtered: Vec<PathBuf>,
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
    pub fn visible_candidates(&self) -> &[PathBuf] {
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
        self.filtered.get(self.selected)
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
        self.query = query.to_string();
        self.apply_filter();
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

    /// Refresh candidates from the filesystem.
    fn refresh_candidates(&mut self) {
        self.candidates.clear();
        self.collect_entries(&self.working_dir.clone(), "", 0);
        self.apply_filter();
    }

    /// Recursively collect directory entries up to a depth limit.
    fn collect_entries(&mut self, base: &Path, prefix: &str, depth: usize) {
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

            // Skip hidden files and common noise
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }

            let rel_path = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };

            let path = entry.path();
            self.candidates.push(PathBuf::from(&rel_path));

            // Recurse into directories
            if path.is_dir() {
                self.collect_entries(&path, &rel_path, depth + 1);
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
                .filter(|p| !p.to_string_lossy().contains('/'))
                .take(MAX_VISIBLE * 2)
                .cloned()
                .collect();
        } else {
            // Fuzzy match on query
            let candidates: Vec<&str> = self
                .candidates
                .iter()
                .map(|p| p.to_str().unwrap_or(""))
                .collect();
            let matches = fuzzy::top_matches(&self.query, candidates, MAX_VISIBLE * 2);
            self.filtered = matches.into_iter().map(PathBuf::from).collect();
        }

        // Sort: directories first, then alphabetically
        self.filtered.sort_by(|a, b| {
            let a_is_dir = self.working_dir.join(a).is_dir();
            let b_is_dir = self.working_dir.join(b).is_dir();
            b_is_dir.cmp(&a_is_dir).then_with(|| a.cmp(b))
        });

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
                .any(|p| p.to_string_lossy().contains("main")),
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
