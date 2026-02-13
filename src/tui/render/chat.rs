//! Chat history rendering functions.

use crate::tui::App;
use crate::tui::chat_renderer::ChatRenderer;
use crate::tui::message_list::{MessageEntry, Sender};
use crate::tui::render_state::StreamingCarryover;
use crate::tui::terminal::StyledLine;
use crate::tui::types::Mode;

fn stable_transcript_end(entries: &[MessageEntry], is_running: bool) -> usize {
    let mut end = entries.len();
    if is_running
        && entries
            .last()
            .is_some_and(|entry| entry.sender == Sender::Agent)
    {
        end = end.saturating_sub(1);
    }
    end
}

fn build_base_transcript_lines(
    header_lines: &[StyledLine],
    entries: &[MessageEntry],
    is_running: bool,
    wrap_width: usize,
) -> (Vec<StyledLine>, usize) {
    if wrap_width == 0 {
        return (Vec::new(), 0);
    }

    let mut lines = header_lines.to_vec();
    let end = stable_transcript_end(entries, is_running);
    if end > 0 {
        lines.extend(ChatRenderer::build_lines(&entries[..end], None, wrap_width));
    }
    (lines, end)
}

fn apply_stable_agent_carryover(
    entry_lines: Vec<StyledLine>,
    carryover: &mut StreamingCarryover,
    wrap_width: usize,
) -> Vec<StyledLine> {
    let skip = carryover.lines_for_width(wrap_width);
    carryover.reset();
    if skip == 0 {
        return entry_lines;
    }
    let mut remaining: Vec<StyledLine> = entry_lines.into_iter().skip(skip).collect();
    if remaining.is_empty() || !remaining.last().is_some_and(StyledLine::is_empty) {
        remaining.push(StyledLine::empty());
    }
    remaining
}

impl App {
    /// Take new chat entries and render them as lines for insertion.
    pub fn take_chat_inserts(&mut self, width: u16) -> Vec<StyledLine> {
        let wrap_width = width.saturating_sub(2) as usize;
        if wrap_width == 0 {
            return Vec::new();
        }

        let entry_count = self.message_list.entries.len();
        if self.render_state.rendered_entries > entry_count {
            self.render_state.rendered_entries = 0;
            self.render_state.buffered_chat_lines.clear();
            self.render_state.streaming_carryover.reset();
        }

        let mut new_lines = Vec::new();
        let mut index = self.render_state.rendered_entries;
        while index < entry_count {
            let entry = &self.message_list.entries[index];
            let is_last = index == entry_count - 1;
            if entry.sender == Sender::Agent && self.is_running && is_last {
                break;
            }

            let entry_lines = ChatRenderer::build_lines(
                &self.message_list.entries[index..=index],
                None,
                wrap_width,
            );

            if entry.sender == Sender::Agent && !self.render_state.streaming_carryover.is_empty() {
                new_lines.extend(apply_stable_agent_carryover(
                    entry_lines,
                    &mut self.render_state.streaming_carryover,
                    wrap_width,
                ));
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
                    wrap_width,
                );
                let already = self
                    .render_state
                    .streaming_carryover
                    .lines_for_width(wrap_width);
                let safe = all_lines.len().saturating_sub(2);
                if safe > already {
                    new_lines.extend(all_lines.into_iter().skip(already).take(safe - already));
                    self.render_state.streaming_carryover.set(safe, wrap_width);
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
        let wrap_width = width.saturating_sub(2) as usize;
        if wrap_width == 0 {
            return Vec::new();
        }

        let (lines, _rendered_entries) = build_base_transcript_lines(
            &self.startup_header_lines,
            &self.message_list.entries,
            self.is_running,
            wrap_width,
        );
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
    ) -> (Vec<StyledLine>, usize, StreamingCarryover) {
        let wrap_width = width.saturating_sub(2) as usize;
        if wrap_width == 0 {
            return (Vec::new(), 0, StreamingCarryover::default());
        }

        let (mut lines, rendered_entries) = build_base_transcript_lines(
            &self.startup_header_lines,
            &self.message_list.entries,
            self.is_running,
            wrap_width,
        );

        let mut streaming_carryover = StreamingCarryover::default();
        if self.is_running && rendered_entries < self.message_list.entries.len() {
            let entry = &self.message_list.entries[rendered_entries];
            if entry.sender == Sender::Agent {
                let all_lines = ChatRenderer::build_lines(
                    &self.message_list.entries[rendered_entries..=rendered_entries],
                    None,
                    wrap_width,
                );
                streaming_carryover = self
                    .render_state
                    .streaming_carryover
                    .for_reflow(wrap_width, all_lines.len());
                if !streaming_carryover.is_empty() {
                    lines.extend(
                        all_lines
                            .into_iter()
                            .take(streaming_carryover.committed_lines()),
                    );
                }
            }
        }

        (lines, rendered_entries, streaming_carryover)
    }

    /// Reprint full chat history into scrollback (used on session resume).
    /// Returns the number of lines written.
    pub fn reprint_chat_scrollback<W: std::io::Write>(
        &mut self,
        w: &mut W,
        width: u16,
    ) -> std::io::Result<usize> {
        let lines = self.build_chat_lines(width);
        for line in &lines {
            line.writeln_with_width(w, width)?;
        }

        self.render_state
            .mark_reflow_complete(stable_transcript_end(
                &self.message_list.entries,
                self.is_running,
            ));

        Ok(lines.len())
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_stable_agent_carryover, build_base_transcript_lines, stable_transcript_end};
    use crate::tui::chat_renderer::ChatRenderer;
    use crate::tui::message_list::{MessageEntry, Sender};
    use crate::tui::render_state::StreamingCarryover;

    fn line_text(line: &crate::tui::terminal::StyledLine) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_str())
            .collect::<String>()
    }

    #[test]
    fn stable_transcript_end_omits_active_streaming_agent_entry() {
        let entries = vec![
            MessageEntry::new(Sender::User, "u".to_string()),
            MessageEntry::new(Sender::Agent, "a".to_string()),
        ];
        assert_eq!(stable_transcript_end(&entries, true), 1);
        assert_eq!(stable_transcript_end(&entries, false), 2);
    }

    #[test]
    fn base_transcript_lines_render_startup_header_once() {
        let entries = vec![
            MessageEntry::new(Sender::User, "hello".to_string()),
            MessageEntry::new(Sender::Agent, "streaming".to_string()),
        ];
        let header = vec![
            crate::tui::terminal::StyledLine::raw("ion v0.0.0"),
            crate::tui::terminal::StyledLine::raw("~/repo [branch]"),
            crate::tui::terminal::StyledLine::empty(),
        ];
        let (lines, end) = build_base_transcript_lines(&header, &entries, true, 80);
        assert_eq!(end, 1);
        let ion_header_count = lines
            .iter()
            .filter(|line| line_text(line).starts_with("ion"))
            .count();
        assert_eq!(ion_header_count, 1);
    }

    #[test]
    fn streaming_carryover_resets_on_width_change_for_reflow() {
        let mut carryover = StreamingCarryover::default();
        carryover.set(6, 118);
        let resized = carryover.for_reflow(78, 30);
        assert!(resized.is_empty());
    }

    #[test]
    fn streaming_carryover_reflow_caps_to_safe_tail_holdback() {
        let mut carryover = StreamingCarryover::default();
        carryover.set(12, 78);
        // all_line_count=9 -> safe=7
        let reflow = carryover.for_reflow(78, 9);
        assert_eq!(reflow.committed_lines(), 7);
    }

    #[test]
    fn resize_reflow_snapshot_keeps_single_header_and_single_transcript_copy() {
        let entries = vec![
            MessageEntry::new(Sender::User, "deps".to_string()),
            MessageEntry::new(
                Sender::Agent,
                "This is actively streaming output that will wrap at narrow widths.".to_string(),
            ),
        ];

        let wrap_width = 52usize;
        let header = vec![
            crate::tui::terminal::StyledLine::raw("ion v0.0.0"),
            crate::tui::terminal::StyledLine::raw("~/repo [branch]"),
            crate::tui::terminal::StyledLine::empty(),
        ];
        let (base, end) = build_base_transcript_lines(&header, &entries, true, wrap_width);
        assert_eq!(end, 1);

        let mut carryover = StreamingCarryover::default();
        carryover.set(6, 120);
        let streaming_lines = ChatRenderer::build_lines(&entries[1..=1], None, wrap_width);
        let reflow = carryover.for_reflow(wrap_width, streaming_lines.len());
        assert!(reflow.is_empty());

        let mut snapshot = base.clone();
        if !reflow.is_empty() {
            snapshot.extend(streaming_lines.into_iter().take(reflow.committed_lines()));
        }

        let ion_header_count = snapshot
            .iter()
            .filter(|line| line_text(line).starts_with("ion"))
            .count();
        let user_prompt_count = snapshot
            .iter()
            .filter(|line| line_text(line).starts_with("› "))
            .count();

        assert_eq!(ion_header_count, 1);
        assert_eq!(user_prompt_count, 1);
    }

    #[test]
    fn stable_agent_carryover_skips_only_for_matching_width() {
        let mut carryover = StreamingCarryover::default();
        carryover.set(2, 80);

        let entry_lines = vec![
            crate::tui::terminal::StyledLine::raw("line 1"),
            crate::tui::terminal::StyledLine::raw("line 2"),
            crate::tui::terminal::StyledLine::raw("line 3"),
        ];

        let matching = apply_stable_agent_carryover(entry_lines.clone(), &mut carryover, 80);
        assert_eq!(matching.len(), 2);
        assert_eq!(line_text(&matching[0]), "line 3");
        assert!(matching[1].is_empty());
        assert!(carryover.is_empty());

        carryover.set(2, 120);
        let mismatched = apply_stable_agent_carryover(entry_lines, &mut carryover, 80);
        assert_eq!(mismatched.len(), 3);
        assert!(carryover.is_empty());
    }

    #[test]
    fn stable_agent_carryover_preserves_separator_when_all_lines_were_committed() {
        let mut carryover = StreamingCarryover::default();
        carryover.set(4, 80);
        let entry_lines = vec![
            crate::tui::terminal::StyledLine::raw("line 1"),
            crate::tui::terminal::StyledLine::raw("line 2"),
            crate::tui::terminal::StyledLine::raw("line 3"),
            crate::tui::terminal::StyledLine::raw("line 4"),
        ];

        let remaining = apply_stable_agent_carryover(entry_lines, &mut carryover, 80);
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].is_empty());
    }
}
