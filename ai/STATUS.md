# ion Status

## Current State

| Metric    | Value         | Updated    |
| --------- | ------------- | ---------- |
| Phase     | Feature work  | 2026-02-07 |
| Status    | P3 tasks open | 2026-02-07 |
| Toolchain | stable        | 2026-01-22 |
| Tests     | 320 passing   | 2026-02-07 |
| Clippy    | clean         | 2026-02-07 |

## Session Summary (2026-02-07)

**Completed:**

- Code review fixes: Vec clone removal, bounds check, Google/ChatGPT quirks tests (cd8c2a9)
- Cost tracking: per-API-call cost via ProviderUsage x ModelPricing, /cost command, completion line display (8118e5c, tk-kxup)
- Full 3-agent review + refactor of commits 827c699..38a8939
- UTF-8 panic fix: char-boundary-safe truncation in summarization (was crashing on CJK/emoji)
- Dead code cleanup: removed unused Agent::compact_with_summary
- Compact tool placeholder now replaced with actual result after compaction
- Eliminated format_k duplication in status bar (reuses format_tokens)
- Created tk-rbx8: per-provider summarization model defaults

## Priority Queue

### P3 — Important improvements

tk-rbx8 (summarization model per provider), tk-c1ij (retry-after), tk-i2o1 (@file refs), tk-g8xo (session cleanup), tk-75jw (web search), tk-2bk7 (scrollback), tk-jqe6 (parallel tool grouping), tk-r11l (research locations), tk-nyqq (symlink skills)

### P4 — Deferred

tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3, tk-ur3b, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-8qwn, tk-iegz

## Key References

| Topic                | Location                                    |
| -------------------- | ------------------------------------------- |
| Architecture         | ai/DESIGN.md                                |
| Compaction v2 design | ai/design/compaction-v2.md                  |
| Compaction research  | ai/research/compaction-techniques-2026.md   |
| Architecture review  | ai/review/architecture-review-2026-02-06.md |
| TUI/UX review        | ai/review/tui-ux-review-2026-02-06.md       |
| Code quality audit   | ai/review/code-quality-audit-2026-02-06.md  |
| Sprint 15 plan       | ai/SPRINTS.md                               |
| Permissions v2       | ai/design/permissions-v2.md                 |
| TUI design           | ai/design/tui-v2.md                         |
