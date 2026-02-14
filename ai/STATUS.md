# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-14 |
| Status    | Tool review complete          | 2026-02-14 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 469 passing (`cargo test -q`) | 2026-02-14 |
| Clippy    | clean                         | 2026-02-14 |

## Completed This Session

- Provider refactor: Split monoliths, extract shared streaming, migrate ChatGPT/Gemini to HttpClient
- Provider review fixes: drain_all rename, stale docs, unnecessary clones, build_headers returns Result
- Tool architecture review (tk-k23x): survey of 6 agents, validated 12-tool set
  - Removed dead DiscoverTool + DiscoveryCallback
  - Fixed Read-mode guard bypass: `env` prefix, subshells, redirections
  - Fixed web_fetch UTF-8 truncation bug
  - Removed emoji from bash blocked command message
  - Deep per-tool review: 7 improvement tasks created (p4)

## Known Issues

- Empty progress line on `--continue` startup (no persisted task summary) — tk-zqsw
- Intermittent duplicate header on aggressive resize churn
- Cost shows in statusline for subscription providers (ChatGPT/Gemini) — tk-bruc

## Blockers

- None.

## Next Steps

1. tk-bruc (p3): Hide cost in statusline for subscription providers
2. tk-vo8l (p3): Evaluate and iterate on system prompt
3. tk-oh88 (p3): Implement OS sandbox execution
4. tk-cmhy (p3): TOML config for approved sandbox directories (blocked by tk-oh88)

## Key References

| Topic                    | Location                                            |
| ------------------------ | --------------------------------------------------- |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`   |
| Tool review              | `ai/review/tool-builtin-review-2026-02-14.md`       |
| LLM crate survey         | `ai/research/rust-llm-crates-survey-2026-02.md`     |
| genai deep dive          | `ai/research/genai-crate-deep-dive-2026-02.md`      |
| rmcp/colgrep research    | `ai/research/rmcp-and-colgrep-crates-2026-02.md`    |
| TUI v3 architecture      | `ai/design/tui-v3-architecture-2026-02.md`          |
| pi-mono provider arch    | `ai/research/pi-mono-provider-architecture-2026.md` |
