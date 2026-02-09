//! Direct crossterm rendering orchestrator (TUI v2 - no ratatui).

use crate::tui::render::selector::{self, SelectorData, SelectorItem};
use crate::tui::render::{widgets::draw_horizontal_border, PROGRESS_HEIGHT, PROMPT_WIDTH};
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::util::format_relative_time;
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

impl App {
    /// Direct crossterm rendering (TUI v2 - no ratatui Terminal/Frame).
    /// Renders the bottom UI area: progress, input, status.
    pub fn draw_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        width: u16,
        height: u16,
    ) -> std::io::Result<()> {
        let ui_height = self.calculate_ui_height(width, height);
        let ui_start = self.ui_start_row(height, ui_height);
        let progress_height = PROGRESS_HEIGHT;

        // Popup height (already included in ui_height via calculate_ui_height).
        // When active, the popup occupies the top rows of the UI area and
        // progress/input/status shift down by this amount.
        let popup_height = if self.mode == Mode::Input {
            if self.command_completer.is_active() {
                self.command_completer.visible_candidates().len() as u16
            } else if self.file_completer.is_active() {
                self.file_completer.visible_candidates().len() as u16
            } else {
                0
            }
        } else {
            0
        };

        // Determine clear_from based on positioning mode.
        // Read last_ui_start BEFORE updating; store new value for next frame.
        let old_ui_start = self.render_state.last_ui_start;
        self.render_state.last_ui_start = Some(ui_start);

        // Use min(old, new) so stale UI rows (e.g. popup dismiss, mode change)
        // are cleared when the UI area shrinks.
        let clear_from = old_ui_start.map_or(ui_start, |old| old.min(ui_start));

        // Clear from UI position downward (never clear full screen - preserves scrollback)
        execute!(w, MoveTo(0, clear_from), Clear(ClearType::FromCursorDown))?;

        // Progress line (after popup area; only in Input mode - selector has its own UI)
        if progress_height > 0 && self.mode == Mode::Input {
            execute!(
                w,
                MoveTo(0, ui_start + popup_height),
                Clear(ClearType::CurrentLine)
            )?;
            self.render_progress_direct(w, width)?;
        }

        // Input area (after popup + progress, with borders)
        let input_start = ui_start
            .saturating_add(popup_height)
            .saturating_add(progress_height);
        let input_height = self.calculate_input_height(width, height).saturating_sub(2); // Minus borders

        // Top border
        draw_horizontal_border(w, input_start, width)?;

        // Input content
        let content_start = input_start.saturating_add(1);
        self.render_input_direct(w, content_start, width, input_height)?;

        // Bottom border
        let border_row = content_start.saturating_add(input_height);
        draw_horizontal_border(w, border_row, width)?;

        // Status line
        let status_row = border_row.saturating_add(1);
        execute!(w, MoveTo(0, status_row), Clear(ClearType::CurrentLine))?;

        // In selector mode, render selector instead of normal input/status
        if self.mode == Mode::Selector {
            self.render_selector_direct(w, ui_start, width, height)?;
        } else if self.mode == Mode::HistorySearch {
            self.render_history_search(w, input_start, width)?;
        } else {
            self.render_status_direct(w, width)?;

            // Render completer popup at top of UI area (rows ui_start to
            // ui_start + popup_height - 1). The popup render function draws
            // upward from a given row, so pass ui_start + popup_height.
            // When the popup deactivates, ui_height shrinks, ui_start moves
            // down, and old_ui_start.min(ui_start) clears stale popup rows.
            if self.command_completer.is_active() {
                self.command_completer
                    .render(w, ui_start + popup_height, width)?;
            } else if self.file_completer.is_active() {
                self.file_completer
                    .render(w, ui_start + popup_height, width)?;
            }

            // Position cursor in input area
            // cursor_pos is relative (x within content, y is visual line 0-indexed)
            let (cursor_x, cursor_y) = self.input_state.cursor_pos;
            let scroll_offset = self.input_state.scroll_offset() as u16;
            let cursor_y = cursor_y.saturating_sub(scroll_offset);
            execute!(w, MoveTo(cursor_x + PROMPT_WIDTH, content_start + cursor_y))?;
        }

        Ok(())
    }

    /// Extract data needed to render the current selector page.
    pub(crate) fn selector_data(&self) -> SelectorData {
        match self.selector_page {
            SelectorPage::Provider => {
                // Find max id length for column alignment
                let max_id_len = self
                    .provider_picker
                    .filtered()
                    .iter()
                    .map(|s| s.provider.id().len())
                    .max()
                    .unwrap_or(0);

                let items = self
                    .provider_picker
                    .filtered()
                    .iter()
                    .map(|s| {
                        // Always show id and auth method in aligned columns
                        let id = s.provider.id();
                        let auth_hint = s.provider.auth_hint();
                        let (hint, warning) = if auth_hint.is_empty() {
                            // Local provider - no auth needed
                            (id.to_string(), None)
                        } else if s.provider.is_oauth() {
                            // OAuth providers - show warning (unofficial)
                            (
                                format!("{:width$}", id, width = max_id_len),
                                Some("⚠ unofficial".to_string()),
                            )
                        } else {
                            // API key providers - show env var
                            (
                                format!("{:width$}  {}", id, auth_hint, width = max_id_len),
                                None,
                            )
                        };
                        SelectorItem {
                            label: s.provider.name().to_string(),
                            is_valid: s.authenticated,
                            hint,
                            warning,
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Providers",
                    description: "Select a provider",
                    items,
                    selected_idx: self.provider_picker.list_state().selected().unwrap_or(0),
                    filter_text: self.provider_picker.filter_input().text().to_string(),
                    show_tabs: true,
                    active_tab: 0,
                }
            }
            SelectorPage::Model => {
                let items = self
                    .model_picker
                    .filtered_models
                    .iter()
                    .map(|m| {
                        let hint = crate::tui::util::format_context_window(m.context_window);
                        SelectorItem {
                            label: m.id.clone(),
                            is_valid: true,
                            hint,
                            warning: None,
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Models",
                    description: "Select a model",
                    items,
                    selected_idx: self.model_picker.model_state.selected().unwrap_or(0),
                    filter_text: self.model_picker.filter_input.text().to_string(),
                    show_tabs: true,
                    active_tab: 1,
                }
            }
            SelectorPage::Session => {
                let items = self
                    .session_picker
                    .filtered_sessions()
                    .iter()
                    .map(|s| {
                        let preview = s.first_user_message.as_ref().map_or_else(
                            || "No preview".to_string(),
                            |m: &String| m.chars().take(40).collect::<String>(),
                        );
                        let label = format!("{} - {}", preview, format_relative_time(s.updated_at));
                        SelectorItem {
                            label,
                            is_valid: true,
                            hint: String::new(),
                            warning: None,
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Sessions",
                    description: "Select a session to resume",
                    items,
                    selected_idx: self.session_picker.list_state().selected().unwrap_or(0),
                    filter_text: self.session_picker.filter_input().text().to_string(),
                    show_tabs: false,
                    active_tab: 0,
                }
            }
        }
    }

    /// Render selector (provider/model/session picker) directly with crossterm.
    pub(crate) fn render_selector_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        start_row: u16,
        width: u16,
        _height: u16,
    ) -> std::io::Result<()> {
        let data = self.selector_data();
        let (cursor_col, cursor_row) = selector::render_selector(w, &data, start_row, width)?;

        // Position cursor in filter input
        execute!(w, MoveTo(cursor_col, cursor_row))?;

        Ok(())
    }
}
