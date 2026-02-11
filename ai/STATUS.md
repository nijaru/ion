# ion Status

## Current State

| Metric | Value | Updated |
| --- | --- | --- |
| Phase | Dogfood readiness (Sprint 16 active) | 2026-02-11 |
| Status | Headless deferred; TUI stability pass in progress | 2026-02-11 |
| Toolchain | stable | 2026-01-22 |
| Tests | 444 passing (`cargo test -q`) | 2026-02-11 |
| Clippy | clean (`cargo clippy -q`) | 2026-02-11 |

## Active Focus

- `tk-86lk` (`active`): close `--continue`/`/resume`/`/clear` rendering regressions with manual visual verification and resize/selector sweep.
- `tk-86lk` follow-up (2026-02-11): fix live resize/composer regressions from Ghostty manual pass (`TERM=xterm-ghostty`, `68x141`): narrow-width wrap truncation, duplicated redraw artifacts, composer growth overwriting history, and repeated `«Pasted #1»` placeholder numbering.
- Sprint 16 execution: `ai/sprints/16-dogfood-tui-stability.md`.

## Blockers

- None.

## Recent Work

- Planned dogfood-readiness roadmap and sprints 16-18 from `ai/design/dogfood-readiness-2026-02.md`.
- Drafted TUI v3 architecture program in `ai/design/tui-v3-architecture-2026-02.md` and created execution sprints 19-22 (`ai/sprints/19-tui-render-core.md` through `ai/sprints/22-tui-perf-and-regression-gates.md`).
- Landed source-level TUI render-state fixes across `src/tui/run.rs`, `src/tui/events.rs`, and `src/tui/render_state.rs`.
- Added lean transition regression coverage; suite now at 444 passing tests.
- Startup `--continue` redraw now uses full-viewport clear (`Clear(All)+MoveTo(0,0)`) instead of `ScrollUp(cursor_y+1)`, removing a source of phantom blank-row insertion.
- Ghostty manual-repro follow-up patch: force reflow when UI growth would intrude into tracked chat (`src/tui/run.rs`), use display-width wrap gate for styled lines (`src/tui/chat_renderer.rs`), and make paste blob IDs monotonic across clears to avoid repeated `«Pasted #1»` (`src/tui/composer/buffer.rs`).
- Small-width redraw hardening: progress and status rows are now width-bounded/truncated before printing (`src/tui/render/progress.rs`, `src/tui/render/status.rs`, `src/tui/util.rs`) so narrow terminals cannot autowrap bottom rows and corrupt chat/history rendering.
- Follow-up width safety pass: clamped popup/history/selector renderer rows to display width and switched file completer truncation away from Unicode-unsafe byte slicing (`src/tui/render/popup.rs`, `src/tui/render/history.rs`, `src/tui/render/selector.rs`, `src/tui/file_completer.rs`).
- Validation: `cargo fmt`; `cargo test -q tui::` (181 passed); `cargo clippy -q` clean. Full `cargo test -q` still fails in this environment due existing non-TUI reqwest/system-configuration proxy panics.

## Next Session Start

1. Run manual TUI checklist: `ai/review/tui-manual-checklist-2026-02.md`.
2. Validate Ghostty regressions specifically: narrow resize wrap integrity, no redraw duplication, Shift+Enter grow/shrink preserving history, paste placeholder numbering behavior.
3. Fix any remaining resize/selector edge cases found during manual verification.
4. Decide whether to execute sprint 19 immediately (TUI render core refactor) or finish sprint-16 closure work first.
5. Close `tk-86lk` if checklist passes, then move Sprint 17 to `active` unless reprioritizing to sprint 19.

## Key References

| Topic | Location |
| --- | --- |
| Sprint index | `ai/SPRINTS.md` |
| Sprint 16 tasks | `ai/sprints/16-dogfood-tui-stability.md` |
| TUI v3 architecture plan | `ai/design/tui-v3-architecture-2026-02.md` |
| TUI v3 sprint execution | `ai/sprints/19-tui-render-core.md` |
| Manual TUI checklist | `ai/review/tui-manual-checklist-2026-02.md` |
| Dogfood readiness design | `ai/design/dogfood-readiness-2026-02.md` |
| Permissions architecture | `ai/design/permissions-v2.md` |
| TUI render pipeline | `ai/design/tui-render-pipeline.md` |
