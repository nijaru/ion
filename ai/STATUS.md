# ion Status

## Current State

| Metric    | Value                                                 | Updated    |
| --------- | ----------------------------------------------------- | ---------- |
| Phase     | Dogfood readiness (Sprint 16 active)                  | 2026-02-12 |
| Status    | Sprint-16 closure gate + runtime stack ordering aligned | 2026-02-12 |
| Toolchain | stable                                                | 2026-01-22 |
| Tests     | 444 passing (`cargo test -q`)                         | 2026-02-11 |
| Clippy    | clean (`cargo clippy -q`)                             | 2026-02-11 |

## Active Focus

- `tk-86lk` (`active`): Code fixes are in; remaining gate is manual checklist + Ghostty validation before closing.
- Sprint 16: `ai/sprints/16-dogfood-tui-stability.md`

## Key Decisions (2026-02-11)

- **TUI**: Stay custom crossterm. rnk spike worth trying (bottom UI only, fork-ready). No crate solves inline-chat + bottom-UI pattern.
- **Providers**: Stay fully custom (~8.9K LOC). Skip genai — custom is justified, genai can't replace cache_control/OAuth/quirks.
- **MCP**: Migrate to `rmcp` (official SDK, 3.4M DL). Phase 1 priority.
- **Architecture target**: Workspace crate split (ion-provider, ion-tool, ion-agent, ion-tui, ion). Design trait boundaries now, split when needed.
- **Language**: Rust everywhere. Multi-environment (desktop, web) via Tauri/Wasm later.
- Full analysis: `ai/design/runtime-stack-integration-plan-2026-02.md`

## Blockers

- None.

## Next Session

1. Manual TUI checklist: `ai/review/tui-manual-checklist-2026-02.md`
2. Validate Ghostty regressions (narrow resize, redraw duplication, composer grow/shrink)
3. Close `tk-86lk` if checklist passes
4. Start `tk-na3u` (MCP `rmcp` migration, Phase 1 priority) after `tk-86lk` is closed
5. Start `tk-add8` on branch `codex/rnk-bottom-ui-spike` (bottom-UI-only spike with kill criteria)

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
