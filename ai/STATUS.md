# ion Status

## Current State

| Metric    | Value                                                          | Updated    |
| --------- | -------------------------------------------------------------- | ---------- |
| Phase     | Dogfood readiness                                              | 2026-02-13 |
| Status    | TUI polish pass complete, dependency audit done, research done | 2026-02-13 |
| Toolchain | stable                                                         | 2026-01-22 |
| Tests     | 468 passing (`cargo test -q`)                                  | 2026-02-13 |
| Clippy    | clean                                                          | 2026-02-13 |

## Completed This Session

- Selector cleanup: `RestoreAfterSelector` redraws gap rows instead of leaving blank
- FullRerender: scroll up to preserve trailing separator in scrolling mode
- Scroll viewport when UI grows in scrolling mode (overlap detection)
- Removed progress gap row — chat separator provides spacing, eliminates UI height oscillation
- Status line: reordered to `[MODE] • model • pct% (used/max) • $cost • ~/path [branch]`
- Status line: always show cost ($0.00 when fresh), session cost accumulates
- Context % persisted on `--continue` (computed eagerly in load_session)
- Flush prompt: removed leading space from `› ` prefix
- Dependency audit: removed 5 unused deps (grep-matcher, once_cell, tiny_http, reedline, rustyline-async)
- Research: Rust LLM provider crates survey, rmcp vs mcp-sdk-rs, colgrep evaluation

## Known Issues

- Empty progress line on `--continue` startup (no persisted task summary) — tk-zqsw
- Intermittent duplicate header on aggressive resize churn

## Blockers

- None.

## Next Steps

See task list (`tk ls`). Priority items:

1. tk-9e4y (p2): Migrate MCP from mcp-sdk-rs to rmcp (official SDK, typed, modern spec)
2. tk-oh88 (p2): Implement OS sandbox execution
3. tk-k23x (p3): Review tool architecture — built-in vs advanced/searchable
4. tk-vo8l (p3): Evaluate and iterate on system prompt (add tool usage guidance)
5. tk-zqsw (p3): Persist progress/completion state for session resume

## Key References

| Topic                 | Location                                         |
| --------------------- | ------------------------------------------------ |
| LLM crate survey      | `ai/research/rust-llm-crates-survey-2026-02.md`  |
| rmcp/colgrep research | `ai/research/rmcp-and-colgrep-crates-2026-02.md` |
| Sprint index          | `ai/SPRINTS.md`                                  |
| TUI v3 architecture   | `ai/design/tui-v3-architecture-2026-02.md`       |
