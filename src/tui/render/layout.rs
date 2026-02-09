//! Layout calculations for the TUI.

use crate::tui::render::{selector_height, PROGRESS_HEIGHT};
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::App;

/// A rectangular region within the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub row: u16,
    pub height: u16,
}

/// Layout for the bottom UI area, computed once per frame.
#[derive(Debug)]
pub struct UiLayout {
    /// Topmost row of the entire UI area.
    pub top: u16,
    /// Clear from this row down (min of previous and current top).
    pub clear_from: u16,
    /// Layout variant based on current mode.
    pub body: BodyLayout,
    /// Terminal width (shared by all components).
    pub width: u16,
}

/// Layout variant for different UI modes.
#[derive(Debug)]
pub enum BodyLayout {
    /// Normal input mode, optionally with a popup.
    Input {
        popup: Option<Region>,
        progress: Region,
        input: Region,
        status: Region,
    },
    /// Full-screen selector (provider/model/session picker).
    Selector { selector: Region },
}

impl App {
    /// Compute the complete UI layout for the current frame.
    /// Returns a `UiLayout` with regions for each component.
    pub fn compute_layout(&self, width: u16, height: u16, last_top: Option<u16>) -> UiLayout {
        if self.mode == Mode::Selector {
            let item_count = match self.selector_page {
                SelectorPage::Provider => self.provider_picker.filtered().len(),
                SelectorPage::Model => self.model_picker.filtered_models.len(),
                SelectorPage::Session => self.session_picker.filtered_sessions().len(),
            };
            let sel_height = selector_height(item_count, height);
            let top = height.saturating_sub(sel_height);
            let clear_from = last_top.map_or(top, |old| old.min(top));
            return UiLayout {
                top,
                clear_from,
                body: BodyLayout::Selector {
                    selector: Region {
                        row: top,
                        height: sel_height,
                    },
                },
                width,
            };
        }

        let popup_height = self.active_popup_height();
        let progress_height = PROGRESS_HEIGHT;
        let input_height = self.calculate_input_height(width, height);
        let status_height = 1u16;
        let total = popup_height + progress_height + input_height + status_height;

        let top = self.ui_start_row(height, total);
        let clear_from = last_top.map_or(top, |old| old.min(top));

        let mut row = top;
        let popup = if popup_height > 0 {
            let r = Region {
                row,
                height: popup_height,
            };
            row += popup_height;
            Some(r)
        } else {
            None
        };
        let progress = Region {
            row,
            height: progress_height,
        };
        row += progress_height;
        let input = Region {
            row,
            height: input_height,
        };
        row += input_height;
        let status = Region {
            row,
            height: status_height,
        };

        UiLayout {
            top,
            clear_from,
            body: BodyLayout::Input {
                popup,
                progress,
                input,
                status,
            },
            width,
        }
    }

    /// Height of the currently active popup (completer or history search).
    fn active_popup_height(&self) -> u16 {
        if self.mode == Mode::Input {
            if self.command_completer.is_active() {
                self.command_completer.visible_candidates().len() as u16
            } else if self.file_completer.is_active() {
                self.file_completer.visible_candidates().len() as u16
            } else {
                0
            }
        } else {
            0
        }
    }

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

        base + self.active_popup_height()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a minimal layout for testing.
    /// Simulates an App in Input mode with given popup height.
    fn test_input_layout(
        popup_height: u16,
        input_height: u16,
        term_height: u16,
        term_width: u16,
        last_top: Option<u16>,
    ) -> UiLayout {
        // Manually build what compute_layout would produce for Input mode
        let progress_height = PROGRESS_HEIGHT;
        let status_height = 1u16;
        let total = popup_height + progress_height + input_height + status_height;
        let top = term_height.saturating_sub(total);
        let clear_from = last_top.map_or(top, |old| old.min(top));

        let mut row = top;
        let popup = if popup_height > 0 {
            let r = Region {
                row,
                height: popup_height,
            };
            row += popup_height;
            Some(r)
        } else {
            None
        };
        let progress = Region {
            row,
            height: progress_height,
        };
        row += progress_height;
        let input = Region {
            row,
            height: input_height,
        };
        row += input_height;
        let status = Region {
            row,
            height: status_height,
        };

        UiLayout {
            top,
            clear_from,
            body: BodyLayout::Input {
                popup,
                progress,
                input,
                status,
            },
            width: term_width,
        }
    }

    #[test]
    fn test_layout_regions_adjacent() {
        let layout = test_input_layout(0, 3, 40, 80, None);
        if let BodyLayout::Input {
            popup,
            progress,
            input,
            status,
        } = &layout.body
        {
            assert!(popup.is_none());
            // progress ends where input starts
            assert_eq!(progress.row + progress.height, input.row);
            // input ends where status starts
            assert_eq!(input.row + input.height, status.row);
            // all within terminal
            assert!(status.row + status.height <= 40);
        } else {
            panic!("Expected Input layout");
        }
    }

    #[test]
    fn test_layout_with_popup_regions_adjacent() {
        let layout = test_input_layout(5, 3, 40, 80, None);
        if let BodyLayout::Input {
            popup,
            progress,
            input,
            status,
        } = &layout.body
        {
            let popup = popup.expect("popup should be Some");
            // popup ends where progress starts
            assert_eq!(popup.row + popup.height, progress.row);
            // progress ends where input starts
            assert_eq!(progress.row + progress.height, input.row);
            // input ends where status starts
            assert_eq!(input.row + input.height, status.row);
        } else {
            panic!("Expected Input layout");
        }
    }

    #[test]
    fn test_layout_popup_dismiss_clears() {
        // Layout with popup: top is higher
        let with_popup = test_input_layout(5, 3, 40, 80, None);
        let old_top = with_popup.top;

        // Layout without popup, using old top as last_top
        let without_popup = test_input_layout(0, 3, 40, 80, Some(old_top));

        // clear_from should cover stale popup rows
        assert!(without_popup.clear_from <= old_top);
        assert!(without_popup.top > old_top);
    }

    #[test]
    fn test_layout_fits_terminal() {
        // Use sizes where total (popup+progress+input+status) fits in terminal
        for term_height in [20, 40, 80] {
            let layout = test_input_layout(5, 3, term_height, 80, None);
            assert!(layout.top <= term_height);
            if let BodyLayout::Input { status, .. } = &layout.body {
                assert!(status.row + status.height <= term_height);
            }
        }
    }

    #[test]
    fn test_layout_selector_mode() {
        // Selector with 10 items on a 40-row terminal
        let sel_height = selector_height(10, 40);
        let top = 40u16.saturating_sub(sel_height);
        let layout = UiLayout {
            top,
            clear_from: top,
            body: BodyLayout::Selector {
                selector: Region {
                    row: top,
                    height: sel_height,
                },
            },
            width: 80,
        };
        if let BodyLayout::Selector { selector } = &layout.body {
            assert_eq!(selector.row, top);
            assert!(selector.row + selector.height <= 40);
        }
    }
}
