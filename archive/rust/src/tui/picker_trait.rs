//! Common trait and generic implementation for filterable pickers.

use crate::tui::filter_input::FilterInputState;
use crate::tui::types::SelectionState;

/// Common navigation interface for pickers.
pub trait PickerNavigation {
    /// Move selection up by count items.
    fn move_up(&mut self, count: usize);
    /// Move selection down by count items.
    fn move_down(&mut self, count: usize);
    /// Jump to the first item.
    fn jump_to_top(&mut self);
    /// Jump to the last item.
    fn jump_to_bottom(&mut self);
}

/// Generic filterable picker with fuzzy search support.
///
/// Provides common navigation and filtering logic for picker modals.
/// Each picker wraps this and provides its own filter matching function.
#[derive(Debug, Clone)]
pub struct FilterablePicker<T: Clone> {
    /// All items.
    items: Vec<T>,
    /// Filtered items after search.
    filtered: Vec<T>,
    /// Filter input state.
    filter_input: FilterInputState,
    /// List selection state.
    list_state: SelectionState,
}

impl<T: Clone> Default for FilterablePicker<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            filtered: Vec::new(),
            filter_input: FilterInputState::default(),
            list_state: SelectionState::default(),
        }
    }
}

impl<T: Clone> FilterablePicker<T> {
    /// Set the items and reset filter.
    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = items;
        self.filter_input.clear();
        self.filtered = self.items.clone();
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Get all items.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Get filtered items.
    #[must_use]
    pub fn filtered(&self) -> &[T] {
        &self.filtered
    }

    /// Get filter input state.
    #[must_use]
    pub fn filter_input(&self) -> &FilterInputState {
        &self.filter_input
    }

    /// Get mutable filter input state.
    pub fn filter_input_mut(&mut self) -> &mut FilterInputState {
        &mut self.filter_input
    }

    /// Get list state.
    #[must_use]
    pub fn list_state(&self) -> &SelectionState {
        &self.list_state
    }

    /// Apply filter using the provided matcher function.
    ///
    /// The matcher receives each item and the filter text, returning true if the item matches.
    pub fn apply_filter(&mut self, matcher: impl Fn(&T, &str) -> bool) {
        let filter = self.filter_input.text();

        if filter.is_empty() {
            self.filtered = self.items.clone();
        } else {
            self.filtered = self
                .items
                .iter()
                .filter(|item| matcher(item, filter))
                .cloned()
                .collect();
        }

        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Apply filter with pre-computed filtered results (for complex matching).
    pub fn set_filtered(&mut self, filtered: Vec<T>) {
        self.filtered = filtered;
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Move selection up by count.
    pub fn move_up(&mut self, count: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = i.saturating_sub(count);
        self.list_state.select(Some(new_i));
    }

    /// Move selection down by count.
    pub fn move_down(&mut self, count: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len();
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = (i + count).min(len - 1);
        self.list_state.select(Some(new_i));
    }

    /// Jump to first item.
    pub fn jump_to_top(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Jump to last item.
    pub fn jump_to_bottom(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(self.filtered.len() - 1));
        }
    }

    /// Get currently selected item.
    #[must_use]
    pub fn selected(&self) -> Option<&T> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
    }

    /// Find and select an item by predicate.
    pub fn select_by(&mut self, predicate: impl Fn(&T) -> bool) {
        if let Some(idx) = self.filtered.iter().position(predicate) {
            self.list_state.select(Some(idx));
        }
    }
}

impl<T: Clone> PickerNavigation for FilterablePicker<T> {
    fn move_up(&mut self, count: usize) {
        Self::move_up(self, count);
    }

    fn move_down(&mut self, count: usize) {
        Self::move_down(self, count);
    }

    fn jump_to_top(&mut self) {
        Self::jump_to_top(self);
    }

    fn jump_to_bottom(&mut self) {
        Self::jump_to_bottom(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_picker_navigation() {
        let mut picker: FilterablePicker<String> = FilterablePicker::default();
        picker.set_items(vec!["a".into(), "b".into(), "c".into()]);

        assert_eq!(picker.selected(), Some(&"a".to_string()));

        picker.move_down(1);
        assert_eq!(picker.selected(), Some(&"b".to_string()));

        picker.move_down(10); // Clamps to end
        assert_eq!(picker.selected(), Some(&"c".to_string()));

        picker.move_up(1);
        assert_eq!(picker.selected(), Some(&"b".to_string()));

        picker.jump_to_bottom();
        assert_eq!(picker.selected(), Some(&"c".to_string()));

        picker.jump_to_top();
        assert_eq!(picker.selected(), Some(&"a".to_string()));
    }

    #[test]
    fn test_picker_filter() {
        let mut picker: FilterablePicker<String> = FilterablePicker::default();
        picker.set_items(vec!["apple".into(), "banana".into(), "apricot".into()]);

        picker.filter_input_mut().set_text("ap");
        picker.apply_filter(|item, filter| item.contains(filter));

        assert_eq!(picker.filtered().len(), 2);
        assert_eq!(picker.selected(), Some(&"apple".to_string()));
    }

    #[test]
    fn test_picker_empty() {
        let mut picker: FilterablePicker<String> = FilterablePicker::default();
        picker.set_items(vec![]);

        assert_eq!(picker.selected(), None);
        picker.move_down(1); // Should not panic
        picker.jump_to_top(); // Should not panic
    }

    #[test]
    fn test_picker_select_by() {
        let mut picker: FilterablePicker<String> = FilterablePicker::default();
        picker.set_items(vec!["a".into(), "b".into(), "c".into()]);

        picker.select_by(|s| s == "b");
        assert_eq!(picker.selected(), Some(&"b".to_string()));
    }
}
