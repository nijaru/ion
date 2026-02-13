//! Chat history rendering functions.

use crate::tui::App;
use crate::tui::chat_renderer::ChatRenderer;
use crate::tui::message_list::Sender;
use crate::tui::terminal::StyledLine;
use crate::tui::types::Mode;

impl App {
    /// Take new chat entries and render them as lines for insertion.
    pub fn take_chat_inserts(&mut self, width: u16) -> Vec<StyledLine> {
        let wrap_width = width.saturating_sub(2);
        if wrap_width == 0 {
            return Vec::new();
        }

        let entry_count = self.message_list.entries.len();
        if self.render_state.rendered_entries > entry_count {
            self.render_state.rendered_entries = 0;
            self.render_state.buffered_chat_lines.clear();
            self.render_state.streaming_lines_rendered = 0;
            self.render_state.streaming_wrap_width = None;
        }

        let mut new_lines = Vec::new();
        let mut index = self.render_state.rendered_entries;
        while index < entry_count {
            let entry = &self.message_list.entries[index];
            // Skip the actively streaming agent entry (handled in streaming phase below)
            let is_last = index == entry_count - 1;
            if entry.sender == Sender::Agent && self.is_running && is_last {
                break;
            }
            let entry_lines = ChatRenderer::build_lines(
                &self.message_list.entries[index..=index],
                None,
                wrap_width as usize,
            );
            // Skip already-committed streaming lines to prevent duplicates
            // (handles tool-call interruption and agent finish)
            if entry.sender == Sender::Agent && self.render_state.streaming_lines_rendered > 0 {
                let skip = self.render_state.streaming_lines_rendered;
                self.render_state.streaming_lines_rendered = 0;
                self.render_state.streaming_wrap_width = None;
                new_lines.extend(entry_lines.into_iter().skip(skip));
            } else {
                new_lines.extend(entry_lines);
            }
            index += 1;
        }
        self.render_state.rendered_entries = index;

        // Incrementally render the actively streaming agent entry.
        // Hold back last 2 lines: the last content line may change from
        // word-wrapping or unclosed markdown, plus the trailing blank line.
        if self.is_running && index < entry_count {
            let entry = &self.message_list.entries[index];
            if entry.sender == Sender::Agent {
                let all_lines = ChatRenderer::build_lines(
                    &self.message_list.entries[index..=index],
                    None,
                    wrap_width as usize,
                );
                let already = self.render_state.streaming_lines_rendered;
                let safe = all_lines.len().saturating_sub(2);
                if safe > already {
                    new_lines.extend(all_lines.into_iter().skip(already).take(safe - already));
                    self.render_state.streaming_lines_rendered = safe;
                    self.render_state.streaming_wrap_width = Some(wrap_width as usize);
                }
            }
        }

        if self.mode == Mode::Selector {
            if !new_lines.is_empty() {
                self.render_state.buffered_chat_lines.extend(new_lines);
            }
            return Vec::new();
        }

        if new_lines.is_empty() && self.render_state.buffered_chat_lines.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
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
        lines.extend(Self::startup_header_lines(&self.session.working_dir));

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

    /// Build chat lines for viewport reflow.
    ///
    /// Includes previously committed lines from an actively streaming
    /// agent entry so resize reflow can repaint without re-appending
    /// those lines on the next incremental frame.
    pub fn build_chat_lines_for_reflow(
        &self,
        width: u16,
    ) -> (Vec<StyledLine>, usize, usize, Option<usize>) {
        let wrap_width = width.saturating_sub(2);
        if wrap_width == 0 {
            return (Vec::new(), 0, 0, None);
        }

        let mut lines = Vec::new();
        lines.extend(Self::startup_header_lines(&self.session.working_dir));

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

        let mut streaming_committed = 0usize;
        let mut streaming_wrap_width = None;
        if self.is_running && end < entry_count {
            let entry = &self.message_list.entries[end];
            if entry.sender == Sender::Agent {
                let all_lines = ChatRenderer::build_lines(
                    &self.message_list.entries[end..=end],
                    None,
                    wrap_width as usize,
                );
                streaming_committed = streaming_lines_for_reflow(
                    wrap_width as usize,
                    self.render_state.streaming_lines_rendered,
                    self.render_state.streaming_wrap_width,
                    all_lines.len(),
                );
                if streaming_committed > 0 {
                    lines.extend(all_lines.into_iter().take(streaming_committed));
                    streaming_wrap_width = Some(wrap_width as usize);
                }
            }
        }

        (lines, end, streaming_committed, streaming_wrap_width)
    }

    /// Reprint full chat history into scrollback (used on session resume).
    /// Returns the number of lines written.
    pub fn reprint_chat_scrollback<W: std::io::Write>(
        &mut self,
        w: &mut W,
        width: u16,
    ) -> std::io::Result<usize> {
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
            line.writeln_with_width(w, width)?;
        }

        self.render_state.mark_reflow_complete(end);

        Ok(lines.len())
    }
}

fn streaming_lines_for_reflow(
    wrap_width: usize,
    streaming_lines_rendered: usize,
    streaming_wrap_width: Option<usize>,
    all_line_count: usize,
) -> usize {
    if streaming_wrap_width != Some(wrap_width) {
        return 0;
    }

    let safe = all_line_count.saturating_sub(2);
    streaming_lines_rendered.min(safe)
}

#[cfg(test)]
mod tests {
    use super::streaming_lines_for_reflow;

    #[test]
    fn reflow_streaming_lines_reset_on_width_change() {
        assert_eq!(streaming_lines_for_reflow(78, 6, Some(118), 30), 0);
    }

    #[test]
    fn reflow_streaming_lines_capped_by_safe_tail_holdback() {
        // all_line_count=9 -> safe=7
        assert_eq!(streaming_lines_for_reflow(78, 12, Some(78), 9), 7);
    }
}
