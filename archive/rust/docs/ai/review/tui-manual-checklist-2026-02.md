# TUI Manual Verification Checklist (Sprint 16)

Target runtime: ~10 minutes

## Setup

- Build once: `cargo run -- --continue`
- Use one short-history session (< terminal height chat area) and one long-history session (> terminal height).

## Checklist

- [ ] `--continue` (short history):
  - Chat history prints with no phantom blank rows at top or bottom.
  - Bottom UI is usable immediately (cursor, input, status/progress line).
- [ ] `--continue` (long history):
  - Latest messages are visible with expected scrollback behavior.
  - No header pinning or forced top anchoring.
- [ ] In-app `/resume`:
  - Open selector, choose another session, verify history and UI redraw cleanly.
  - Confirm no duplicate first-frame inserts.
- [ ] `/clear`:
  - Clears current conversation and starts fresh.
  - Does not push large blank regions into scrollback.
- [ ] Resize behavior:
  - Resize narrower then wider in both tracking and scrolling states.
  - Chat + UI stay coherent; no stale rows or clipped prompt/status remnants.
- [ ] Cancel behavior:
  - During active run, `Esc` cancels once.
  - Double `Esc` on non-empty input clears input.
  - Double `Ctrl+C`/`Ctrl+D` while idle exits as expected.
- [ ] External editor suspend/resume (`Ctrl+G`):
  - Terminal restores correctly after returning from editor.
  - Input content updates without render corruption.

## Pass Criteria

- All boxes checked.
- No top-pinned header artifacts.
- No phantom blank-line growth during resume/clear/resize flows.
