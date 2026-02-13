# ion Status

## Current State

| Metric    | Value                                                                                     | Updated    |
| --------- | ----------------------------------------------------------------------------------------- | ---------- |
| Phase     | Dogfood readiness (Sprint 16 active)                                                      | 2026-02-13 |
| Status    | RNK-first TUI refactor in progress; resize/monitor-churn edge cases still under active fix | 2026-02-13 |
| Toolchain | stable                                                                                    | 2026-01-22 |
| Tests     | 472 passing (`cargo test -q`)                                                             | 2026-02-13 |
| Clippy    | passing with existing repo-wide warnings (non-TUI)                                        | 2026-02-13 |

## Active Focus

- `tk-add8` (`active`, p2): RNK migration stabilization on `codex/rnk-bottom-ui-spike`.
- `tk-bcau` (`active`, p2): Soft-wrap/viewport separation and deterministic resize/reflow behavior.
- `tk-86lk` (`open`, p3): Fallback tracker for `--continue`/header/scrollback regressions under resize churn.

### Latest Progress (2026-02-13)

- Commit `53976b7`: startup header made static (version + cwd only); status line now shows `cwd [branch]`.
- RNK rendering contract hardened in `src/tui/rnk_text.rs`: switched from `render_to_string_no_trim` to `render_to_string` to avoid trailing-space padding that corrupted wraps/scrollback on resize and monitor moves.
- Streaming carryover finalize path fixed in `src/tui/render/chat.rs`: separator blank line is always preserved when carryover is applied, preventing adjacent agent messages from collapsing together.
- Validation complete: `cargo fmt`, `cargo check -q`, `cargo test -q` (472 pass), `cargo clippy -q --all-targets --all-features` (existing non-TUI warnings only).

## Known Issues

- Intermittent duplicate top transcript/header blocks still reproducible during aggressive monitor-switch + resize churn.
- Some wrapped markdown lines still show malformed indentation/alignment in edge resize sequences.

## Blockers

- None.

## Next Session

1. Reproduce monitor-switch + resize churn with deterministic steps and capture exact failing frame transitions in `prepare_frame`/reflow paths.
2. Audit markdown wrap pipeline for mixed-width continuation indent edge cases and lock formatting invariants with tests.
3. Add targeted regression coverage for resize+reflow+streaming interactions (especially top-block duplication and indentation drift).
4. Continue planned `src/tui/` architecture cut from `ai/design/tui-v3-architecture-2026-02.md` once resize stability is verified.

## Key References

| Topic                   | Location                                              |
| ----------------------- | ----------------------------------------------------- |
| Sprint index            | `ai/SPRINTS.md`                                       |
| Sprint 16               | `ai/sprints/16-dogfood-tui-stability.md`              |
| Runtime stack plan      | `ai/design/runtime-stack-integration-plan-2026-02.md` |
| Soft-wrap viewport plan | `ai/design/chat-softwrap-scrollback-2026-02.md`       |
| TUI v3 architecture     | `ai/design/tui-v3-architecture-2026-02.md`            |
| Manual TUI checklist    | `ai/review/tui-manual-checklist-2026-02.md`           |
