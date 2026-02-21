//! Direct crossterm rendering orchestrator (TUI v2 - no ratatui).

use crate::tui::render::bottom_ui::BottomUiFrame;
use crate::tui::render::layout::{BodyLayout, UiLayout};
use crate::tui::render::selector::{self, SelectorData, SelectorItem};
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::util::{
    display_width, format_context_window, format_price, format_relative_time, shorten_home_prefix,
};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use rnk::components::{Span, Text};
use rnk::core::Color as RnkColor;

impl App {
    /// Direct crossterm rendering (TUI v2 - no ratatui Terminal/Frame).
    /// Renders the bottom UI area using precomputed layout regions.
    pub fn draw_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        layout: &UiLayout,
    ) -> std::io::Result<()> {
        if self.mode == Mode::Selector || self.mode == Mode::OAuthConfirm {
            self.render_state.note_selector_top(layout.top);
        }
        self.render_state.note_ui_top(layout.top);

        // Clear current UI area downward (preserves chat scrollback above top).
        // Stale rows between clear_from and top are handled separately in
        // render_frame before chat insertion to avoid erasing new content.
        execute!(w, MoveTo(0, layout.top), Clear(ClearType::FromCursorDown))?;

        match &layout.body {
            BodyLayout::Selector { selector } => {
                if self.mode == Mode::OAuthConfirm {
                    self.render_oauth_confirm(w, selector.row, layout.width)?;
                } else {
                    self.render_selector_direct(w, selector.row, layout.width)?;
                }
            }
            BodyLayout::Input {
                popup,
                progress,
                input,
                status,
            } => {
                let show_progress_status = self.mode != Mode::HistorySearch;
                self.render_bottom_ui(
                    w,
                    BottomUiFrame {
                        progress_row: progress.row,
                        progress_height: progress.height,
                        input_row: input.row,
                        input_height: input.height,
                        status_row: status.row,
                        width: layout.width,
                        show_progress_status,
                    },
                )?;

                if self.mode == Mode::HistorySearch {
                    self.render_history_search(w, input.row, layout.width)?;
                } else if let Some(popup_region) = popup {
                    // Render completer popup in its assigned region.
                    // The popup region is at the top of the UI area; when the popup
                    // deactivates, the region disappears, top moves down, and
                    // clear_from covers stale popup rows.
                    let popup_anchor = popup_region.row + popup_region.height;
                    if self.command_completer.is_active() {
                        self.command_completer
                            .render(w, popup_anchor, layout.width)?;
                    } else if self.file_completer.is_active() {
                        self.file_completer.render(w, popup_anchor, layout.width)?;
                    }
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
                    .map(|s| display_width(s.provider.id()))
                    .max()
                    .unwrap_or(0);

                let items = self
                    .provider_picker
                    .filtered()
                    .iter()
                    .map(|s| {
                        let id = s.provider.id();
                        let auth_hint = s.provider.auth_hint();
                        let id_pad = format!(
                            "{}{}",
                            id,
                            " ".repeat(max_id_len.saturating_sub(display_width(id)))
                        );
                        let (hint, warning, warning_color) = if auth_hint.is_empty() {
                            (id.to_string(), None, None)
                        } else if s.provider == crate::provider::Provider::Gemini {
                            // Active ban wave — red warning
                            (id_pad, Some("⚠ ban risk".to_string()), Some(RnkColor::Red))
                        } else if s.provider.is_oauth() {
                            // Other OAuth (ChatGPT) — yellow unofficial caution
                            (id_pad, Some("⚠ unofficial".to_string()), None)
                        } else {
                            (format!("{}  {}", id_pad, auth_hint), None, None)
                        };
                        SelectorItem {
                            label: s.provider.name().to_string(),
                            is_valid: s.authenticated,
                            hint,
                            warning,
                            warning_color,
                        }
                    })
                    .collect();
                let effective_id_w = max_id_len.max(2);
                let col_hint = format!(
                    "{}{}  Auth",
                    "ID",
                    " ".repeat(effective_id_w.saturating_sub(display_width("ID")))
                );
                SelectorData {
                    title: "Providers",
                    description: "Select a provider",
                    items,
                    selected_idx: self.provider_picker.list_state().selected().unwrap_or(0),
                    filter_text: self.provider_picker.filter_input().text().to_string(),
                    show_tabs: true,
                    active_tab: 0,
                    loading: false,
                    column_header: Some(("Provider".to_string(), col_hint)),
                }
            }
            SelectorPage::Model => {
                let models = &self.model_picker.filtered_models;
                let max_provider_w = models
                    .iter()
                    .map(|m| display_width(&m.provider))
                    .max()
                    .unwrap_or(3)
                    .max(3); // at least "Org" header width

                // Width for price columns: at least as wide as the header label
                let max_in_w = models
                    .iter()
                    .map(|m| format_price(m.pricing.input).len())
                    .max()
                    .unwrap_or(2)
                    .max("In".len());
                let max_out_w = models
                    .iter()
                    .map(|m| format_price(m.pricing.output).len())
                    .max()
                    .unwrap_or(3)
                    .max("Out".len());

                let items = models
                    .iter()
                    .map(|m| {
                        // Strip org prefix from label when Org column shows it separately.
                        // e.g. "anthropic/claude-opus-4-5" → "claude-opus-4-5"
                        let label =
                            m.id.find('/')
                                .map_or(m.id.as_str(), |pos| &m.id[pos + 1..])
                                .to_string();
                        let ctx = format_context_window(m.context_window);
                        let price_in = format_price(m.pricing.input);
                        let price_out = format_price(m.pricing.output);
                        let hint = format!(
                            "{}{}  {:<6}  {:<max_in_w$}  {:<max_out_w$}",
                            m.provider,
                            " ".repeat(max_provider_w.saturating_sub(display_width(&m.provider))),
                            ctx,
                            price_in,
                            price_out,
                            max_in_w = max_in_w,
                            max_out_w = max_out_w,
                        );
                        SelectorItem {
                            label,
                            is_valid: true,
                            hint,
                            warning: None,
                            warning_color: None,
                        }
                    })
                    .collect();

                let col_hint = format!(
                    "{}{}  {:<6}  {:<max_in_w$}  {:<max_out_w$}",
                    "Org",
                    " ".repeat(max_provider_w.saturating_sub(display_width("Org"))),
                    "Ctx",
                    "In",
                    "Out",
                    max_in_w = max_in_w,
                    max_out_w = max_out_w,
                );
                SelectorData {
                    title: "Models",
                    description: "Select a model",
                    items,
                    selected_idx: self.model_picker.model_state.selected().unwrap_or(0),
                    filter_text: self.model_picker.filter_input.text().to_string(),
                    show_tabs: true,
                    active_tab: 1,
                    loading: self.model_picker.is_loading,
                    column_header: Some(("Model".to_string(), col_hint)),
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
                            warning_color: None,
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
                    loading: false,
                    column_header: Some(("Session".to_string(), "Directory".to_string())),
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

    /// Render the OAuth ban-risk confirmation dialog.
    pub(crate) fn render_oauth_confirm<W: std::io::Write>(
        &self,
        w: &mut W,
        start_row: u16,
        width: u16,
    ) -> std::io::Result<()> {
        use crate::tui::rnk_text::render_truncated_text_line;
        use crate::tui::util::display_width;

        execute!(w, MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;

        let line_w = width.saturating_sub(1) as usize;
        // inner_w: cells available between │ and │ (inner_w + 2 border = line_w)
        let inner_w = line_w.saturating_sub(2);

        let paint = |w: &mut W, row: u16, spans: Vec<Span>| -> std::io::Result<()> {
            execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
            if !spans.is_empty() {
                let rendered = render_truncated_text_line(Text::spans(spans), line_w);
                write!(w, "{rendered}")?;
            }
            Ok(())
        };

        let cyan = |s: String| Span::new(s).color(RnkColor::Cyan);
        let pad = |used: usize| " ".repeat(inner_w.saturating_sub(used));

        let mut row = start_row;

        // ┌─ ⚠ Google OAuth Warning ─────┐  (cyan border, red ⚠, yellow title text)
        let warn_icon = " ⚠ ";
        let warn_title = " Google OAuth Warning ";
        let fill_len = line_w
            .saturating_sub(2) // ┌ ┐
            .saturating_sub(display_width(warn_icon))
            .saturating_sub(display_width(warn_title));
        let fill = "─".repeat(fill_len);
        paint(
            w,
            row,
            vec![
                cyan(format!("┌─{warn_icon}")),
                Span::new(warn_title).color(RnkColor::Yellow).bold(),
                cyan(format!("{fill}┐")),
            ],
        )?;
        row += 1;

        // Warning body lines: cyan border, red warning text
        let warn_lines = [
            "Google is actively banning accounts using Gemini OAuth in",
            "third-party tools. Accounts are permanently disabled with",
            "no warning — including paying subscribers.",
        ];
        for text in &warn_lines {
            let used = 1 + display_width(text); // leading space + text
            paint(
                w,
                row,
                vec![
                    cyan("│ ".to_string()),
                    Span::new(text.to_string()).color(RnkColor::Red),
                    cyan(format!("{}│", pad(used))),
                ],
            )?;
            row += 1;
        }

        // Blank separator
        paint(w, row, vec![cyan(format!("│{}│", " ".repeat(inner_w)))])?;
        row += 1;

        // "Continue at your own risk?" in yellow
        let question = "Continue at your own risk?";
        let q_used = 1 + display_width(question);
        paint(
            w,
            row,
            vec![
                cyan("│ ".to_string()),
                Span::new(question).color(RnkColor::Yellow),
                cyan(format!("{}│", pad(q_used))),
            ],
        )?;
        row += 1;

        // [Enter] in red (danger), [Esc] in cyan (safe)
        let yes_label = "[Enter] Yes, continue";
        let no_label = "[Esc] No, go back";
        let actions_used = 2 + display_width(yes_label) + 3 + display_width(no_label);
        paint(
            w,
            row,
            vec![
                cyan("│  ".to_string()),
                Span::new(yes_label).color(RnkColor::Red),
                Span::new("   ".to_string()),
                Span::new(no_label).color(RnkColor::Cyan),
                cyan(format!("{}│", pad(actions_used))),
            ],
        )?;
        row += 1;

        // └───────────────────────────────┘  (cyan)
        paint(
            w,
            row,
            vec![cyan(format!("└{}┘", "─".repeat(line_w.saturating_sub(2))))],
        )?;

        execute!(w, MoveTo(0, start_row))?;
        Ok(())
    }
}
