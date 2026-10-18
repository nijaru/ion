//! Terminal-local output interleaved with committed history, never model input.
use std::ops::Range;

use ion_core::TranscriptItem;
use ratatui::text::Line;

use crate::{tool_output::OutputDetail, transcript_render::ThinkingVisibility};

#[derive(Default)]
pub(super) struct Output {
    entries: Vec<Entry>,
    published: usize,
}

struct Entry {
    // A stable history prefix, not an index into provisional live progress.
    after_item: usize,
    text: String,
    diagnostic: bool,
}

impl Output {
    pub(super) fn push(&mut self, text: String, after_item: usize, diagnostic: bool) {
        if diagnostic
            && self.entries.last().is_some_and(|entry| {
                entry.diagnostic && entry.after_item == after_item && entry.text == text
            })
        {
            return;
        }
        self.entries.push(Entry {
            after_item,
            text,
            diagnostic,
        });
    }

    pub(super) fn finish_commit(&mut self, through: usize) {
        self.published += self.entries[self.published..]
            .iter()
            .take_while(|entry| entry.after_item <= through)
            .count();
    }

    pub(super) fn rows(
        &self,
        history: &[TranscriptItem],
        range: Range<usize>,
        pending: bool,
        width: usize,
        detail: OutputDetail,
        thinking: ThinkingVisibility,
    ) -> Vec<Line<'static>> {
        let mut rows = Vec::new();
        let mut cursor = range.start;
        for entry in self.entries[if pending { self.published } else { 0 }..]
            .iter()
            .take_while(|entry| entry.after_item <= range.end)
        {
            // Both cursors refer to immutable prefixes of the same Session.
            // Session replacement resets this terminal-local log.
            assert!(entry.after_item >= cursor);
            append(
                &mut rows,
                crate::transcript_render::rows(
                    &history[cursor..entry.after_item],
                    width,
                    detail,
                    thinking,
                ),
            );
            append(&mut rows, super::notice_rows(&entry.text, width));
            cursor = entry.after_item;
        }
        append(
            &mut rows,
            crate::transcript_render::rows(&history[cursor..range.end], width, detail, thinking),
        );
        rows
    }
}

fn append(rows: &mut Vec<Line<'static>>, next: Vec<Line<'static>>) {
    if next.is_empty() {
        return;
    }
    if rows.last().is_some_and(|row| row.width() > 0) {
        rows.push(Line::default());
    }
    rows.extend(next);
}
