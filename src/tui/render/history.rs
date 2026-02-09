//! History search overlay rendering (Ctrl+R).

use crate::tui::render::popup::{PopupItem, PopupRegion, PopupStyle, render_popup};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

impl App {
    /// Render history search overlay (Ctrl+R).
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn render_history_search<W: std::io::Write>(
        &self,
        w: &mut W,
        input_start: u16,
        width: u16,
    ) -> std::io::Result<()> {
        use crossterm::style::{Color as CColor, Print, ResetColor, SetForegroundColor};

        let max_visible = 8;
        let matches = &self.history_search.matches;
        let selected = self.history_search.selected;
        let query = &self.history_search.query;

        // Calculate how many matches to show
        let visible_count = matches.len().min(max_visible);
        let popup_height = (visible_count + 1) as u16; // +1 for search prompt
        let popup_start = input_start.saturating_sub(popup_height);

        // Render search prompt at bottom of popup (not an item list — rendered separately)
        let prompt_row = input_start.saturating_sub(1);
        execute!(w, MoveTo(0, prompt_row), Clear(ClearType::CurrentLine))?;
        execute!(
            w,
            SetForegroundColor(CColor::Cyan),
            Print("(reverse-i-search)`"),
            ResetColor,
            Print(query),
            SetForegroundColor(CColor::Cyan),
            Print("': "),
            ResetColor,
        )?;

        // Show selected entry preview
        if let Some(&idx) = matches.get(selected)
            && let Some(entry) = self.input_history.get(idx)
        {
            let preview: String = entry
                .chars()
                .take((width as usize).saturating_sub(25))
                .collect();
            execute!(w, Print(&preview))?;
        }

        // Render matches above the prompt using shared popup renderer
        if !matches.is_empty() {
            let start_idx = if selected >= max_visible {
                selected - max_visible + 1
            } else {
                0
            };

            let max_len = (width as usize).saturating_sub(4);
            let display_strings: Vec<String> = matches
                .iter()
                .skip(start_idx)
                .take(max_visible)
                .map(|&history_idx| {
                    self.input_history
                        .get(history_idx)
                        .map(|entry| {
                            entry
                                .lines()
                                .next()
                                .unwrap_or("")
                                .chars()
                                .take(max_len)
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect();

            let items: Vec<PopupItem> = display_strings
                .iter()
                .enumerate()
                .map(|(i, display)| PopupItem {
                    primary: display,
                    secondary: "",
                    is_selected: start_idx + i == selected,
                    color_override: None,
                })
                .collect();

            render_popup(
                w,
                &items,
                PopupRegion {
                    row: popup_start,
                    height: visible_count as u16,
                },
                PopupStyle {
                    primary_color: CColor::Reset,
                    show_secondary_dimmed: false,
                    dim_unselected: true,
                },
                width.saturating_sub(2),
            )?;
        }

        Ok(())
    }
}
