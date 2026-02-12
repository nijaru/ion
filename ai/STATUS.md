# ion Status

## Current State

| Metric    | Value                                                 | Updated    |
| --------- | ----------------------------------------------------- | ---------- |
| Phase     | Dogfood readiness (Sprint 16 active)                  | 2026-02-12 |
| Status    | RNK/core-TUI spike prioritized; MCP migration deferred | 2026-02-12 |
| Toolchain | stable                                                | 2026-01-22 |
| Tests     | 444 passing (`cargo test -q`)                         | 2026-02-11 |
| Clippy    | clean (`cargo clippy -q`)                             | 2026-02-11 |

## Active Focus

- `tk-add8` (`active`): Time-boxed RNK bottom-UI spike on `codex/rnk-bottom-ui-spike` with explicit keep/kill criteria.
- `tk-add8` progress (2026-02-12): Initial RNK path landed in `9e41163` behind `ION_RNK_BOTTOM_UI=1`. Scope includes progress/input/status rendering only; chat scrollback path unchanged.
- `tk-86lk` (`open`, blocked by `tk-add8`): Keep as fallback regression stream if RNK spike is killed.
- Sprint 16: `ai/sprints/16-dogfood-tui-stability.md`

## Key Decisions (2026-02-11)

- **TUI**: Stay custom crossterm. rnk spike worth trying (bottom UI only, fork-ready). No crate solves inline-chat + bottom-UI pattern.
- **Providers**: Stay fully custom (~8.9K LOC). Skip genai — custom is justified, genai can't replace cache_control/OAuth/quirks.
- **MCP**: Defer `rmcp` migration until MCP is part of active workflows; not currently a near-term product blocker.
- **Architecture target**: Workspace crate split (ion-provider, ion-tool, ion-agent, ion-tui, ion). Design trait boundaries now, split when needed.
- **Language**: Rust everywhere. Multi-environment (desktop, web) via Tauri/Wasm later.
- Full analysis: `ai/design/runtime-stack-integration-plan-2026-02.md`

## Blockers

- None.

## Next Session

1. Execute `tk-add8` on `codex/rnk-bottom-ui-spike` (bottom UI only: input/progress/status)
2. Manual smoke with `ION_RNK_BOTTOM_UI=1 cargo run -- --continue` on Ghostty (narrow resize, composer grow/shrink, redraw duplication)
3. Record keep/kill decision with evidence against Ghostty/narrow-width regressions
4. If RNK is killed, resume `tk-86lk` + checklist closure on current crossterm path
5. Continue core agent/API reliability tasks (`tk-oh88`, `tk-ts00`) after TUI direction is decided
6. Keep `tk-na3u` deferred until MCP usage becomes product-relevant

## Key References

| Topic                   | Location                                              |
| ----------------------- | ----------------------------------------------------- |
| Sprint index            | `ai/SPRINTS.md`                                       |
| Sprint 16               | `ai/sprints/16-dogfood-tui-stability.md`              |
| Runtime stack plan      | `ai/design/runtime-stack-integration-plan-2026-02.md` |
| TUI crate research      | `ai/research/tui-crates-2026-02.md`                   |
| Provider crate research | `ai/research/provider-crates-2026-02.md`              |
| Manual TUI checklist    | `ai/review/tui-manual-checklist-2026-02.md`           |
| TUI v3 architecture     | `ai/design/tui-v3-architecture-2026-02.md`            |
| Dogfood readiness       | `ai/design/dogfood-readiness-2026-02.md`              |
