# ion Status

## Current State

| Metric | Value | Updated |
| --- | --- | --- |
| Phase | `tui-work` stabilization and parity hardening | 2026-03-11 |
| Status | Rewrite retained; runtime/footer contract hardening landed, PTY parity still pending | 2026-03-11 |
| Active branch | `tui-work` | 2026-03-11 |
| Tests | `cargo test -q`: 527 passing on `tui-work` | 2026-03-11 |
| Clippy | Branch-wide `-D warnings` backlog remains outside current TUI fixes | 2026-03-11 |

## Active Work

1. `tk-avhl` (p1): TUI parity umbrella on `tui-work`
2. `tk-s2ib` (p1): audit `crates/tui` coordinate/layout contracts
3. `tk-ajlv` (p1): lock inline/footer reserve contract
4. `tk-9yt1` (p1): PTY parity checklist before merge
5. `tk-6xmj` (p1): user-reported scroll/footer regression tracker
6. `tk-nxz3` (p3): ACP backend architecture (protocol-first)

## Current Findings

- The rewrite stays the direction; no fallback to the old renderer.
- The main TUI failure class remains contract mismatch inside the new stack:
  - frame-buffer coordinates vs terminal-global assumptions
  - inline reserve height vs actual rendered height
  - stale inline rows during reserve growth and resize
- This session hardened the runtime/footer path:
  - `Terminal` now tracks and clears stale inline regions across `Inline -> Inline` growth and resize
  - `AppRunner` invalidates `prev_buf` when the render area changes
  - `IonApp` footer now renders from one `FooterLayout`/`FooterViewModel` instead of stacked ad hoc canvases
  - widget/canvas docs now explicitly state the frame-buffer coordinate contract
- Unit coverage now exists for:
  - bottom-anchored inline region math
  - stale-clear union math
  - footer layout clipping/slack behavior
  - multiline footer row stacking
- PTY parity is still not closed. The remaining open question is user-observed behavior in a real terminal, especially the former duplicate prompt/border rows on initial multiline growth.

## Next Steps

1. Run PTY/manual validation for multiline growth/shrink, resize, and footer placement in native terminal and `tmux`.
2. If the duplicate-row repro is gone, close `tk-ajlv` and move to the remaining parity checklist (`tk-9yt1`).
3. If PTY still exposes edge cases, continue the `crates/tui` audit with the new runtime/frame-buffer contract in mind.
4. Once TUI is stable enough to dogfood, start `tk-43cd` (persist rendered display entries).
5. ACP starts only after TUI/core agent are stable enough to support a new backend layer.

## Key References

| Topic | Location |
| --- | --- |
| Active parity umbrella | `.tasks/tk-avhl.json` |
| TUI audit | `.tasks/tk-s2ib.json` |
| Inline/footer contract | `.tasks/tk-ajlv.json` |
| PTY checklist | `.tasks/tk-9yt1.json` |
| ACP architecture | `.tasks/tk-nxz3.json` |
| TUI rewrite target | `ai/design/tui-v3-architecture-2026-02.md` |
| Current TUI audit note | `ai/review/tui-lib-audit-2026-03-11.md` |
