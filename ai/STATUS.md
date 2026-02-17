# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-16 |
| Status    | OpenAI Responses API done     | 2026-02-16 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 486 passing (`cargo test -q`) | 2026-02-16 |
| Clippy    | clean                         | 2026-02-16 |

## Completed This Session

- OpenAI Responses API provider (`src/provider/openai_responses/`) — replaces Chat Completions for Provider::OpenAI
  - types.rs, convert.rs, client.rs with streaming + non-streaming support
  - Reasoning summary → ThinkingDelta, budget_tokens → effort mapping
  - Incremental tool call accumulation via ToolBuilder + dedup guard
  - `strict: false` on tool defs, temperature forwarded, usage extraction
  - `with_base_url` still routes to OpenAICompat for proxy compatibility
- Review pass fixed 8 issues (3 ERROR, 5 WARN)

## Known Issues

- Empty progress line on `--continue` startup (no persisted task summary) — tk-zqsw
- Intermittent duplicate header on aggressive resize churn
- Needs manual testing with OPENAI_API_KEY (gpt-4.1-mini, gpt-5.x if available)

## Blockers

- None.

## Next Steps

1. Manual test OpenAI Responses provider with real API key
2. tk-ioxh (p3): Evaluate async subagent execution model
3. tk-oh88 (p3): Implement OS sandbox execution

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
