# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | Core hardening      | 2026-02-06 |
| Status    | Permissions v2 done | 2026-02-06 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 314 passing         | 2026-02-06 |
| Clippy    | clean               | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Session Summary (2026-02-06)

**Permissions v2 implemented:**

- Removed approval system: ApprovalHandler, ApprovalResponse, NeedsApproval, TuiApprovalHandler, Mode::Approval
- Removed ToolMode::Agi variant, CLI flags -r -w -y --agi
- Kept --read (long only) and --no-sandbox
- Write mode allows all tools unconditionally; sandbox provides security
- 870 lines deleted across 12 source files
- Review found --read not wired to CLI `run` mode — fixed
- Removed unimplemented deny_commands config field
- Deleted stale v1 permission design doc, updated DESIGN.md and DECISIONS.md

## Next Session

1. **tk-ubad (P2):** /compact slash command
2. **tk-yy1q (P2):** Fix Google provider (Generative Lang API)
3. **tk-g1fy (P2):** Modular streaming interface

## Priority Queue

### P2 — Core functionality

| Task    | Title                                     | Status |
| ------- | ----------------------------------------- | ------ |
| tk-ubad | /compact slash command                    | Open   |
| tk-yy1q | Fix Google provider (Generative Lang API) | Open   |
| tk-g1fy | Modular streaming interface               | Open   |

### P3 — Important improvements

tk-75jw (web search), tk-kxup (cost tracking), tk-i2o1 (@file refs), tk-nyqq (symlink skills), tk-r11l (research standard locations), tk-kqie (stream timeout), tk-c1ij (retry-after), tk-4fyx (compaction tuning), tk-g8xo (session cleanup), tk-2bk7 (scrollback), tk-jqe6 (parallel tool grouping)

### P4 — Deferred

tk-ltyy, tk-epd1, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3

## Key References

| Topic                  | Location                                  |
| ---------------------- | ----------------------------------------- |
| Architecture           | ai/DESIGN.md                              |
| Permissions v2         | ai/design/permissions-v2.md               |
| Permission research    | ai/research/permission-systems-2026.md    |
| Extensibility research | ai/research/extensibility-systems-2026.md |
| System prompt research | ai/research/system-prompt-survey-2026.md  |
| TUI design             | ai/design/tui-v2.md                       |
| Tool pass design       | ai/design/tool-pass.md                    |
| Agent design           | ai/design/agent.md                        |
| TUI analysis           | ai/review/tui-analysis-2026-02-04.md      |
| Claude Code comparison | ai/research/claude-code-architecture.md   |
