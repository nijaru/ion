//! Direct crossterm rendering orchestrator (TUI v2 - no ratatui).

use crate::tui::render::layout::{BodyLayout, UiLayout};
use crate::tui::render::selector::{self, SelectorData, SelectorItem};
use crate::tui::render::{widgets::draw_horizontal_border, PROMPT_WIDTH};
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::util::{format_relative_time, shorten_home_prefix};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

impl App {
    /// Direct crossterm rendering (TUI v2 - no ratatui Terminal/Frame).
    /// Renders the bottom UI area using precomputed layout regions.
    pub fn draw_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        layout: &UiLayout,
    ) -> std::io::Result<()> {
        self.render_state.last_ui_start = Some(layout.top);

        // Clear from UI position downward (never clear full screen - preserves scrollback)
        execute!(
            w,
            MoveTo(0, layout.clear_from),
            Clear(ClearType::FromCursorDown)
        )?;

        match &layout.body {
            BodyLayout::Selector { selector } => {
                self.render_selector_direct(w, selector.row, layout.width)?;
            }
            BodyLayout::Input {
                popup,
                progress,
                input,
                status,
            } => {
                // Input area with borders
                let content_height = input.height.saturating_sub(2); // Minus borders
                draw_horizontal_border(w, input.row, layout.width)?;
                let content_start = input.row.saturating_add(1);
                self.render_input_direct(w, content_start, layout.width, content_height)?;
                let border_row = content_start.saturating_add(content_height);
                draw_horizontal_border(w, border_row, layout.width)?;

                // Status line
                execute!(w, MoveTo(0, status.row), Clear(ClearType::CurrentLine))?;

                if self.mode == Mode::HistorySearch {
                    self.render_history_search(w, input.row, layout.width)?;
                } else {
                    // Progress line (not shown during history search)
                    execute!(w, MoveTo(0, progress.row), Clear(ClearType::CurrentLine))?;
                    self.render_progress_direct(w, layout.width)?;

                    self.render_status_direct(w, layout.width)?;

                    // Render completer popup in its assigned region.
                    // The popup region is at the top of the UI area; when the popup
                    // deactivates, the region disappears, top moves down, and
                    // clear_from covers stale popup rows.
                    if let Some(popup_region) = popup {
                        let popup_anchor = popup_region.row + popup_region.height;
                        if self.command_completer.is_active() {
                            self.command_completer
                                .render(w, popup_anchor, layout.width)?;
                        } else if self.file_completer.is_active() {
                            self.file_completer.render(w, popup_anchor, layout.width)?;
                        }
                    }

                    // Position cursor in input area
                    let (cursor_x, cursor_y) = self.input_state.cursor_pos;
                    let scroll_offset = self.input_state.scroll_offset() as u16;
                    let cursor_y = cursor_y.saturating_sub(scroll_offset);
                    execute!(w, MoveTo(cursor_x + PROMPT_WIDTH, content_start + cursor_y))?;
                }
            }
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
                        let hint = shorten_home_prefix(&s.working_dir);
                        SelectorItem {
                            label,
                            is_valid: true,
                            hint,
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
    ) -> std::io::Result<()> {
        let data = self.selector_data();
        let (cursor_col, cursor_row) = selector::render_selector(w, &data, start_row, width)?;

        // Position cursor in filter input
        execute!(w, MoveTo(cursor_col, cursor_row))?;

        Ok(())
    }
}
