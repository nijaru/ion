//! History search overlay rendering (Ctrl+R).

use crate::tui::App;
use crate::tui::render::popup::{PopupItem, PopupRegion, PopupStyle, render_popup};
use crate::tui::rnk_text::render_truncated_text_line;
use crate::tui::util::{display_width, truncate_to_display_width};
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use rnk::components::Text;
use rnk::core::Color as RnkColor;

impl App {
    /// Render history search overlay (Ctrl+R).
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn render_history_search<W: std::io::Write>(
        &self,
        w: &mut W,
        input_start: u16,
        width: u16,
    ) -> std::io::Result<()> {
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
        let total_width = width.saturating_sub(1) as usize;
        let prefix = "(reverse-i-search)`";
        let middle = "': ";
        let mut prompt = String::new();
        prompt.push_str(prefix);
        let query_budget =
            total_width.saturating_sub(display_width(prefix) + display_width(middle));
        prompt.push_str(&truncate_to_display_width(query, query_budget));
        prompt.push_str(middle);

        // Show selected entry preview
        if let Some(&idx) = matches.get(selected)
            && let Some(entry) = self.input_history.get(idx)
        {
            let preview = entry.lines().next().unwrap_or("");
            let preview_budget = total_width.saturating_sub(display_width(&prompt));
            prompt.push_str(&truncate_to_display_width(preview, preview_budget));
        }
        let clipped = truncate_to_display_width(&prompt, total_width);
        let prompt = render_truncated_text_line(Text::new(clipped), total_width);
        write!(w, "{prompt}")?;

        // Render matches above the prompt using shared popup renderer
        if !matches.is_empty() {
            let start_idx = if selected >= max_visible {
                selected - max_visible + 1
            } else {
                0
            };

            let display_strings: Vec<String> = matches
                .iter()
                .skip(start_idx)
                .take(max_visible)
                .map(|&history_idx| {
                    self.input_history
                        .get(history_idx)
                        .map(|entry| entry.lines().next().unwrap_or("").to_string())
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
                    primary_color: RnkColor::Reset,
                    show_secondary_dimmed: false,
                    dim_unselected: true,
                },
                width.saturating_sub(2),
            )?;
        }

        Ok(())
    }
}
