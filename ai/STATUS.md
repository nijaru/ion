# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-14 |
| Status    | Pruned, prompt/config done    | 2026-02-14 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 471 passing (`cargo test -q`) | 2026-02-14 |
| Clippy    | clean                         | 2026-02-14 |

## Completed This Session

- Added `instructions` config field — appends to default system prompt without replacing it
- Fixed template whitespace (Platform line combines OS + shell)
- Provider refactor plan verified complete (already done in prior session)
- Pruned ai/: DECISIONS.md 974→101 lines (archived to DECISIONS-archive-jan.md), deleted 3 superseded files, removed stale plan

## Known Issues

- Empty progress line on `--continue` startup (no persisted task summary) — tk-zqsw
- Intermittent duplicate header on aggressive resize churn
- gpt-5.x-codex underperforms vs Codex CLI due to Chat Completions format mismatch — tk-gkrr

## Blockers

- None.

## Next Steps

1. tk-gkrr (p2): OpenAI Responses API provider path — biggest perf gap for gpt-5.x models
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
