//! Layout calculations for the TUI.

use crate::tui::render::{selector_height, PROGRESS_HEIGHT};
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::App;

impl App {
    /// Calculate the height needed for the input box based on content.
    /// Returns height including borders.
    /// Min: 3 lines (1 content + 2 borders)
    /// Max: `viewport_height` - 3 (reserved for progress + status)
    pub(crate) fn calculate_input_height(&self, viewport_width: u16, viewport_height: u16) -> u16 {
        const MIN_HEIGHT: u16 = 3;
        const MIN_RESERVED: u16 = 3; // status (1) + optional progress (up to 2)
        const BORDER_OVERHEAD: u16 = 2; // Top and bottom borders
        const LEFT_MARGIN: u16 = 3; // " > " prompt gutter
        const RIGHT_MARGIN: u16 = 1; // Right margin for symmetry

        // Dynamic max based on viewport height
        let max_height = viewport_height.saturating_sub(MIN_RESERVED).max(MIN_HEIGHT);

        if self.input_is_empty() {
            return MIN_HEIGHT;
        }

        // Available width for text (subtract borders, gutter, and right margin)
        let text_width = viewport_width
            .saturating_sub(BORDER_OVERHEAD)
            .saturating_sub(LEFT_MARGIN + RIGHT_MARGIN) as usize;
        if text_width == 0 {
            return MIN_HEIGHT;
        }

        // Use ComposerState's visual line count
        let line_count = self
            .input_state
            .visual_line_count(&self.input_buffer, text_width) as u16;

        // Add border overhead and clamp to bounds
        (line_count + BORDER_OVERHEAD).clamp(MIN_HEIGHT, max_height)
    }

    /// Calculate the total height of the bottom UI area.
    /// Returns: progress (1) + input (with borders) + status (1)
    /// For selector mode, returns height based on actual item count.
    pub fn calculate_ui_height(&self, width: u16, height: u16) -> u16 {
        if self.mode == Mode::Selector {
            let item_count = match self.selector_page {
                SelectorPage::Provider => self.provider_picker.filtered().len(),
                SelectorPage::Model => self.model_picker.filtered_models.len(),
                SelectorPage::Session => self.session_picker.filtered_sessions().len(),
            };
            return selector_height(item_count, height);
        }

        let progress_height = PROGRESS_HEIGHT;
        let input_height = self.calculate_input_height(width, height);
        let status_height = 1u16;
        let base = progress_height + input_height + status_height;

        // When a completer popup is active, include its height so the UI area
        // extends upward to cover the popup. This ensures the existing
        // Clear(FromCursorDown) at ui_start clears the popup area, and the
        // old_ui_start.min(ui_start) logic clears stale rows on dismiss.
        let popup_height = if self.command_completer.is_active() {
            self.command_completer.visible_candidates().len() as u16
        } else if self.file_completer.is_active() {
            self.file_completer.visible_candidates().len() as u16
        } else {
            0
        };

        base + popup_height
    }

    /// Resolve the UI start row, using row tracking or startup anchor.
    pub fn ui_start_row(&self, height: u16, ui_height: u16) -> u16 {
        let bottom_start = height.saturating_sub(ui_height);

        // Row tracking mode: UI follows chat content
        if let Some(chat_row) = self.render_state.chat_row {
            return chat_row.min(bottom_start);
        }

        // Startup: use anchor when no messages exist
        if self.message_list.entries.is_empty()
            && let Some(anchor) = self.render_state.startup_ui_anchor
        {
            return anchor.min(bottom_start);
        }

        // Default: bottom of screen
        bottom_start
    }
}
