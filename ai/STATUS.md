# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-14 |
| Status    | Provider refactor landed      | 2026-02-14 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 468 passing (`cargo test -q`) | 2026-02-14 |
| Clippy    | clean                         | 2026-02-14 |

## Completed This Session

- Provider refactor: Split monoliths, extract shared streaming, migrate ChatGPT/Gemini to HttpClient
  - anthropic/client.rs 760→123 lines (conversion + tests → convert.rs)
  - openai_compat: merged request_builder + stream_handler → convert.rs
  - subscription/ → chatgpt/ + gemini/ (named by API surface)
  - Shared ToolCallAccumulator in stream.rs
  - ChatGPT/Gemini now use HttpClient (gains timeouts, rate limiting, Accept header)

## Known Issues

- Empty progress line on `--continue` startup (no persisted task summary) — tk-zqsw
- Intermittent duplicate header on aggressive resize churn

## Blockers

- None.

## Next Steps

See task list (`tk ls`). Priority items:

1. tk-k23x (p3): Review tool architecture — built-in vs advanced/searchable
2. tk-vo8l (p3): Evaluate and iterate on system prompt
3. tk-oh88 (p3): Implement OS sandbox execution
4. tk-cmhy (p3): TOML config for approved sandbox directories

## Key References

| Topic                 | Location                                            |
| --------------------- | --------------------------------------------------- |
| LLM crate survey      | `ai/research/rust-llm-crates-survey-2026-02.md`     |
| genai deep dive       | `ai/research/genai-crate-deep-dive-2026-02.md`      |
| rmcp/colgrep research | `ai/research/rmcp-and-colgrep-crates-2026-02.md`    |
| TUI v3 architecture   | `ai/design/tui-v3-architecture-2026-02.md`          |
| pi-mono provider arch | `ai/research/pi-mono-provider-architecture-2026.md` |
