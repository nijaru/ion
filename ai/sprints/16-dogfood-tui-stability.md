# Sprint 16: Dogfood TUI Stability

Goal: TUI flows used every hour (`--continue`, `/resume`, `/clear`, resize, cancel) are stable and predictable.

Source: `ai/design/dogfood-readiness-2026-02.md`

## Tasks

## Task: Close Resume/Clear Scrollback Regression Work

**Depends on:** none

### Description

Finish the active `tk-86lk` stream by verifying current fixes and closing any remaining root-cause rendering bugs in the state transition path.

### Acceptance Criteria

- [ ] `cargo run -- --continue` no longer shows header pinning or phantom blank lines in short-history and long-history sessions.
- [ ] `/resume` in-session path behaves equivalently to startup resume.
- [ ] `/clear` does not inject blank scrollback rows.
- [ ] `cargo test` passes.

### Technical Notes

Focus files: `src/tui/run.rs`, `src/tui/events.rs`, `src/tui/render_state.rs`.

## Task: Add Lean Transition Regression Tests

**Depends on:** Close Resume/Clear Scrollback Regression Work

### Description

Add only low-cost unit tests for transition logic around `ChatPosition` and chat insert planning.

### Acceptance Criteria

- [ ] New tests cover Empty->Tracking/Overflow behavior and session-load reset behavior.
- [ ] No heavy PTY/snapshot harness introduced.
- [ ] `cargo test` passes with new tests.

### Technical Notes

Tests should stay in existing modules (`src/tui/run.rs`, `src/tui/render_state.rs`).

## Task: Stabilize Resize/Selector Edge Cases

**Depends on:** Close Resume/Clear Scrollback Regression Work

### Description

Review and fix remaining edge cases where selector transitions and resize can cause stale UI anchors or unintended clears.

### Acceptance Criteria

- [ ] Selector open/close preserves chat position and input usability.
- [ ] Resize from tracking and scrolling modes preserves expected behavior.
- [ ] No regressions in `/resume` and `/clear` after resize handling changes.

### Technical Notes

Focus files: `src/tui/events.rs`, `src/tui/run.rs`, `src/tui/render/layout.rs`, `src/tui/render/direct.rs`.

## Task: Ship Manual TUI Verification Checklist

**Depends on:** Stabilize Resize/Selector Edge Cases

### Description

Create and adopt a short manual verification checklist for TUI-critical changes.

### Acceptance Criteria

- [ ] Checklist covers `--continue` short/long, in-app `/resume`, `/clear`, resize, cancel, and editor suspend/resume.
- [ ] Checklist is documented in `ai/review/` or `ai/design/` and referenced from `ai/STATUS.md`.
- [ ] Team can run checklist in under 10 minutes.

### Technical Notes

Prefer a markdown checklist over brittle scripted visual assertions.

## Demo

Run ion for a real coding session and exercise all checklist flows without rendering anomalies.
