//! API Provider picker modal.
//!
//! Allows selecting the API provider (Anthropic, `OpenRouter`, etc.)
//! with visual indication of authentication status.

use crate::provider::ProviderStatus;
use crate::tui::filter_input::FilterInputState;
use crate::tui::picker_trait::{FilterablePicker, PickerNavigation};
use crate::tui::types::SelectionState;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

/// State for the API provider picker modal.
#[derive(Default)]
pub struct ProviderPicker {
    picker: FilterablePicker<ProviderStatus>,
}

impl ProviderPicker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh provider detection and reset selection.
    pub fn refresh(&mut self) {
        let providers = ProviderStatus::sorted(ProviderStatus::detect_all());
        self.picker.set_items(providers);
    }

    /// Apply filter to provider list.
    pub fn apply_filter(&mut self) {
        let matcher = SkimMatcherV2::default().ignore_case();
        self.picker.apply_filter(|p, filter| {
            matcher.fuzzy_match(p.provider.name(), filter).is_some()
                || matcher
                    .fuzzy_match(p.provider.description(), filter)
                    .is_some()
        });
    }

    /// Get all providers.
    #[must_use]
    pub fn providers(&self) -> &[ProviderStatus] {
        self.picker.items()
    }

    /// Get filtered providers.
    #[must_use]
    pub fn filtered(&self) -> &[ProviderStatus] {
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

    /// Get currently selected provider.
    #[must_use]
    pub fn selected(&self) -> Option<&ProviderStatus> {
        self.picker.selected()
    }

    /// Select a specific provider by enum value.
    pub fn select_provider(&mut self, provider: crate::provider::Provider) {
        self.picker.select_by(|s| s.provider == provider);
    }
}

impl PickerNavigation for ProviderPicker {
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
