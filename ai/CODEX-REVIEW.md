# Codex TUI Review 2026-01-28

## Scope

- `src/main.rs`
- `src/tui/render.rs`
- `src/tui/mod.rs`
- `src/tui/events.rs`
- `src/tui/composer/mod.rs`
- `src/tui/input.rs`
- `src/tui/terminal.rs`
- `src/tui/chat_renderer.rs`
- `src/tui/composer/buffer.rs`

## Fixes Applied

- Scrollback line endings now emit CRLF in raw mode to avoid column drift/indent.
  - Files: `src/tui/terminal.rs`
- Visual line up/down movement no longer clamps to `line_len - 1`.
  - Files: `src/tui/composer/mod.rs`
- Tests updated to match corrected cursor behavior.
  - Files: `src/tui/composer/mod.rs`

## Findings

### High

- Visual line navigation clamps to `line_len - 1`, causing cursor drift on wraps.
  - Location: `src/tui/composer/mod.rs` (move_up_visual/move_down_visual)
  - Impact: cursor shifts left when moving up/down across wrapped lines
  - Status: fixed

### Medium

- Scrollback output uses LF only; raw mode preserves column, indenting next line.
  - Location: `src/tui/terminal.rs` (scrollback writes)
  - Impact: header version line shows 3-space indent; potential column drift
  - Status: fixed

- Input composer scroll offset is unused for long input.
  - Location: `src/tui/render.rs` + `src/tui/composer/mod.rs`
  - Impact: cursor can move off-screen when input exceeds max height
  - Status: pending (tk-28a4)

- Progress line duplicates after terminal tab switch during streaming.
  - Location: `src/tui/render.rs`, `src/main.rs`
  - Impact: multiple progress lines in scrollback; UI desync
  - Status: pending (tk-7aem)

## Observations

- Insert-before scrollback path in `src/main.rs` is consistent and bounded by UI height.
- `draw_direct` clears from min(old,new) UI start, which handles shrinking input cleanly.
- Composer cursor calculation and wrap logic match the tests and render path.

## Plan

1. Integrate input scroll offset into render path
   - Call `scroll_to_cursor`, render only visible window, and offset cursor position
2. Reproduce tab-switch duplication and add a redraw strategy
   - Consider focus/visibility event handling and force clear + redraw

## Tests

- `cargo test composer`

## Tasks

- tk-8bix [BUG] Scrollback lines use LF only; header/version line indents in raw mode
- tk-5pyy [BUG] Visual line up/down clamps to line_len-1 causing cursor drift on wraps
- tk-28a4 [BUG] Input composer lacks scroll offset for long input; cursor can move off-screen
- tk-7aem [BUG] Progress line duplicates when switching terminal tabs during streaming

## Workspace Note

- Two archive tarballs are marked deleted; confirm desired state before committing.
