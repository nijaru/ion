# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | Feature work   | 2026-02-08 |
| Status    | P1 gaps closed | 2026-02-08 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 390 passing    | 2026-02-08 |
| Clippy    | clean          | 2026-02-08 |

## Session Summary (2026-02-08)

**Sprint 16: Activate Dormant Infrastructure**

Three P1 gaps from architecture review now closed:

- Default subagents (Phase 1): `SubagentRegistry::with_defaults()` ships explorer + planner configs. User YAML overrides by name. CLI mode now registers subagents too. (cf9ca9c)
- Config-driven hooks (Phase 2): `[[hooks]]` in TOML parsed into `CommandHook` at startup. Shell commands run with env vars (ION_HOOK_EVENT, ION_TOOL_NAME, ION_WORKING_DIR). Supports tool_pattern regex filtering. Both TUI and CLI. (92b0586)
- MCP lazy loading (Phase 3): MCP tools indexed but not registered in system prompt. Model discovers via `mcp_tools` search tool, calls via `ToolOrchestrator` fallback. Saves ~13K tokens per MCP server. (32639c0)

Also closed: tk-ije3 (hooks architecture).

**Prior session (2026-02-07):**

- Web search tool (tk-75jw), scrollback preservation (tk-2bk7), parallel tool grouping, session retention, attachment improvements

## Priority Queue

### P4 — Deferred

tk-r11l, tk-nyqq, tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-8qwn, tk-iegz

## Key References

| Topic               | Location                                    |
| ------------------- | ------------------------------------------- |
| Architecture        | ai/DESIGN.md                                |
| Architecture review | ai/review/architecture-review-2026-02-06.md |
| TUI/UX review       | ai/review/tui-ux-review-2026-02-06.md       |
| Code quality audit  | ai/review/code-quality-audit-2026-02-06.md  |
| Sprint 16 plan      | ai/SPRINTS.md                               |
| Permissions v2      | ai/design/permissions-v2.md                 |
| TUI design          | ai/design/tui-v2.md                         |
