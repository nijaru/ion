//! Common state for autocomplete popups.

/// Generic completer state that can be embedded in domain-specific completers.
///
/// Handles active state, query text, filtered candidates, selection, and navigation.
/// Each completer wraps this and provides its own filtering logic.
#[derive(Debug, Clone)]
pub struct CompleterState<T: Clone> {
    /// Whether completion is active.
    active: bool,
    /// The query text.
    query: String,
    /// Filtered candidates after search.
    filtered: Vec<T>,
    /// Currently selected index in filtered list.
    selected: usize,
    /// Maximum visible items in popup.
    max_visible: usize,
}

impl<T: Clone> CompleterState<T> {
    /// Create a new completer state with the given max visible items.
    #[must_use]
    pub fn new(max_visible: usize) -> Self {
        Self {
            active: false,
            query: String::new(),
            filtered: Vec::new(),
            selected: 0,
            max_visible,
        }
    }

    /// Check if completion is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the current query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Activate completion.
    pub fn activate(&mut self) {
        self.active = true;
        self.query.clear();
        self.selected = 0;
    }

    /// Deactivate completion and clear state.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.query.clear();
        self.filtered.clear();
        self.selected = 0;
    }

    /// Set the query text. Does not apply filtering - caller must do that.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
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
            let max = self.filtered.len().min(self.max_visible).saturating_sub(1);
            self.selected = (self.selected + 1).min(max);
        }
    }

    /// Get the currently selected index.
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Get the selected item if any.
    #[must_use]
    pub fn selected(&self) -> Option<&T> {
        self.filtered.get(self.selected)
    }

    /// Get visible candidates (up to max_visible).
    #[must_use]
    pub fn visible_candidates(&self) -> &[T] {
        let end = self.filtered.len().min(self.max_visible);
        &self.filtered[..end]
    }

    /// Set filtered candidates and clamp selection.
    pub fn set_filtered(&mut self, filtered: Vec<T>) {
        self.filtered = filtered;
        self.clamp_selection();
    }

    /// Clamp selection to valid range.
    pub fn clamp_selection(&mut self) {
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activate_deactivate() {
        let mut state: CompleterState<String> = CompleterState::new(7);

        assert!(!state.is_active());
        state.activate();
        assert!(state.is_active());
        assert!(state.query().is_empty());

        state.deactivate();
        assert!(!state.is_active());
    }

    #[test]
    fn test_navigation() {
        let mut state: CompleterState<String> = CompleterState::new(7);
        state.activate();
        state.set_filtered(vec!["a".into(), "b".into(), "c".into()]);

        assert_eq!(state.selected_index(), 0);
        assert_eq!(state.selected(), Some(&"a".to_string()));

        state.move_down();
        assert_eq!(state.selected_index(), 1);

        state.move_up();
        assert_eq!(state.selected_index(), 0);

        // Should not go below 0
        state.move_up();
        assert_eq!(state.selected_index(), 0);
    }

    #[test]
    fn test_visible_candidates() {
        let mut state: CompleterState<i32> = CompleterState::new(3);
        state.set_filtered(vec![1, 2, 3, 4, 5]);

        // Only max_visible (3) items shown, even though 5 are filtered
        assert_eq!(state.visible_candidates(), &[1, 2, 3]);
    }

    #[test]
    fn test_clamp_selection() {
        let mut state: CompleterState<String> = CompleterState::new(7);
        state.set_filtered(vec!["a".into(), "b".into(), "c".into()]);
        state.move_down();
        state.move_down(); // selected = 2

        // Shrink filtered list
        state.set_filtered(vec!["a".into()]);
        assert_eq!(state.selected_index(), 0); // Clamped
    }

    #[test]
    fn test_empty() {
        let mut state: CompleterState<String> = CompleterState::new(7);
        state.activate();

        assert_eq!(state.selected(), None);
        state.move_down(); // Should not panic
        state.move_up(); // Should not panic
    }
}
