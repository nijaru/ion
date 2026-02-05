//! Session picker for resuming previous sessions.

use crate::session::{SessionStore, SessionSummary};
use crate::tui::filter_input::FilterInputState;
use crate::tui::fuzzy;
use crate::tui::picker_trait::{FilterablePicker, PickerNavigation};
use crate::tui::types::SelectionState;

/// State for the session picker modal.
#[derive(Default)]
pub struct SessionPicker {
    picker: FilterablePicker<SessionSummary>,
    /// Loading state.
    pub is_loading: bool,
    /// Error message if load failed.
    pub error: Option<String>,
}

impl SessionPicker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset picker state.
    pub fn reset(&mut self) {
        self.picker.filter_input_mut().clear();
        self.apply_filter();
    }

    /// Load sessions from store.
    pub fn load_sessions(&mut self, store: &SessionStore, limit: usize) {
        self.is_loading = true;
        match store.list_recent(limit) {
            Ok(sessions) => {
                self.picker.set_items(sessions);
                self.is_loading = false;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.is_loading = false;
            }
        }
    }

    /// Check if we have sessions loaded.
    #[must_use]
    pub fn has_sessions(&self) -> bool {
        !self.picker.items().is_empty()
    }

    /// Apply filter to session list.
    pub fn apply_filter(&mut self) {
        let filter = self.picker.filter_input().text();

        if filter.is_empty() {
            self.picker.set_filtered(self.picker.items().to_vec());
        } else {
            // Build candidate strings for fuzzy matching
            let candidates: Vec<String> = self
                .picker
                .items()
                .iter()
                .map(|s| {
                    format!(
                        "{} {} {}",
                        s.id,
                        s.first_user_message.as_deref().unwrap_or(""),
                        s.working_dir
                    )
                })
                .collect();

            let candidate_refs: Vec<&str> =
                candidates.iter().map(std::string::String::as_str).collect();
            let matches = fuzzy::top_matches(filter, candidate_refs.iter().copied(), 50);

            // Map matches back to sessions
            let filtered: Vec<SessionSummary> = matches
                .iter()
                .filter_map(|matched| {
                    candidates
                        .iter()
                        .position(|c| c.as_str() == *matched)
                        .map(|idx| self.picker.items()[idx].clone())
                })
                .collect();
            self.picker.set_filtered(filtered);
        }
    }

    /// Get all sessions.
    #[must_use]
    pub fn sessions(&self) -> &[SessionSummary] {
        self.picker.items()
    }

    /// Get filtered sessions.
    #[must_use]
    pub fn filtered_sessions(&self) -> &[SessionSummary] {
        self.picker.filtered()
    }

    /// Get filter input state.
    #[must_use]
    pub fn filter_input(&self) -> &FilterInputState {
        self.picker.filter_input()
    }

    /// Get mutable filter input state.
    pub fn filter_input_mut(&mut self) -> &mut FilterInputState {
        self.picker.filter_input_mut()
    }

    /// Get list state.
    #[must_use]
    pub fn list_state(&self) -> &SelectionState {
        self.picker.list_state()
    }

    /// Get currently selected session.
    #[must_use]
    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.picker.selected()
    }
}

impl PickerNavigation for SessionPicker {
    fn move_up(&mut self, count: usize) {
        self.picker.move_up(count);
    }

    fn move_down(&mut self, count: usize) {
        self.picker.move_down(count);
    }

    fn jump_to_top(&mut self) {
        self.picker.jump_to_top();
    }

    fn jump_to_bottom(&mut self) {
        self.picker.jump_to_bottom();
    }
}
