//! Chat history rendering functions.

use crate::tui::chat_renderer::ChatRenderer;
use crate::tui::message_list::Sender;
use crate::tui::terminal::StyledLine;
use crate::tui::types::Mode;
use crate::tui::App;

impl App {
    /// Take new chat entries and render them as lines for insertion.
    pub fn take_chat_inserts(&mut self, width: u16) -> Vec<StyledLine> {
        let wrap_width = width.saturating_sub(2);
        if wrap_width == 0 {
            return Vec::new();
        }

        // Insert header once at startup (into scrollback, not viewport)
        let header_lines = if self.render_state.header_inserted {
            Vec::new()
        } else {
            self.render_state.header_inserted = true;
            Self::startup_header_lines()
        };

        let entry_count = self.message_list.entries.len();
        if self.render_state.rendered_entries > entry_count {
            self.render_state.rendered_entries = 0;
            self.render_state.buffered_chat_lines.clear();
        }

        let mut new_lines = Vec::new();
        let mut index = self.render_state.rendered_entries;
        while index < entry_count {
            let entry = &self.message_list.entries[index];
            // Only skip the last entry if it's an Agent entry being actively streamed
            // This allows Tool entries and completed Agent responses to render mid-run
            let is_last = index == entry_count - 1;
            if entry.sender == Sender::Agent && self.is_running && is_last {
                break;
            }
            let mut entry_lines = ChatRenderer::build_lines(
                &self.message_list.entries[index..=index],
                None,
                wrap_width as usize,
            );
            new_lines.append(&mut entry_lines);
            index += 1;
        }
        self.render_state.rendered_entries = index;

        if self.mode == Mode::Selector {
            if !new_lines.is_empty() {
                self.render_state.buffered_chat_lines.extend(new_lines);
            }
            // Still return header if it needs to be inserted
            return header_lines;
        }

        if new_lines.is_empty()
            && self.render_state.buffered_chat_lines.is_empty()
            && header_lines.is_empty()
        {
            return Vec::new();
        }

        let mut out = header_lines;
        if !self.render_state.buffered_chat_lines.is_empty() {
            out.append(&mut self.render_state.buffered_chat_lines);
        }
        out.extend(new_lines);
        out
    }

    /// Build chat history lines for a given width.
    pub fn build_chat_lines(&self, width: u16) -> Vec<StyledLine> {
        let wrap_width = width.saturating_sub(2);
        if wrap_width == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        lines.extend(Self::startup_header_lines());

        let entry_count = self.message_list.entries.len();
        let mut end = entry_count;
        if self.is_running
            && let Some(last) = self.message_list.entries.last()
            && last.sender == Sender::Agent
        {
            end = end.saturating_sub(1);
        }
        if end > 0 {
            lines.extend(ChatRenderer::build_lines(
                &self.message_list.entries[..end],
                None,
                wrap_width as usize,
            ));
        }

        lines
    }

    /// Reprint full chat history into scrollback (used on session resume).
    pub fn reprint_chat_scrollback<W: std::io::Write>(
        &mut self,
        w: &mut W,
        width: u16,
    ) -> std::io::Result<()> {
        let entry_count = self.message_list.entries.len();
        let mut end = entry_count;
        if self.is_running
            && let Some(last) = self.message_list.entries.last()
            && last.sender == Sender::Agent
        {
            end = end.saturating_sub(1);
        }

        let lines = self.build_chat_lines(width);
        for line in &lines {
            line.writeln(w)?;
        }

        self.render_state.mark_reflow_complete(end);

        Ok(())
    }
}
