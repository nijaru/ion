//! Session picker for resuming previous sessions.

use crate::session::{SessionStore, SessionSummary};
use crate::tui::filter_input::FilterInputState;
use crate::tui::fuzzy;
use crate::tui::types::SelectionState;

/// State for the session picker modal.
#[derive(Default)]
pub struct SessionPicker {
    /// All available sessions.
    pub sessions: Vec<SessionSummary>,
    /// Filtered sessions based on search.
    pub filtered_sessions: Vec<SessionSummary>,
    /// Filter input state.
    pub filter_input: FilterInputState,
    /// List state.
    pub list_state: SelectionState,
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
        self.filter_input.clear();
        self.apply_filter();
    }

    /// Load sessions from store.
    pub fn load_sessions(&mut self, store: &SessionStore, limit: usize) {
        self.is_loading = true;
        match store.list_recent(limit) {
            Ok(sessions) => {
                self.sessions = sessions;
                self.is_loading = false;
                self.error = None;
                self.apply_filter();
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
        !self.sessions.is_empty()
    }

    /// Apply filter to session list.
    pub fn apply_filter(&mut self) {
        let filter = self.filter_input.text();

        if filter.is_empty() {
            self.filtered_sessions = self.sessions.clone();
        } else {
            // Build candidate strings for fuzzy matching
            let candidates: Vec<String> = self
                .sessions
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

            let candidate_refs: Vec<&str> = candidates.iter().map(std::string::String::as_str).collect();
            let matches = fuzzy::top_matches(filter, candidate_refs.iter().copied(), 50);

            // Map matches back to sessions
            self.filtered_sessions = matches
                .iter()
                .filter_map(|matched| {
                    candidates
                        .iter()
                        .position(|c| c.as_str() == *matched)
                        .map(|idx| self.sessions[idx].clone())
                })
                .collect();
        }

        if self.filtered_sessions.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self, count: usize) {
        if self.filtered_sessions.is_empty() {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = i.saturating_sub(count);
        self.list_state.select(Some(new_i));
    }

    /// Move selection down.
    pub fn move_down(&mut self, count: usize) {
        if self.filtered_sessions.is_empty() {
            return;
        }
        let len = self.filtered_sessions.len();
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = (i + count).min(len - 1);
        self.list_state.select(Some(new_i));
    }

    /// Jump to top.
    pub fn jump_to_top(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.list_state
                .select(Some(self.filtered_sessions.len() - 1));
        }
    }

    /// Get currently selected session.
    #[must_use] 
    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_sessions.get(i))
    }
}
