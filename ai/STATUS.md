# ion Status

## Current State

| Metric    | Value                                                 | Updated    |
| --------- | ----------------------------------------------------- | ---------- |
| Phase     | Dogfood readiness (Sprint 16 active)                  | 2026-02-12 |
| Status    | RNK-first TUI spike active; MCP migration deferred     | 2026-02-12 |
| Toolchain | stable                                                | 2026-01-22 |
| Tests     | 451 passing (`cargo test -q`)                         | 2026-02-12 |
| Clippy    | clean (`cargo clippy -q`)                             | 2026-02-11 |

## Active Focus

- `tk-add8` (`active`): Time-boxed RNK bottom-UI spike on `codex/rnk-bottom-ui-spike` with explicit keep/kill criteria.
- `tk-add8` progress (2026-02-12): Initial RNK path landed in `9e41163` behind `ION_RNK_BOTTOM_UI=1`. Scope includes progress/input/status rendering only; chat scrollback path unchanged.
- `tk-add8` progress (2026-02-12): Follow-up RNK fixes restore colored `[READ]/[WRITE]` mode label and reserve a persistent blank spacer row above progress while streaming.
- `tk-add8` progress (2026-02-12): Direction updated to RNK-first on this branch: collapse env-gated bottom-UI split path and keep one Input-mode renderer path.
- `tk-add8` progress (2026-02-12): RNK-first collapse landed: Input-mode bottom UI now renders through RNK path only; legacy crossterm bottom-UI modules removed.
- `tk-add8` progress (2026-02-12): Remaining UI surfaces migrated to RNK primitives (selector + popup/completer + history-search prompt row). UI rendering stack is now RNK-first across active surfaces.
- `tk-add8` progress (2026-02-12): Resize behavior updated to preserve single-copy chat history: removed resize-triggered full transcript reprint; overlap now handled by bounded viewport scroll before bottom UI redraw.
- `tk-add8` progress (2026-02-12): Shared RNK text-line helper introduced and duplicated per-module RNK render snippets removed; chat `StyledLine` terminal writes now render through RNK spans/text path.
- `tk-add8` progress (2026-02-12): Resize now triggers viewport-safe reflow from canonical chat lines (visible-tail repaint at new width) to prevent narrow-width soft-wrap cutoff artifacts while keeping a single on-screen transcript block.
- `tk-rpst` (`done`, p2): Resize bugfix shipped for duplicate history + prompt-box artifacts after shrinking terminal width/height.
- `tk-bcau` (`open`, p2): Soft-wrap chat + viewport-separation architecture selected as target regardless of RNK choice.
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

1. Manual smoke with `cargo run -- --continue` on Ghostty (selector/completer/history-search visuals + resize/composer behavior) against RNK-first path, specifically verifying no duplicate on-screen transcript block and no narrow-width word clipping after repeated resize/move
2. Record keep/kill decision with evidence against Ghostty/narrow-width regressions
3. Start `tk-bcau` soft-wrap viewport migration (remove width-coupled reflow path first, then planner transition updates)
4. If RNK is killed, resume `tk-86lk` + checklist closure on current crossterm path
5. Continue core agent/API reliability tasks (`tk-oh88`, `tk-ts00`) after TUI direction is decided
6. Keep `tk-na3u` deferred until MCP usage becomes product-relevant

## Key References

| Topic                   | Location                                              |
| ----------------------- | ----------------------------------------------------- |
| Sprint index            | `ai/SPRINTS.md`                                       |
| Sprint 16               | `ai/sprints/16-dogfood-tui-stability.md`              |
| Runtime stack plan      | `ai/design/runtime-stack-integration-plan-2026-02.md` |
| Soft-wrap viewport plan | `ai/design/chat-softwrap-scrollback-2026-02.md`       |
| TUI crate research      | `ai/research/tui-crates-2026-02.md`                   |
| Provider crate research | `ai/research/provider-crates-2026-02.md`              |
| Manual TUI checklist    | `ai/review/tui-manual-checklist-2026-02.md`           |
| TUI v3 architecture     | `ai/design/tui-v3-architecture-2026-02.md`            |
| Dogfood readiness       | `ai/design/dogfood-readiness-2026-02.md`              |
