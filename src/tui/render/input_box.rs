//! Input box rendering (prompt, content lines, scrolling).

use crate::tui::composer::{build_visual_lines, ComposerState};
use crate::tui::render::{CONTINUATION, INPUT_MARGIN, PROMPT};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;

impl App {
    /// Render input content directly with crossterm.
    pub(crate) fn render_input_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        start_row: u16,
        width: u16,
        height: u16,
    ) -> std::io::Result<()> {
        let content = self.input_buffer.get_content();
        let content_width = width.saturating_sub(INPUT_MARGIN) as usize;
        let visual_lines = build_visual_lines(&content, content_width);

        // Calculate cursor and line count from precomputed content/lines (single pass)
        if content_width > 0 {
            self.input_state.calculate_cursor_pos_with(
                &content,
                &visual_lines,
                self.input_buffer.len_chars(),
                content_width,
            );
        }
        let total_lines =
            ComposerState::visual_line_count_with(&content, &visual_lines, content_width);
        let visible_height = height as usize;
        self.input_state
            .scroll_to_cursor(visible_height, total_lines);
        let scroll_offset = self.input_state.scroll_offset();
        let total_chars = content.chars().count();

        for row in 0..visible_height {
            let line_index = scroll_offset + row;
            if line_index >= total_lines {
                break;
            }
            let (start, end) = if line_index < visual_lines.len() {
                visual_lines[line_index]
            } else {
                (total_chars, total_chars)
            };

            // Extract chunk for this visual line (exclude trailing newline if present)
            let chunk: String = content
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .filter(|&c| c != '\n')
                .collect();

            execute!(w, MoveTo(0, start_row + row as u16))?;
            if line_index == 0 {
                write!(w, "{PROMPT}{chunk}")?;
            } else {
                write!(w, "{CONTINUATION}{chunk}")?;
            }
        }

        // If empty, just show the prompt
        if content.is_empty() {
            execute!(w, MoveTo(0, start_row))?;
            write!(w, "{PROMPT}")?;
        }

        Ok(())
    }
}
