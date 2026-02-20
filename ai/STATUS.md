# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-19 |
| Status    | Quick wins complete           | 2026-02-19 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 499 passing (`cargo test -q`) | 2026-02-19 |
| Clippy    | clean                         | 2026-02-19 |

## Completed This Session

- **MCP tools callable** — `all_tool_definitions()` on `ToolOrchestrator` now includes MCP tools.
  LLM can call them directly; `mcp_tools` for search only. System prompt updated.

- **Selector column headers + gap fix** — `column_header` field uses the wasted overhead slot.
  Provides Org/Ctx/In/Out columns on model picker, ID/Auth on provider, Directory on session.
  Fixed 2-line gap after selector dismissal.

- **Tool quick wins:**
  - guard: `sudo`/`doas` prefix stripped before `analyze_command`; blocked in Read mode
  - list: MAX_RESULTS=2000 cap with truncation message
  - glob: optional `path` parameter to restrict search scope

- **Persist completion summary** (tk-zqsw) — DB migration v4 adds completion columns to sessions.
  Saved after each completed task, restored on `--continue` so progress line isn't blank.

## Blockers

- tk-cmhy blocked by tk-oh88 (config depends on sandbox landing first)

## Next Steps

1. **tk-43cd** (p3): Persist MessageList display entries — needs DB schema + lifecycle work
2. **tk-oh88** (p3): OS sandbox execution — main safety feature, unblocks tk-cmhy
3. **tk-ioxh** (p3): Evaluate async subagent execution model
4. **tk-ww4t** (p4): Formalize SQLite migrations — added v4 this session, good time to document pattern

## Key References

| Topic                    | Location                                            |
| ------------------------ | --------------------------------------------------- |
| Codex CLI analysis       | `ai/research/codex-cli-system-prompt-tools-2026.md` |
| Prompt survey (5 agents) | `ai/research/system-prompt-survey-2026-02.md`       |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`   |
| Tool review              | `ai/review/tool-builtin-review-2026-02-14.md`       |
| TUI v3 architecture      | `ai/design/tui-v3-architecture-2026-02.md`          |
| Config system design     | `ai/design/config-system.md`                        |
