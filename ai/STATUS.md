# ion Status

## Current State

| Metric    | Value                | Updated    |
| --------- | -------------------- | ---------- |
| Phase     | Sprint 15 COMPLETE   | 2026-02-06 |
| Status    | All 14/14 tasks done | 2026-02-06 |
| Toolchain | stable               | 2026-01-22 |
| Tests     | 314 passing          | 2026-02-06 |
| Clippy    | clean                | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests)  | 2026-02-06 |

## Session Summary (2026-02-06)

**Completed:**

- Permissions v2: removed approval system (870 lines), simplified to Read/Write
- Compaction tuning: trigger 80%, target 60%, configurable protected_messages
- Streaming refactor: shared ToolBuilder, supports_tool_streaming trait method
- Stale stream detection: 120s timeout
- CLI run path fix: --no-sandbox + config defaults wired through
- Reviews: architecture, TUI/UX, code quality audits completed

**Sprint 15 progress:**

- Phase 1 (code quality): 8/8 tasks done
- Phase 2 (TUI UX): 4/4 tasks done (streaming text display implemented)
- Phase 3 (architecture): 2/2 tasks done

## Next Session

1. **LLM-based compaction** (tk-k28w, P2) — architecture prerequisite for memory system
2. **Fix Google provider** (tk-yy1q, P2) — Generative Lang API broken
3. **Cost tracking** (tk-kxup, P3) — provider usage wired, needs `ModelPricing` integration

## Priority Queue

### P2 — Core functionality

| Task    | Title                                     | Status |
| ------- | ----------------------------------------- | ------ |
| tk-yy1q | Fix Google provider (Generative Lang API) | Open   |

### P3 — Important improvements

tk-kxup (cost tracking), tk-i2o1 (@file refs), tk-75jw (web search), tk-c1ij (retry-after), tk-g8xo (session cleanup), tk-2bk7 (scrollback), tk-jqe6 (parallel tool grouping), tk-r11l (research locations), tk-nyqq (symlink skills)

### P4 — Deferred

tk-epd1, tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3

## Key References

| Topic               | Location                                    |
| ------------------- | ------------------------------------------- |
| Architecture        | ai/DESIGN.md                                |
| Architecture review | ai/review/architecture-review-2026-02-06.md |
| TUI/UX review       | ai/review/tui-ux-review-2026-02-06.md       |
| Code quality audit  | ai/review/code-quality-audit-2026-02-06.md  |
| Sprint 15 plan      | ai/SPRINTS.md                               |
| Permissions v2      | ai/design/permissions-v2.md                 |
| TUI design          | ai/design/tui-v2.md                         |
