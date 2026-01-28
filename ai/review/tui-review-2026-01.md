# TUI Review 2026-01-28

## Summary

- Scope: `src/main.rs`, `src/tui/render.rs`, `src/tui/mod.rs`, `src/tui/events.rs`, `src/tui/composer/mod.rs`, `src/tui/input.rs`, `src/tui/terminal.rs`, `src/tui/chat_renderer.rs`, `src/tui/composer/buffer.rs`, `src/tui/highlight.rs`
- Findings: 2 fix-now, 4 follow-up

## Issues (Fix Now)

- [HIGH] Visual line navigation clamps to `line_len - 1`, causing cursor drift on wrapped lines
  - Location: `src/tui/composer/mod.rs` (move_up_visual/move_down_visual)
  - Impact: cursor shifts left when moving up/down across wrapped lines
  - Fix: clamp to `line_len` so end-of-line is reachable
- [MEDIUM] Scrollback line printing uses LF only; raw mode can preserve column and indent next line
  - Location: `src/tui/terminal.rs` (StyledLine::println, print_styled_lines_to_scrollback)
  - Impact: header version line shows 3-space indent; potential column drift
  - Fix: emit `\r\n` for scrollback lines

## Issues (Follow-up)

- [MEDIUM] Input composer scroll offset is unused for long input
  - Location: `src/tui/render.rs` + `src/tui/composer/mod.rs`
  - Impact: cursor can move off-screen when input exceeds max height
  - Fix: apply `scroll_to_cursor`, render with `scroll_offset`, and offset cursor position
- [MEDIUM] Progress line duplicates after terminal tab switch during streaming
  - Location: `src/tui/render.rs`, `src/main.rs`
  - Impact: multiple progress lines in scrollback; UI desync
  - Fix: reproduce, then consider focus/visibility event handling + full UI redraw
- [MEDIUM] Markdown list items render as empty bullets when list contains blank lines
  - Location: `src/tui/highlight.rs` (render_markdown list handling)
  - Impact: output shows lone `*` lines before list content
  - Fix: drop list item marker if no text is rendered for the item
- [LOW] Input history/storage includes trailing whitespace
  - Location: `src/tui/events.rs` → `SessionStore::add_input_history`
  - Impact: inconsistent recall/history spacing
  - Fix: normalize CRLF → LF and trim trailing whitespace before storing
- [MEDIUM] Markdown tables and list spacing need width-aware pretty printing
  - Location: `src/tui/highlight.rs` (render_markdown)
  - Impact: mixed alignment, lists/tables look malformed at small widths
  - Fix: add markdown pretty-printer or table renderer with wrapping
- [MEDIUM] Resize reflow clears pre-ion scrollback
  - Location: `src/main.rs` resize handler (`\x1b[3J`)
  - Impact: terminal history lost on resize
  - Fix: decide on preserving history vs full reflow; implement chosen strategy
- [LOW] Idle UI height could be reduced by hiding progress line
  - Location: `src/tui/render.rs`, `calculate_ui_height`
  - Impact: extra blank line when idle
  - Fix: make progress line conditional

## Fixes Applied

- Normalize input spacing before history/storage.
- Drop empty list-item markers in markdown rendering.

## Plan

1. Fix scrollback line endings (`\r\n`)
2. Fix visual line up/down clamp
3. Integrate input scroll offset for long input
4. Reproduce tab-switch duplication and add redraw strategy
5. Normalize input spacing for history/storage
6. Improve markdown list rendering + plan for markdown pretty printing

## Evidence

- Cursor drift: `move_up_visual`/`move_down_visual` use `line_len.saturating_sub(1)`.
- Line indent: scrollback writes use `writeln!` only; raw mode can keep column.
- Input scroll: `scroll_to_cursor` never called; `scroll_offset` unused in render path.
- Markdown lists: pulldown-cmark emits empty list items; renderer prints marker without text.
