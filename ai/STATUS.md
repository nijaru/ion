# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-18 |
| Status    | TUI polish + provider fixes   | 2026-02-18 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 497 passing (`cargo test -q`) | 2026-02-18 |
| Clippy    | clean                         | 2026-02-18 |

## Completed This Session

- **tk-9ozb** — Selector column alignment: `max_label_width` across all items, labels padded
  before hint column. (`src/tui/render/selector.rs`)

- **tk-9eni** — Model list loading indicator + disk cache. `SelectorData.loading` shows
  "Loading..." when empty; cache-first open with background refresh; no-flicker update
  preserves filter/provider scope on refresh. (`src/tui/render/selector.rs`,
  `src/tui/session/providers.rs`, `src/tui/session/update.rs`)

- **Review fixes (round 2)** — provider-scoped model list preserved on background refresh;
  `is_loading` set once after cache load; cache write errors logged via `tracing::warn!`.

## Known Issues

- tk-nupp (p2): Empty response observed once with chatgpt provider — trace logging active

## Blockers

- None.

## Next Steps

1. tk-43cd (p3): Persist MessageList display entries in session storage
2. tk-ioxh (p3): Evaluate async subagent execution model
3. tk-cmhy (p3): TOML config for approved sandbox directories

## Key References

| Topic                    | Location                                            |
| ------------------------ | --------------------------------------------------- |
| Codex CLI analysis       | `ai/research/codex-cli-system-prompt-tools-2026.md` |
| Prompt survey (5 agents) | `ai/research/system-prompt-survey-2026-02.md`       |
| API auto-injection       | `ai/research/api-auto-injected-context-2026.md`     |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`   |
| Tool review              | `ai/review/tool-builtin-review-2026-02-14.md`       |
| LLM crate survey         | `ai/research/rust-llm-crates-survey-2026-02.md`     |
| pi-mono provider arch    | `ai/research/pi-mono-provider-architecture-2026.md` |
| TUI v3 architecture      | `ai/design/tui-v3-architecture-2026-02.md`          |
