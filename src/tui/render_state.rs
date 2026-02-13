//! Render state management for chat positioning and incremental updates.
//!
//! # Chat Positioning Modes
//!
//! The TUI uses a `ChatPosition` enum to track where chat content sits
//! relative to the terminal viewport. This replaces the previous
//! scattered `Option` fields with an explicit state machine.
//!
//! ## Row-Tracking Mode (`Tracking { next_row, .. }`)
//!
//! When chat content fits on screen, content is printed at absolute row
//! positions without scrolling. The UI follows the chat content.
//!
//! ```text
//! +-------------------+
//! | ION               | <- header
//! | v0.1.0            |
//! |                   |
//! | > hello           | <- chat starts at next_row
//! | Hi there!         |
//! |                   | <- next_row advances here
//! | ----------------  | <- UI starts at next_row
//! |  > input          |
//! | [READ] model      |
//! |                   |
//! |                   | <- empty space below
//! +-------------------+
//! ```
//!
//! ## Scroll Mode (`Scrolling { .. }`)
//!
//! When content exceeds screen height, we transition to scroll mode.
//! Content is pushed into terminal scrollback via `ScrollUp`, and the
//! UI stays at the bottom.
//!
//! ```text
//! +-------------------+ --+
//! | (scrollback)      |   | previous content
//! | (scrollback)      |   | in terminal buffer
//! +-------------------+ --+
//! | older messages    | <- visible viewport
//! | ...               |
//! | newest message    |
//! | ----------------  | <- UI at bottom
//! |  > input          |
//! | [READ] model      |
//! +-------------------+
//! ```

use crate::tui::terminal::StyledLine;

/// Tracks where the chat content sits relative to the terminal viewport.
/// Encodes the positioning mode and associated row anchors.
#[derive(Debug, Clone, Copy)]
pub enum ChatPosition {
    /// Initial state: no header printed, no chat content.
    /// UI will render at bottom of screen.
    Empty,

    /// Header has been printed. UI anchors below the header.
    /// `anchor` is the row immediately after the header lines.
    /// No chat messages exist yet.
    Header { anchor: u16 },

    /// Chat content is being placed at explicit row positions.
    /// Content fits on screen; UI follows the chat.
    /// `next_row` is where the next chat line will be printed.
    /// `ui_drawn_at` tracks where draw_direct last placed the UI top.
    Tracking {
        next_row: u16,
        ui_drawn_at: Option<u16>,
    },

    /// Chat content has overflowed the viewport.
    /// Content is pushed into scrollback via ScrollUp.
    /// UI is pinned to `term_height - ui_height`.
    /// `ui_drawn_at` tracks where draw_direct last placed the UI top.
    Scrolling { ui_drawn_at: Option<u16> },
}

impl ChatPosition {
    /// Row to place the UI when using row-tracking.
    /// Returns None in Scrolling mode (use bottom-pinned layout).
    pub fn ui_anchor(&self) -> Option<u16> {
        match self {
            Self::Empty => None,
            Self::Header { anchor } => Some(*anchor),
            Self::Tracking { next_row, .. } => Some(*next_row),
            Self::Scrolling { .. } => None,
        }
    }

    /// Previous frame's UI top row, for clear_from computation.
    pub fn last_ui_top(&self) -> Option<u16> {
        match self {
            Self::Tracking { ui_drawn_at, .. } | Self::Scrolling { ui_drawn_at } => *ui_drawn_at,
            _ => None,
        }
    }

    /// Record where draw_direct placed the UI this frame.
    pub fn set_ui_drawn_at(&mut self, row: u16) {
        match self {
            Self::Tracking { ui_drawn_at, .. } | Self::Scrolling { ui_drawn_at } => {
                *ui_drawn_at = Some(row);
            }
            Self::Empty | Self::Header { .. } => {}
        }
    }

    /// Clear the ui_drawn_at field (used when exiting selector mode).
    pub fn clear_ui_drawn_at(&mut self) {
        match self {
            Self::Tracking { ui_drawn_at, .. } | Self::Scrolling { ui_drawn_at } => {
                *ui_drawn_at = None;
            }
            Self::Empty | Self::Header { .. } => {}
        }
    }

    /// Whether the header has been printed.
    pub fn header_inserted(&self) -> bool {
        !matches!(self, Self::Empty)
    }

    /// Whether we are in row-tracking mode.
    pub fn is_tracking(&self) -> bool {
        matches!(self, Self::Tracking { .. })
    }

    /// How many rows to scroll to push visible content into scrollback.
    /// In Scrolling mode, the full viewport is used; in other modes,
    /// only rows with known content are counted.
    pub fn scroll_amount(&self, ui_height: u16, term_height: u16) -> u16 {
        match self {
            Self::Scrolling { .. } => term_height,
            Self::Tracking {
                next_row,
                ui_drawn_at,
            } => {
                // Content up to next_row, UI drawn at ui_drawn_at
                let content_bottom = ui_drawn_at
                    .map(|row| row.saturating_add(ui_height))
                    .unwrap_or(next_row.saturating_add(ui_height));
                content_bottom.min(term_height)
            }
            Self::Header { anchor } => anchor.saturating_add(ui_height).min(term_height),
            Self::Empty => 0,
        }
    }
}

/// Manages render state for chat positioning and incremental updates.
pub struct RenderState {
    /// Position state machine (replaces chat_row, startup_ui_anchor,
    /// last_ui_start, header_inserted).
    pub position: ChatPosition,

    /// Number of chat entries already inserted into scrollback.
    pub rendered_entries: usize,

    /// Buffered chat lines while selector is open.
    pub buffered_chat_lines: Vec<StyledLine>,

    /// Lines from the streaming agent entry already committed to scrollback.
    /// Reset when the entry finishes, a tool call interrupts, or reflow occurs.
    pub streaming_lines_rendered: usize,
    /// Wrap width used when `streaming_lines_rendered` was computed.
    /// Prevents line-count carryover across width changes.
    pub streaming_wrap_width: Option<usize>,

    /// Flag to clear visible screen (e.g., /clear command).
    pub needs_screen_clear: bool,
    /// Flag to repaint visible chat from canonical entries.
    /// Used for explicit full redraw flows (e.g., session load).
    pub needs_reflow: bool,
    /// Flag to clear selector area without full screen repaint.
    pub needs_selector_clear: bool,
    /// Last top row where any bottom UI variant was rendered.
    /// Used for clear-from calculations when position state is Empty/Header.
    pub last_ui_top: Option<u16>,
    /// Row to clear from when leaving selector mode.
    /// Captured before mode switch so stale selector rows are removed safely.
    pub selector_clear_from: Option<u16>,
    /// Last top row where selector UI was rendered.
    /// Used as fallback when position state cannot provide a prior UI top.
    pub last_selector_top: Option<u16>,

    /// Force a render on the first frame after session load or /clear.
    pub needs_initial_render: bool,
}

impl RenderState {
    /// Create a new `RenderState` with default values.
    pub fn new() -> Self {
        Self {
            position: ChatPosition::Empty,
            rendered_entries: 0,
            buffered_chat_lines: Vec::new(),
            streaming_lines_rendered: 0,
            streaming_wrap_width: None,
            needs_screen_clear: false,
            needs_reflow: false,
            needs_selector_clear: false,
            last_ui_top: None,
            selector_clear_from: None,
            last_selector_top: None,
            needs_initial_render: false,
        }
    }

    /// Reset for /clear command (new conversation).
    ///
    /// Resets state for starting a fresh conversation, including
    /// re-showing the startup header.
    pub fn reset_for_new_conversation(&mut self) {
        self.position = ChatPosition::Empty;
        self.rendered_entries = 0;
        self.buffered_chat_lines.clear();
        self.streaming_lines_rendered = 0;
        self.streaming_wrap_width = None;
        self.last_ui_top = None;
        self.selector_clear_from = None;
        self.last_selector_top = None;
        self.needs_initial_render = true;
    }

    /// Reset for loading existing session (resume/load).
    ///
    /// Prepares state for displaying a loaded session's chat history.
    /// Position is preserved so the next frame can compute an accurate
    /// reflow scroll amount from the currently visible viewport.
    pub fn reset_for_session_load(&mut self) {
        self.rendered_entries = 0;
        self.buffered_chat_lines.clear();
        self.streaming_lines_rendered = 0;
        self.streaming_wrap_width = None;
        self.selector_clear_from = None;
        self.last_selector_top = None;
        self.needs_initial_render = true;
    }

    /// Record where bottom UI was rendered this frame.
    pub fn note_ui_top(&mut self, row: u16) {
        self.last_ui_top = Some(row);
        self.position.set_ui_drawn_at(row);
    }

    /// Last known top row for bottom UI clearing.
    #[must_use]
    pub fn last_ui_top(&self) -> Option<u16> {
        self.position.last_ui_top().or(self.last_ui_top)
    }

    /// Record where selector UI was last rendered.
    pub fn note_selector_top(&mut self, row: u16) {
        self.last_selector_top = Some(row);
    }

    /// Queue selector-area clearing from a captured row.
    pub fn queue_selector_clear(&mut self, from_row: Option<u16>) {
        self.selector_clear_from = from_row.or(self.last_selector_top);
        self.last_selector_top = None;
        self.needs_selector_clear = true;
    }

    /// Cancel a pending selector-area clear.
    pub fn cancel_selector_clear(&mut self) {
        self.needs_selector_clear = false;
        self.selector_clear_from = None;
        self.last_selector_top = None;
    }

    /// Resolve and consume pending selector clear row.
    /// Falls back to the provided row when none was captured.
    #[must_use]
    pub fn take_selector_clear_from(&mut self, fallback_row: u16) -> u16 {
        self.selector_clear_from.take().unwrap_or(fallback_row)
    }

    /// After printing `line_count` lines from row 0, set position based on
    /// whether content fits on screen or overflows.
    /// Returns scroll-up amount needed (0 if content fits).
    #[allow(clippy::cast_possible_truncation)]
    pub fn position_after_reprint(
        &mut self,
        line_count: usize,
        term_height: u16,
        ui_height: u16,
    ) -> u16 {
        let available = term_height.saturating_sub(ui_height) as usize;
        if line_count <= available {
            self.position = ChatPosition::Tracking {
                next_row: line_count as u16,
                ui_drawn_at: None,
            };
            0
        } else {
            let excess = (line_count
                .min(term_height as usize)
                .saturating_sub(available)) as u16;
            self.position = ChatPosition::Scrolling { ui_drawn_at: None };
            excess
        }
    }

    /// Mark reflow as complete after `reprint_chat_scrollback`.
    ///
    /// Updates state to reflect that all entries have been rendered.
    /// Position is already set by `position_after_reprint`.
    pub fn mark_reflow_complete(&mut self, entries: usize) {
        self.rendered_entries = entries;
        self.buffered_chat_lines.clear();
        self.streaming_lines_rendered = 0;
        self.streaming_wrap_width = None;
    }
}

impl Default for RenderState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_after_reprint_fits_on_screen() {
        let mut state = RenderState::new();
        let excess = state.position_after_reprint(5, 40, 6);
        assert_eq!(excess, 0);
        assert!(matches!(
            state.position,
            ChatPosition::Tracking {
                next_row: 5,
                ui_drawn_at: None
            }
        ));
    }

    #[test]
    fn position_after_reprint_overflows() {
        let mut state = RenderState::new();
        // 40 lines of content, terminal 40 tall, UI 6 tall -> 34 available, 6 excess
        let excess = state.position_after_reprint(40, 40, 6);
        assert_eq!(excess, 6);
        assert!(matches!(
            state.position,
            ChatPosition::Scrolling { ui_drawn_at: None }
        ));
    }

    #[test]
    fn position_after_reprint_exact_fit() {
        let mut state = RenderState::new();
        // Exactly fills available space
        let excess = state.position_after_reprint(34, 40, 6);
        assert_eq!(excess, 0);
        assert!(matches!(
            state.position,
            ChatPosition::Tracking {
                next_row: 34,
                ui_drawn_at: None
            }
        ));
    }

    #[test]
    fn position_after_reprint_content_exceeds_terminal() {
        let mut state = RenderState::new();
        // More lines than terminal height (capped to term_height)
        let excess = state.position_after_reprint(100, 40, 6);
        // min(100, 40) - 34 = 6
        assert_eq!(excess, 6);
        assert!(matches!(
            state.position,
            ChatPosition::Scrolling { ui_drawn_at: None }
        ));
    }

    #[test]
    fn needs_initial_render_set_on_reset() {
        let mut state = RenderState::new();
        assert!(!state.needs_initial_render);

        state.note_ui_top(9);
        state.reset_for_new_conversation();
        assert!(state.needs_initial_render);
        assert_eq!(state.last_ui_top, None);

        state.needs_initial_render = false;
        state.reset_for_session_load();
        assert!(state.needs_initial_render);
        assert_eq!(state.last_ui_top, None);
    }

    #[test]
    fn reset_for_session_load_preserves_position_and_clears_buffers() {
        let mut state = RenderState::new();
        state.position = ChatPosition::Tracking {
            next_row: 12,
            ui_drawn_at: Some(12),
        };
        state.rendered_entries = 7;
        state.buffered_chat_lines.push(StyledLine::raw("buffered"));
        state.streaming_lines_rendered = 3;
        state.selector_clear_from = Some(5);
        state.last_selector_top = Some(9);
        state.needs_initial_render = false;

        state.reset_for_session_load();

        assert!(matches!(
            state.position,
            ChatPosition::Tracking {
                next_row: 12,
                ui_drawn_at: Some(12)
            }
        ));
        assert_eq!(state.rendered_entries, 0);
        assert!(state.buffered_chat_lines.is_empty());
        assert_eq!(state.streaming_lines_rendered, 0);
        assert_eq!(state.selector_clear_from, None);
        assert_eq!(state.last_selector_top, None);
        assert!(state.needs_initial_render);
    }

    #[test]
    fn selector_clear_queue_take_cancel() {
        let mut state = RenderState::new();
        state.note_selector_top(7);
        state.queue_selector_clear(Some(9));
        assert!(state.needs_selector_clear);
        assert_eq!(state.selector_clear_from, Some(9));
        assert_eq!(state.last_selector_top, None);
        assert_eq!(state.take_selector_clear_from(20), 9);
        assert_eq!(state.selector_clear_from, None);

        state.note_selector_top(8);
        state.queue_selector_clear(None);
        assert_eq!(state.selector_clear_from, Some(8));
        assert_eq!(state.take_selector_clear_from(20), 8);
        state.cancel_selector_clear();
        assert!(!state.needs_selector_clear);
        assert_eq!(state.selector_clear_from, None);
        assert_eq!(state.last_selector_top, None);
    }

    #[test]
    fn render_state_last_ui_top_falls_back_for_empty_and_header() {
        let mut state = RenderState::new();
        state.note_ui_top(11);
        assert_eq!(state.last_ui_top(), Some(11));

        state.position = ChatPosition::Header { anchor: 3 };
        assert_eq!(state.last_ui_top(), Some(11));

        state.position = ChatPosition::Tracking {
            next_row: 5,
            ui_drawn_at: Some(8),
        };
        assert_eq!(state.last_ui_top(), Some(8));
    }

    #[test]
    fn chat_position_ui_anchor() {
        assert_eq!(ChatPosition::Empty.ui_anchor(), None);
        assert_eq!(ChatPosition::Header { anchor: 3 }.ui_anchor(), Some(3));
        assert_eq!(
            ChatPosition::Tracking {
                next_row: 10,
                ui_drawn_at: None
            }
            .ui_anchor(),
            Some(10)
        );
        assert_eq!(
            ChatPosition::Scrolling { ui_drawn_at: None }.ui_anchor(),
            None
        );
    }

    #[test]
    fn chat_position_last_ui_top() {
        assert_eq!(ChatPosition::Empty.last_ui_top(), None);
        assert_eq!(ChatPosition::Header { anchor: 3 }.last_ui_top(), None);
        assert_eq!(
            ChatPosition::Tracking {
                next_row: 10,
                ui_drawn_at: Some(10)
            }
            .last_ui_top(),
            Some(10)
        );
        assert_eq!(
            ChatPosition::Scrolling {
                ui_drawn_at: Some(30)
            }
            .last_ui_top(),
            Some(30)
        );
    }

    #[test]
    fn chat_position_set_ui_drawn_at() {
        let mut pos = ChatPosition::Tracking {
            next_row: 5,
            ui_drawn_at: None,
        };
        pos.set_ui_drawn_at(10);
        assert_eq!(pos.last_ui_top(), Some(10));

        let mut pos = ChatPosition::Scrolling { ui_drawn_at: None };
        pos.set_ui_drawn_at(30);
        assert_eq!(pos.last_ui_top(), Some(30));

        // Empty and Header do nothing
        let mut pos = ChatPosition::Empty;
        pos.set_ui_drawn_at(5);
        assert_eq!(pos.last_ui_top(), None);
    }

    #[test]
    fn chat_position_header_inserted() {
        assert!(!ChatPosition::Empty.header_inserted());
        assert!(ChatPosition::Header { anchor: 3 }.header_inserted());
        assert!(
            ChatPosition::Tracking {
                next_row: 5,
                ui_drawn_at: None
            }
            .header_inserted()
        );
        assert!(ChatPosition::Scrolling { ui_drawn_at: None }.header_inserted());
    }

    #[test]
    fn chat_position_scroll_amount() {
        // Scrolling: full terminal
        assert_eq!(
            ChatPosition::Scrolling { ui_drawn_at: None }.scroll_amount(5, 40),
            40
        );
        // Tracking: content rows + UI height
        assert_eq!(
            ChatPosition::Tracking {
                next_row: 10,
                ui_drawn_at: Some(10)
            }
            .scroll_amount(5, 40),
            15 // ui_drawn_at(10) + ui_height(5) = 15
        );
        // Tracking without ui_drawn_at: next_row + UI height
        assert_eq!(
            ChatPosition::Tracking {
                next_row: 10,
                ui_drawn_at: None
            }
            .scroll_amount(5, 40),
            15
        );
        // Header: anchor + UI height
        assert_eq!(ChatPosition::Header { anchor: 3 }.scroll_amount(5, 40), 8);
        // Empty: no known content rows
        assert_eq!(ChatPosition::Empty.scroll_amount(5, 40), 0);
    }

    #[test]
    fn chat_position_clear_ui_drawn_at() {
        let mut pos = ChatPosition::Tracking {
            next_row: 5,
            ui_drawn_at: Some(10),
        };
        pos.clear_ui_drawn_at();
        assert_eq!(pos.last_ui_top(), None);

        let mut pos = ChatPosition::Scrolling {
            ui_drawn_at: Some(30),
        };
        pos.clear_ui_drawn_at();
        assert_eq!(pos.last_ui_top(), None);
    }
}
