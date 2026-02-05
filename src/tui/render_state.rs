//! Render state management for chat positioning and incremental updates.
//!
//! # Chat Positioning Modes
//!
//! The TUI uses two positioning modes for chat content:
//!
//! ## Row-Tracking Mode (`chat_row = Some(row)`)
//!
//! When chat content fits on screen, content is printed at absolute row
//! positions without scrolling. The UI follows the chat content.
//!
//! ```text
//! ┌─────────────────┐
//! │ ION             │ ← header
//! │ v0.1.0          │
//! │                 │
//! │ > hello         │ ← chat starts at chat_row
//! │ Hi there!       │
//! │                 │ ← chat_row advances here
//! │ ──────────────  │ ← UI starts at chat_row
//! │  > input        │
//! │ [READ] model    │
//! │                 │
//! │                 │ ← empty space below
//! └─────────────────┘
//! ```
//!
//! ## Scroll Mode (`chat_row = None`)
//!
//! When content exceeds screen height, we transition to scroll mode.
//! Content is pushed into terminal scrollback via `ScrollUp`, and the
//! UI stays at the bottom.
//!
//! ```text
//! ┌─────────────────┐ ─┐
//! │ (scrollback)    │  │ previous content
//! │ (scrollback)    │  │ in terminal buffer
//! ├─────────────────┤ ─┘
//! │ older messages  │ ← visible viewport
//! │ ...             │
//! │ newest message  │
//! │ ──────────────  │ ← UI at bottom
//! │  > input        │
//! │ [READ] model    │
//! └─────────────────┘
//! ```

use crate::tui::terminal::StyledLine;

/// Manages render state for chat positioning and incremental updates.
///
/// This struct centralizes all render-related state that was previously
/// scattered across the App struct, making reset logic clearer and more
/// consistent.
pub struct RenderState {
    /// Number of chat entries already inserted into scrollback.
    pub rendered_entries: usize,

    /// Buffered chat lines while selector is open.
    pub buffered_chat_lines: Vec<StyledLine>,

    /// Whether the startup header has been inserted into scrollback.
    pub header_inserted: bool,

    /// Current chat row position for row-tracking mode.
    /// - `None`: Scroll mode (content pushed into scrollback)
    /// - `Some(row)`: Row-tracking mode (chat fits on screen, print at row)
    pub chat_row: Option<u16>,

    /// UI anchor row for startup before first message (keeps UI near header).
    pub startup_ui_anchor: Option<u16>,

    /// Last UI start row for detecting changes that need extra clearing.
    pub last_ui_start: Option<u16>,

    /// Flag to clear visible screen (e.g., /clear command).
    pub needs_screen_clear: bool,
    /// Flag to clear selector area without full screen repaint.
    pub needs_selector_clear: bool,
}

impl RenderState {
    /// Create a new `RenderState` with default values.
    pub fn new() -> Self {
        Self {
            rendered_entries: 0,
            buffered_chat_lines: Vec::new(),
            header_inserted: false,
            chat_row: None,
            startup_ui_anchor: None,
            last_ui_start: None,
            needs_screen_clear: false,
            needs_selector_clear: false,
        }
    }

    /// Reset for /clear command (new conversation).
    ///
    /// Resets state for starting a fresh conversation, including
    /// re-showing the startup header.
    pub fn reset_for_new_conversation(&mut self) {
        self.rendered_entries = 0;
        self.buffered_chat_lines.clear();
        self.header_inserted = false;
        self.chat_row = None;
        self.last_ui_start = None;
    }

    /// Reset for loading existing session (resume/load).
    ///
    /// Prepares state for displaying a loaded session's chat history.
    pub fn reset_for_session_load(&mut self) {
        self.rendered_entries = 0;
        self.buffered_chat_lines.clear();
        self.startup_ui_anchor = None;
        self.chat_row = None;
        self.last_ui_start = None;
    }

    /// Mark reflow as complete after `reprint_chat_scrollback`.
    ///
    /// Updates state to reflect that all entries have been rendered.
    pub fn mark_reflow_complete(&mut self, entries: usize) {
        self.rendered_entries = entries;
        self.header_inserted = true;
        self.buffered_chat_lines.clear();
    }
}

impl Default for RenderState {
    fn default() -> Self {
        Self::new()
    }
}
