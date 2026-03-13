# ion Status

## Current State

| Metric | Value | Updated |
| --- | --- | --- |
| Phase | TUI architecture decision: Bubble Tea v2 vs custom Rust host | 2026-03-12 |
| Status | Research active; comparing all-Go vs Go-host/Rust-core vs continued custom Rust TUI | 2026-03-12 |
| Active branch | `codex/go-rewrite-host` | 2026-03-12 |
| Tests | `cargo test -q`: 527 passing on `tui-work` | 2026-03-11 |
| Clippy | Branch-wide `-D warnings` backlog remains outside current TUI fixes | 2026-03-11 |

## Active Work

1. `tk-avhl` (p1): TUI parity umbrella on `tui-work`
2. `tk-s2ib` (p1): audit `crates/tui` coordinate/layout contracts
3. `tk-ajlv` (p1): lock inline/footer reserve contract
4. `tk-9yt1` (p1): PTY parity checklist before merge
5. `tk-6xmj` (p1): user-reported scroll/footer regression tracker
6. `tk-n6f7` (p1): evaluate Bubble Tea v2 vs custom Rust TUI host
7. `tk-3bd5` (p1): Go rewrite branch for Bubble Tea v2 host
8. `tk-nxz3` (p3): ACP backend architecture (protocol-first)

## Current Findings

- The rewrite stays the direction; no fallback to the old renderer.
- Bubble Tea v2 is now a serious alternative, not a hypothetical one. The decision is no longer "keep patching the Rust host by default."
- Current candidate directions:
  - all Go with Bubble Tea v2
  - Go Bubble Tea host plus Rust core/runtime
  - continued custom Rust TUI (currently weakest option)
- Real rewrite branch now exists:
  - `codex/go-rewrite-host`
  - `go-host/` contains the first Bubble Tea v2 vertical slice
- The current custom Rust TUI still has unresolved trust issues in the exact host/runtime layer Bubble Tea is strongest at:
  - inline region ownership
  - redraw/clear contracts
  - multiline composer growth/shrink
  - resize behavior
  - PTY edge cases
- Research note capturing the current facts and trade-offs:
  - `ai/research/bubbletea-v2-vs-rust-tui-host-2026-03-12.md`
- Current Go rewrite slice already proves a few practical things:
  - Bubble Tea v2 + Bubbles can stand up transcript/textarea/footer structure quickly
  - the `charm.land/...` module path migration is real but manageable
  - the next meaningful evaluation step is behavior quality, not framework setup

## Next Steps

1. Exercise and improve the real `go-host/` vertical slice rather than building a toy spike.
2. Compare the Bubble Tea host path against the Rust host on actual behavior quality:
   - multiline composer
   - inline redraw behavior
   - transcript/footer interaction
   - resize behavior
3. Decide between:
   - all Go
   - Go Bubble Tea host + Rust core
   - continued custom Rust TUI
4. Only then decide whether more work should land on `tui-work` or the rewrite path.
5. Keep ACP and native-agent architecture in scope while making the TUI decision so the boundary is right the first time.

## Key References

| Topic | Location |
| --- | --- |
| Active parity umbrella | `.tasks/tk-avhl.json` |
| TUI audit | `.tasks/tk-s2ib.json` |
| Inline/footer contract | `.tasks/tk-ajlv.json` |
| PTY checklist | `.tasks/tk-9yt1.json` |
| Bubble Tea decision track | `.tasks/tk-n6f7.json` |
| Go rewrite branch task | `.tasks/tk-3bd5.json` |
| ACP architecture | `.tasks/tk-nxz3.json` |
| TUI rewrite target | `ai/design/tui-v3-architecture-2026-02.md` |
| Current TUI audit note | `ai/review/tui-lib-audit-2026-03-11.md` |
| Bubble Tea research note | `ai/research/bubbletea-v2-vs-rust-tui-host-2026-03-12.md` |
