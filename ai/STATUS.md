# ion Status

## Current State

| Metric    | Value                                               | Updated    |
| --------- | --------------------------------------------------- | ---------- |
| Phase     | Dogfood readiness                                   | 2026-02-13 |
| Status    | TUI rendering fixes complete, dependency audit done | 2026-02-13 |
| Toolchain | stable                                              | 2026-01-22 |
| Tests     | 468 passing (`cargo test -q`)                       | 2026-02-13 |
| Clippy    | clean                                               | 2026-02-13 |

## Active Focus

### Completed This Session

- Selector cleanup: `RestoreAfterSelector` redraws gap rows instead of leaving blank
- FullRerender: scroll up to preserve trailing separator in scrolling mode
- Scroll viewport when UI grows in scrolling mode (overlap detection)
- **Removed progress gap row** — chat separator provides spacing, eliminates UI height oscillation
- Status line reorder: `[MODE] • model • pct% (used/max)  |  ~/path [branch]`
- Flush prompt: removed leading space from `› ` prefix
- Dependency audit: removed 5 unused deps (grep-matcher, once_cell, tiny_http, reedline, rustyline-async)
- Research: Rust LLM provider crates survey, rmcp vs mcp-sdk-rs, colgrep evaluation

### Key Commits

- `fd928b9`: Remove progress gap row + status line reorder + flush prompt
- `598b039`: Remove unused deps
- `87c4783`: Scroll viewport when UI grows in scrolling mode
- `a00db53`: FullRerender scroll-up fix
- `04b5863`: Selector cleanup RestoreAfterSelector

## Known Issues

- Empty progress line on `--continue` startup (no persisted task summary) — tk-zqsw
- Intermittent duplicate header on aggressive resize churn

## Blockers

- None.

## Next Steps

See task list (`tk ls`). Priority items:

1. tk-9e4y (p2): Migrate MCP from mcp-sdk-rs to rmcp
2. tk-oh88 (p2): Implement OS sandbox execution
3. tk-k23x (p3): Review tool architecture — built-in vs advanced/searchable
4. tk-vo8l (p3): Evaluate and iterate on system prompt
5. tk-zqsw (p3): Persist progress/completion state for session resume

## Key References

| Topic                   | Location                                         |
| ----------------------- | ------------------------------------------------ |
| LLM crate survey        | `ai/research/rust-llm-crates-survey-2026-02.md`  |
| rmcp/colgrep research   | `ai/research/rmcp-and-colgrep-crates-2026-02.md` |
| Sprint index            | `ai/SPRINTS.md`                                  |
| TUI v3 architecture     | `ai/design/tui-v3-architecture-2026-02.md`       |
| Soft-wrap viewport plan | `ai/design/chat-softwrap-scrollback-2026-02.md`  |
