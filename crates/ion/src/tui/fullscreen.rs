//! Fullscreen transcript view (pi parity: `--tui-mode fullscreen`,
//! the `tui.altScreen.*` bindings). TERMINAL.md architecture: the
//! inline frontend commits finished turns to native scrollback and
//! keeps only the live band in the diffed window. Fullscreen inverts
//! that ownership for as long as it is open: the whole terminal is a
//! diffed viewport over the committed transcript plus the live band,
//! the scroll position is explicit, and leaving fullscreen prints the
//! transcript back into native scrollback so the inline model resumes
//! without loss.
//!
//! This module is the pure controller: scroll clamping against the
//! wrapped transcript, substring search over rendered lines, and the
//! prompt-boundary index that lets jump commands hop between turns.
//! The reducer never sees the view — it is loop-owned, like the inline
//! `Transcript` — because it renders a projection of committed lines
//! that the reducer has already settled.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Search state over the wrapped transcript. The query is plain
/// substring (pi's `tui.altScreen.search`): case-sensitive matching
/// against the rendered text, exactly what the user sees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SearchState {
    pub query: String,
    /// Indices of wrapped rows whose rendered text contains the query.
    pub matches: Vec<usize>,
    /// Position in `matches`; None before the first jump.
    pub selected: Option<usize>,
}

/// The fullscreen controller state. `scroll` is the first visible
/// wrapped row; `follow` keeps the view pinned to the bottom while
/// new content arrives, exactly like pi's transcript-follow behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FullscreenView {
    /// First visible wrapped row (0 = transcript top).
    scroll: usize,
    /// Sticky bottom-follow: any new committed row keeps the view at
    /// the end until the user scrolls up. Re-enabled by jumping to the
    /// bottom (pi: `tui.altScreen.bottom`, end key).
    follow: bool,
    /// Rendered prompt boundaries: wrapped-row indices where a user
    /// turn starts. Jump commands hop between these (pi:
    /// previousPrompt / nextPrompt).
    prompt_rows: Vec<usize>,
    /// True while the search input owns the keyboard (pi: the search
    /// dialog). Typed keys edit the query; enter cycles matches; esc
    /// closes and clears the search.
    searching: bool,
    search: SearchState,
}

impl Default for FullscreenView {
    fn default() -> Self {
        Self {
            scroll: 0,
            follow: true,
            prompt_rows: Vec::new(),
            searching: false,
            search: SearchState::default(),
        }
    }
}

impl FullscreenView {
    pub(super) fn scroll(&self) -> usize {
        self.scroll
    }

    /// Re-clamp the scroll after committed rows changed. While
    /// following, pin to the bottom; otherwise keep the position (a
    /// shorter transcript can only pull it in).
    pub(super) fn clamp(&mut self, total_rows: usize, viewport_rows: usize) {
        let max = total_rows.saturating_sub(viewport_rows.min(total_rows));
        if self.follow {
            self.scroll = max;
        } else {
            self.scroll = self.scroll.min(max);
        }
    }

    /// Scroll by a signed step of wrapped rows (mouse wheel, half-page,
    /// line keys). Any explicit upward scroll suspends follow; downward
    /// past the end re-enables it.
    pub(super) fn scroll_by(&mut self, step: isize, total_rows: usize, viewport_rows: usize) {
        let max = total_rows.saturating_sub(viewport_rows.min(total_rows));
        let next = if step < 0 {
            self.scroll.saturating_sub(step.unsigned_abs())
        } else {
            self.scroll.saturating_add(step as usize)
        };
        self.scroll = next.min(max);
        self.follow = self.scroll >= max;
    }

    /// Jump to the transcript top (pi: `tui.altScreen.top`, home).
    pub(super) fn jump_top(&mut self) {
        self.scroll = 0;
        self.follow = false;
    }

    /// Jump to the end and follow new output (pi: `tui.altScreen.bottom`,
    /// end).
    pub(super) fn jump_bottom(&mut self, total_rows: usize, viewport_rows: usize) {
        self.follow = true;
        self.clamp(total_rows, viewport_rows);
    }

    /// Record where user prompts begin so jump commands can hop turns.
    /// Called by the renderer whenever the transcript is (re)built.
    pub(super) fn set_prompt_rows(&mut self, rows: Vec<usize>) {
        self.prompt_rows = rows;
    }

    /// Jump to the previous/next user prompt (pi: previousPrompt /
    /// nextPrompt). Anchors to the last prompt strictly above the
    /// current top row (or below it), matching "hop between turns".
    pub(super) fn jump_prompt(&mut self, forward: bool, total_rows: usize, viewport_rows: usize) {
        let target = if forward {
            self.prompt_rows
                .iter()
                .copied()
                .find(|&row| row > self.scroll)
        } else {
            self.prompt_rows
                .iter()
                .copied()
                .rfind(|&row| row < self.scroll)
        };
        if let Some(row) = target {
            let max = total_rows.saturating_sub(viewport_rows.min(total_rows));
            self.scroll = row.min(max);
            self.follow = false;
        }
    }

    // ---- search ----

    pub(super) fn search(&self) -> &SearchState {
        &self.search
    }

    pub(super) fn searching(&self) -> bool {
        self.searching
    }

    /// Open the search input (pi: `tui.altScreen.search`, ctrl+f).
    pub(super) fn open_search(&mut self) {
        self.searching = true;
    }

    /// Close the search input and clear the highlights (pi:
    /// searchClose, esc). The query is discarded, not retained.
    pub(super) fn close_search(&mut self) {
        self.searching = false;
        self.search.query.clear();
        self.search.matches.clear();
        self.search.selected = None;
    }

    /// Recompute matches for a fresh/changed query over the wrapped
    /// rows. Selects the first match at or below the current view top
    /// so an in-view hit is immediately obvious.
    pub(super) fn search_set(&mut self, query: &str, wrapped: &[Line<'_>], viewport_rows: usize) {
        self.search.query = query.to_owned();
        if query.is_empty() {
            self.search.matches.clear();
            self.search.selected = None;
            return;
        }
        self.search.matches = wrapped
            .iter()
            .enumerate()
            .filter(|(_, line)| line_text(line).contains(query))
            .map(|(row, _)| row)
            .collect();
        // Anchor to the first match at/below the view top; otherwise
        // start at the first match, wherever it sits.
        let anchor = self
            .search
            .matches
            .iter()
            .position(|&row| row >= self.scroll);
        self.search.selected = Some(anchor.unwrap_or(0));
        if let Some(row) = self.selected_row() {
            self.scroll_to_row_visible(row, viewport_rows);
        }
    }

    /// Cycle to the next/previous match (pi: searchNext /
    /// searchPrevious). Wraps like pi. Returns the selected wrapped
    /// row.
    pub(super) fn search_next(&mut self, forward: bool, viewport_rows: usize) -> Option<usize> {
        if self.search.matches.is_empty() {
            return None;
        }
        let index = match self.search.selected {
            None => 0,
            Some(index) => {
                let len = self.search.matches.len();
                if forward {
                    (index + 1) % len
                } else {
                    (index + len - 1) % len
                }
            }
        };
        self.search.selected = Some(index);
        let row = self.search.matches[index];
        self.scroll_to_row_visible(row, viewport_rows);
        Some(row)
    }

    /// The wrapped row of the currently selected match.
    pub(super) fn selected_row(&self) -> Option<usize> {
        self.search
            .selected
            .and_then(|index| self.search.matches.get(index).copied())
    }

    /// Scroll so `row` is inside the viewport (top-aligned when it is
    /// above the view; bottom-anchored when below).
    fn scroll_to_row_visible(&mut self, row: usize, viewport_rows: usize) {
        if row < self.scroll {
            self.scroll = row;
        } else if row + 1 > self.scroll + viewport_rows {
            self.scroll = row + 1 - viewport_rows;
        }
        self.follow = false;
    }

    /// Style a wrapped line for fullscreen rendering: search matches
    /// highlight in bold+underline (pi's searchMatch styling).
    pub(super) fn render_line(
        &self,
        line: &Line<'_>,
        row: usize,
        palette: &super::render::Palette,
    ) -> Line<'static> {
        let selected = self.selected_row();
        let in_matches = self.search.matches.contains(&row);
        if self.search.query.is_empty() || !in_matches {
            let _ = palette;
            return clone_line(line);
        }
        let base = line_text(line);
        let query = self.search.query.as_str();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut rest = base.as_str();
        let is_selected = selected == Some(row);
        let style = if is_selected {
            Style::new().bold().underlined().yellow()
        } else {
            Style::new().underlined()
        };
        while let Some(at) = rest.find(query) {
            if at > 0 {
                spans.push(Span::raw(rest[..at].to_owned()));
            }
            spans.push(Span::styled(rest[at..at + query.len()].to_owned(), style));
            rest = &rest[at + query.len()..];
        }
        spans.push(Span::raw(rest.to_owned()));
        Line::from(spans)
    }
}

/// Plain text of a styled line, for search matching.
fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.to_string())
        .collect()
}

/// Clone a borrowed line to an owned one, preserving span styles.
fn clone_line(line: &Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|span| Span::styled(span.content.to_string(), span.style))
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(rows: usize) -> Vec<Line<'static>> {
        (0..rows).map(|i| Line::from(format!("row {i}"))).collect()
    }

    #[test]
    fn follow_pins_to_bottom_and_clamps_on_growth() {
        let mut view = FullscreenView::default();
        assert!(view.follow);
        view.clamp(100, 10);
        assert_eq!(view.scroll(), 90);
        // New rows arrive: follow keeps the bottom.
        view.clamp(120, 10);
        assert_eq!(view.scroll(), 110);
    }

    #[test]
    fn scrolling_up_suspends_follow_and_past_end_resumes() {
        let mut view = FullscreenView::default();
        view.clamp(100, 10);
        view.scroll_by(-3, 100, 10);
        assert_eq!(view.scroll(), 87);
        assert!(!view.follow);
        // Scrolling to the max re-enables follow.
        view.scroll_by(3, 100, 10);
        assert_eq!(view.scroll(), 90);
        assert!(view.follow);
        // And beyond: clamped, still following.
        view.scroll_by(5, 100, 10);
        assert_eq!(view.scroll(), 90);
    }

    #[test]
    fn top_and_bottom_jumps_match_pi_keys() {
        let mut view = FullscreenView::default();
        view.clamp(100, 10);
        view.jump_top();
        assert_eq!(view.scroll(), 0);
        assert!(!view.follow);
        view.jump_bottom(100, 10);
        assert_eq!(view.scroll(), 90);
        assert!(view.follow);
    }

    #[test]
    fn prompt_jumps_hop_between_turn_boundaries() {
        let mut view = FullscreenView::default();
        view.set_prompt_rows(vec![0, 40, 80]);
        view.clamp(100, 10);
        // From the bottom view (rows 90-99 visible), every prompt is
        // above: backward hops to 80 first, then 40, then 0.
        view.jump_prompt(false, 100, 10);
        assert_eq!(view.scroll(), 80);
        view.jump_prompt(false, 100, 10);
        assert_eq!(view.scroll(), 40);
        view.jump_prompt(false, 100, 10);
        assert_eq!(view.scroll(), 0);
        // Forward hops in order; the last prompt is a no-op.
        view.jump_prompt(true, 100, 10);
        assert_eq!(view.scroll(), 40);
        view.jump_prompt(true, 100, 10);
        assert_eq!(view.scroll(), 80);
        view.jump_prompt(true, 100, 10);
        assert_eq!(view.scroll(), 80);
    }

    #[test]
    fn search_finds_matches_and_cycles_with_wrap() {
        let mut view = FullscreenView::default();
        view.clamp(100, 10);
        let lines: Vec<Line<'static>> = (0..100)
            .map(|i| {
                if i % 25 == 5 {
                    Line::from(format!("row {i} with needle here"))
                } else {
                    Line::from(format!("row {i}"))
                }
            })
            .collect();
        view.search_set("needle", &lines, 10);
        assert_eq!(view.search().matches, vec![5, 30, 55, 80]);
        // Anchored to the first match at/below the bottom-pinned view
        // (scroll 90): that is 80... which is BELOW scroll. Anchor
        // finds none >= 90, so falls back to first match.
        assert_eq!(view.selected_row(), Some(5));
        view.scroll = 90;
        view.search_next(true, 10);
        assert_eq!(view.selected_row(), Some(30));
        view.search_next(true, 10);
        assert_eq!(view.selected_row(), Some(55));
        view.search_next(true, 10);
        assert_eq!(view.selected_row(), Some(80));
        // Wrap to the first.
        view.search_next(true, 10);
        assert_eq!(view.selected_row(), Some(5));
        // Backward wraps too.
        view.search_next(false, 10);
        assert_eq!(view.selected_row(), Some(80));
    }

    #[test]
    fn empty_query_clears_search_state() {
        let mut view = FullscreenView::default();
        let lines = wrapped(10);
        view.search_set("row", &lines, 10);
        assert!(!view.search().matches.is_empty());
        view.search_set("", &lines, 10);
        assert!(view.search().matches.is_empty());
        assert!(view.search().selected.is_none());
    }

    #[test]
    fn match_selection_scroll_keeps_the_hit_visible() {
        let mut view = FullscreenView::default();
        view.clamp(100, 10);
        let lines: Vec<Line<'static>> = (0..100)
            .map(|i| {
                if i == 3 {
                    Line::from("early needle")
                } else if i == 97 {
                    Line::from("late needle")
                } else {
                    Line::from(format!("row {i}"))
                }
            })
            .collect();
        // From the bottom view (scroll 90), the on-screen match 97 is
        // the anchor; cycling forward wraps to the off-screen 3 and
        // scrolls to make it visible.
        view.search_set("needle", &lines, 10);
        assert_eq!(view.selected_row(), Some(97));
        view.search_next(true, 10);
        assert_eq!(view.selected_row(), Some(3));
        assert_eq!(view.scroll(), 3);
    }
}
