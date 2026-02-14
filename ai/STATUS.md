# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-14 |
| Status    | System prompt complete        | 2026-02-14 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 469 passing (`cargo test -q`) | 2026-02-14 |
| Clippy    | clean                         | 2026-02-14 |

## Completed This Session

- tk-vo8l: System prompt — 17 patterns adopted from competitive survey (5 agents)
  - Prompt text: no-flattery, hard response limit, fast context, no-permission-questions, no-emoji, file creation restraint, git commit restraint, no-deps, no-placeholder, security awareness, no-revert, colon prohibition
  - Code: model-specific hints (GPT-5: minimize-reasoning, DeepSeek: directness), OS/shell in environment, bash truncation 100KB→40KB
  - Model ID threaded through ContextManager.assemble() for per-model prompt selection

## Known Issues

- Empty progress line on `--continue` startup (no persisted task summary) — tk-zqsw
- Intermittent duplicate header on aggressive resize churn
- gpt-5.x-codex underperforms vs Codex CLI due to Chat Completions format mismatch — tk-gkrr

## Blockers

- None.

## Next Steps

1. tk-5ne8 (p3): Prune repository — clean up stale files, organize ai/
2. tk-gkrr (p2): OpenAI Responses API provider path — biggest perf gap for gpt-5.x models
3. tk-ioxh (p3): Evaluate async subagent execution model
4. tk-oh88 (p3): Implement OS sandbox execution

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
