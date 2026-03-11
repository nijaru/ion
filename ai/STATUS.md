# ion Status

## Current State

| Metric | Value | Updated |
| --- | --- | --- |
| Phase | `tui-work` stabilization and parity hardening | 2026-03-11 |
| Status | Rewrite retained; `crates/tui` contract audit in progress, text entry box still unstable | 2026-03-11 |
| Active branch | `tui-work` | 2026-03-11 |
| Tests | `cargo test -q`: 524 passing on `tui-work` | 2026-03-11 |
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
- The main TUI failure class is contract mismatch inside the new stack:
  - buffer/write coordinates vs widget/layout coordinates
  - inline reserve height vs actual rendered height
  - footer layout overflow when multiline input exceeds the reserved region
- Recent fixes restored scrollback parity, prompt-box styling, local row clearing, and panic-hook cleanup, but the text entry box is still the main instability.
- Current footer contract is only partially improved:
  - shrink behavior is closer to the intended reserve model
  - initial multiline growth can still duplicate prompt/border rows
  - PTY parity is still blocked on prompt-box correctness
- Current implementation direction remains: reserve height grows with the active draft, overflow is clipped within the reserve, and slack stays below the visible footer until reset.

## Next Steps

1. Fix duplicate prompt/border rows on initial multiline growth in the text entry box.
2. Complete the `crates/tui` audit and encode findings as tests/guards.
3. Close remaining parity items (resize, picker/completer clearing, cancel flows, transcript ordering).
4. Run PTY/manual checklist only after the text entry box is stable enough to trust.
5. Once TUI is stable enough to dogfood, start `tk-43cd` (persist rendered display entries).
6. ACP starts only after TUI/core agent are stable enough to support a new backend layer.

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
