//! API Provider picker modal.
//!
//! Allows selecting the API provider (Anthropic, OpenRouter, etc.)
//! with visual indication of authentication status.

use crate::provider::ProviderStatus;
use crate::tui::filter_input::FilterInputState;
use crate::tui::types::SelectionState;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

/// State for the API provider picker modal.
#[derive(Default)]
pub struct ProviderPicker {
    /// All provider statuses (detected on open).
    pub providers: Vec<ProviderStatus>,
    /// List selection state.
    pub list_state: SelectionState,
    /// Filter input state for type-to-filter.
    pub filter_input: FilterInputState,
    /// Filtered providers based on search.
    pub filtered: Vec<ProviderStatus>,
}

impl ProviderPicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh provider detection and reset selection.
    pub fn refresh(&mut self) {
        self.providers = ProviderStatus::sorted(ProviderStatus::detect_all());
        self.filter_input.clear();
        self.apply_filter();
    }

    /// Apply filter to provider list.
    pub fn apply_filter(&mut self) {
        let matcher = SkimMatcherV2::default().ignore_case();
        let filter = self.filter_input.text();

        self.filtered = self
            .providers
            .iter()
            .filter(|p| {
                if filter.is_empty() {
                    return true;
                }
                matcher.fuzzy_match(p.provider.name(), filter).is_some()
                    || matcher
                        .fuzzy_match(p.provider.description(), filter)
                        .is_some()
            })
            .cloned()
            .collect();

        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self, count: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = i.saturating_sub(count);
        self.list_state.select(Some(new_i));
    }

    /// Move selection down.
    pub fn move_down(&mut self, count: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len();
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = (i + count).min(len - 1);
        self.list_state.select(Some(new_i));
    }

    /// Jump to top.
    pub fn jump_to_top(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(self.filtered.len() - 1));
        }
    }

    /// Get currently selected provider.
    pub fn selected(&self) -> Option<&ProviderStatus> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
    }

    /// Select a specific provider by enum value.
    pub fn select_provider(&mut self, provider: crate::provider::Provider) {
        if let Some(idx) = self.filtered.iter().position(|s| s.provider == provider) {
            self.list_state.select(Some(idx));
        }
    }
}
