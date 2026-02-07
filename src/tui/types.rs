//! TUI type definitions: enums, structs, and constants.

use std::time::Duration;

/// Window duration for double-tap cancel/quit detection.
pub(super) const CANCEL_WINDOW: Duration = Duration::from_millis(1500);

/// Number of lines to show in queued message preview.
pub(crate) const QUEUED_PREVIEW_LINES: usize = 5;

/// Thinking budget level for extended reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingLevel {
    /// No extended thinking (default)
    #[default]
    Off,
    /// Standard budget (4k tokens)
    Standard,
    /// Extended budget (16k tokens)
    Extended,
}

impl ThinkingLevel {
    /// Cycle to the next level
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Standard,
            Self::Standard => Self::Extended,
            Self::Extended => Self::Off,
        }
    }

    /// Get the token budget for this level, None if Off
    #[must_use]
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Standard => Some(4096),
            Self::Extended => Some(16384),
        }
    }

    /// Display label for the status line (empty string when off)
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::Standard => "[think:4k]",
            Self::Extended => "[think:16k]",
        }
    }
}

/// Modal states for the TUI. The default is Input (no mode switching required).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Standard input mode (always active unless a modal is open)
    #[default]
    Input,
    /// Bottom-anchored selector shell (provider/model)
    Selector,
    /// Keybinding help overlay (Ctrl+H)
    HelpOverlay,
    /// Ctrl+R history search
    HistorySearch,
}

/// State for Ctrl+R history search.
#[derive(Debug, Clone, Default)]
pub struct HistorySearchState {
    /// Current search query
    pub query: String,
    /// Filtered history entries (indices into input_history)
    pub matches: Vec<usize>,
    /// Currently selected match index
    pub selected: usize,
}

impl HistorySearchState {
    /// Create a new history search state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Update matches based on query and history.
    pub fn update_matches(&mut self, history: &[String]) {
        let query_lower = self.query.to_lowercase();
        self.matches = history
            .iter()
            .enumerate()
            .rev() // Most recent first
            .filter(|(_, entry)| entry.to_lowercase().contains(&query_lower))
            .map(|(i, _)| i)
            .collect();
        // Reset selection if out of bounds
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }

    /// Get the currently selected history entry index, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    /// Move selection up (to older entries in filtered list).
    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() && self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Move selection down (to newer entries in filtered list).
    pub fn select_next(&mut self) {
        if !self.matches.is_empty() && self.selected + 1 < self.matches.len() {
            self.selected += 1;
        }
    }

    /// Clear the search state.
    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.selected = 0;
    }
}

/// Active page within the selector shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorPage {
    Provider,
    Model,
    Session,
}

/// Simple list selection state (replaces `ratatui::widgets::ListState`).
#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    selected: Option<usize>,
}

impl SelectionState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }
}

/// Summary of a completed task for post-completion display.
#[derive(Clone)]
pub struct TaskSummary {
    pub elapsed: std::time::Duration,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub cost: f64,
    pub was_cancelled: bool,
}
