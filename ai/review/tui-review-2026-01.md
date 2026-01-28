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

- [MEDIUM] Progress line duplicates after terminal tab switch during streaming
  - Location: `src/tui/render.rs`, `src/main.rs`
  - Impact: multiple progress lines in scrollback; UI desync
  - Fix: reproduce, then consider focus/visibility event handling + full UI redraw
- [MEDIUM] Markdown tables and list spacing need width-aware pretty printing
  - Location: `src/tui/highlight.rs` (render_markdown)
  - Impact: mixed alignment, lists/tables look malformed at small widths
  - Fix: add markdown pretty-printer or table renderer with wrapping
  - Follow-up: enforce single blank line between entries; add spacing after lists
- [LOW] Tool error messages duplicate \"Error:\" prefix
  - Location: `src/tui/message_list.rs`
  - Impact: awkward \"Error: Error: ...\" rendering
  - Fix: strip repeated \"Error:\" prefixes before display
- [MEDIUM] Resize reflow clears pre-ion scrollback once chat exists
  - Location: `src/main.rs` resize handler (`\x1b[3J`)
  - Impact: terminal history lost after resize during chat sessions
  - Fix: decide on preservation strategy or make behavior configurable
- [LOW] Large blank gap on launch due to UI anchoring
  - Location: `src/main.rs` + insert-before rendering
  - Impact: many empty lines between shell output and header/input
  - Fix: top-anchor UI until first message and verify resize behavior
- [LOW] Exiting clears too much screen area, leaving blank lines before prompt
  - Location: `src/main.rs` exit cleanup
  - Impact: blank scrollback gap after quitting TUI
  - Fix: clear only UI rows, keep cursor near UI start
- [LOW] --continue may load empty sessions
  - Location: `src/session/store.rs`, `src/tui/session.rs`
  - Impact: resume shows empty UI instead of last conversation
  - Fix: skip saving empty/system-only sessions, filter list_recent to user messages

## Fixes Applied

- Normalize input spacing before history/storage.
- Drop empty list-item markers in markdown rendering.
- Add input scroll offset for long input.
- Reflow chat on resize by clearing scrollback once chat exists.
- Anchor startup UI near header; clear anchored UI on exit.
- Wrap StyledLine output to terminal width for resize reflow.
- Exit clears only UI rows instead of whole screen.
- Skip saving empty/system-only sessions; list_recent filters to sessions with user messages.
- Enforce single blank line between entries; add spacing after lists in markdown rendering.
- Strip repeated \"Error:\" prefixes in tool results.
- Trim leading/trailing blank lines per entry to avoid double separators.

## Plan

1. Fix scrollback line endings (`\r\n`)
2. Fix visual line up/down clamp
3. Integrate input scroll offset for long input
4. Reproduce tab-switch duplication and add redraw strategy
5. Normalize input spacing for history/storage
6. Improve markdown list rendering + plan for markdown pretty printing
7. Reflow visible chat on resize while preserving scrollback
8. Reduce idle UI height by hiding progress line

## Evidence

- Cursor drift: `move_up_visual`/`move_down_visual` use `line_len.saturating_sub(1)`.
- Line indent: scrollback writes use `writeln!` only; raw mode can keep column.
- Input scroll: `scroll_to_cursor` never called; `scroll_offset` unused in render path.
- Markdown lists: pulldown-cmark emits empty list items; renderer prints marker without text.
- Resize: chat reflow clears scrollback to avoid stale wrapping.
