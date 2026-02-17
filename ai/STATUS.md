# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-16 |
| Status    | TUI polish + provider done    | 2026-02-16 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 486 passing (`cargo test -q`) | 2026-02-16 |
| Clippy    | clean                         | 2026-02-16 |

## Completed This Session

- OpenAI Responses API provider (`src/provider/openai_responses/`) + review fixes
- Tool display overhaul:
  - Grep/glob display as "search" (user-facing rename)
  - Bash calls never collapsed — each command shown individually
  - Tool args styled: bold name, plain parens, cyan content
  - Inline code backticks: cyan instead of dim gray
- Session replay fix: tool results matched by `tool_call_id` instead of position (fixed misattribution on `--continue`)
- `/clear` now purges terminal scrollback buffer
- Debug builds auto-log to `~/.ion/ion.log` (trace-level SSE events in stream loops)
- CLAUDE.md: added Model Knowledge section (current model generations)

## Known Issues

- tk-nupp (p2): Empty response observed once with chatgpt provider — trace logging now active, needs reproduction
- tk-xjmf (p3): Missing newline above progress line during streaming
- tk-86lk (p3): `--continue` header pinning breaks scrollback
- Needs manual testing with OPENAI_API_KEY for openai_responses provider

## Blockers

- None.

## Next Steps

1. Test all visual changes (tool display, cyan args, `/clear` purge, `--continue` replay)
2. tk-nupp (p2): Reproduce empty response, check `~/.ion/ion.log`
3. tk-xjmf (p3): Missing newline above progress line
4. tk-ioxh (p3): Evaluate async subagent execution model

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
