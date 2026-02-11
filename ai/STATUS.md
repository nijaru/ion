# ion Status

## Current State

| Metric | Value | Updated |
| --- | --- | --- |
| Phase | Dogfood readiness (Sprint 16 active) | 2026-02-11 |
| Status | Headless deferred; TUI stability pass in progress | 2026-02-11 |
| Toolchain | stable | 2026-01-22 |
| Tests | 436 passing (`cargo test -q`) | 2026-02-11 |
| Clippy | clean (last verified) | 2026-02-09 |

## Active Focus

- `tk-86lk` (`active`): close `--continue`/`/resume`/`/clear` rendering regressions with manual visual verification and resize/selector sweep.
- Sprint 16 execution: `ai/sprints/16-dogfood-tui-stability.md`.

## Blockers

- None.

## Recent Work

- Planned dogfood-readiness roadmap and sprints 16-18 from `ai/design/dogfood-readiness-2026-02.md`.
- Landed source-level TUI render-state fixes across `src/tui/run.rs`, `src/tui/events.rs`, and `src/tui/render_state.rs`.
- Added lean transition regression coverage; suite now at 436 passing tests.

## Next Session Start

1. Run manual TUI checklist on `--continue` short/long history, in-app `/resume`, `/clear`, resize, cancel, and suspend/resume.
2. Fix any remaining resize/selector edge cases found during manual verification.
3. Close `tk-86lk` if checklist passes, then move Sprint 17 to `active`.

## Key References

| Topic | Location |
| --- | --- |
| Sprint index | `ai/SPRINTS.md` |
| Sprint 16 tasks | `ai/sprints/16-dogfood-tui-stability.md` |
| Dogfood readiness design | `ai/design/dogfood-readiness-2026-02.md` |
| Permissions architecture | `ai/design/permissions-v2.md` |
| TUI render pipeline | `ai/design/tui-render-pipeline.md` |
