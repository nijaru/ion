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

- **tk-9ozb fixed** — Selector column alignment: `max_label_width` computed across all items,
  labels padded before hint column so env vars / context windows align vertically.
  (`src/tui/render/selector.rs`)

- **Tool result coloring** — result status lines (` ✓`, ` ✗`, ` ⎿`) always dim gray, 2-space
  indent. Fixed read tool getting 4-space indent via syntax branch. (`src/tui/chat_renderer.rs`)

- **grep/glob display** — path-first format `grep(src, "pattern...")`, glob relativizes absolute
  paths, `display_name()` no-op removed. (`src/tui/message_list.rs`)

- **Review fixes** — OpenAI Responses API: temperature cleared when reasoning active; InputImage
  nested `{"url": "..."}` shape; lifecycle fallback no longer corrupts entry 0.
  (`src/provider/openai_responses/`, `src/tui/session/lifecycle.rs`)

## Known Issues

- tk-nupp (p2): Empty response observed once with chatgpt provider — trace logging active

## Blockers

- None.

## Next Steps

1. tk-43cd (p3): Persist MessageList display entries in session storage
2. tk-ioxh (p3): Evaluate async subagent execution model
3. tk-9eni (p3): Model list loading state + cache

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
