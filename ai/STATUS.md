# ion Status

## Current State

| Metric    | Value                           | Updated    |
| --------- | ------------------------------- | ---------- |
| Phase     | Feature work                    | 2026-02-09 |
| Status    | Agent quality + caching shipped | 2026-02-09 |
| Toolchain | stable                          | 2026-01-22 |
| Tests     | 434 passing                     | 2026-02-09 |
| Clippy    | clean                           | 2026-02-09 |

## Session Summary (2026-02-09)

**Agent quality + caching improvements (47b9176..696e861, 12 commits):**

System prompt:

- Expanded from ~47 to ~80 lines with Task Execution (iterative work, verification, status updates) and Tool Usage (per-tool rules, parallel execution, anti-patterns) sections
- MCP tool hint via conditional template block (`has_mcp_tools` on ContextManager)
- Surveyed Claude Code, Codex CLI, Gemini CLI patterns

Anthropic caching:

- Cache breakpoint on last tool definition (covers system + tools)
- Fallback to system block breakpoint when no tools present
- History breakpoint on second-to-last real user message (skips tool results)
- Added `cache_control` to Image content blocks
- Fixed `input_tokens` missing `#[serde(default)]` — message_delta deserialization was silently failing

OpenAI-compat:

- Parse `prompt_tokens_details.cached_tokens` from streaming usage

Render cache:

- Added `has_mcp_tools` to cache key
- Added `InstructionLoader::is_stale()` — AGENTS.md changes detected mid-session via mtime + new-file checks

Review: ai/review/cache-prompt-review-2026-02-09.md

**Earlier: TUI render pipeline refactor, 7 render hardening fixes**

## Priority Queue

### P3

- tk-9tig: Custom slash commands via // prefix
- tk-3whx: Anthropic non-streaming path loses usage data

### P4 — Deferred

tk-r11l, tk-nyqq, tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-iegz, tk-mmup, tk-3fm2

## Key References

| Topic               | Location                                    |
| ------------------- | ------------------------------------------- |
| Architecture        | ai/DESIGN.md                                |
| Architecture review | ai/review/architecture-review-2026-02-06.md |
| TUI/UX review       | ai/review/tui-ux-review-2026-02-06.md       |
| Code quality audit  | ai/review/code-quality-audit-2026-02-06.md  |
| Cache/prompt review | ai/review/cache-prompt-review-2026-02-09.md |
| Sprint 16 plan      | ai/SPRINTS.md                               |
| Permissions v2      | ai/design/permissions-v2.md                 |
| TUI design          | ai/design/tui-v2.md                         |
| Render pipeline     | ai/design/tui-render-pipeline.md            |
