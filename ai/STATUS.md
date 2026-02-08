# ion Status

## Current State

| Metric    | Value         | Updated    |
| --------- | ------------- | ---------- |
| Phase     | Feature work  | 2026-02-07 |
| Status    | P3 tasks open | 2026-02-07 |
| Toolchain | stable        | 2026-01-22 |
| Tests     | 372 passing   | 2026-02-07 |
| Clippy    | clean         | 2026-02-07 |

## Session Summary (2026-02-07)

**Completed:**

- Parallel tool call grouping (tk-jqe6): Consecutive same-name tool calls collapsed into "read(3 files)" with per-item `⎿` results. ID-based result routing fixes bug where parallel results went to wrong entry. (e52c255)
- Session retention (tk-g8xo): `session_retention_days` config (default 90), auto-cleanup on startup. Also prunes empty sessions. (cf21462)
- Attachment follow-ups (tk-24mu, tk-roct, tk-w51v): Line ranges, vision guard, token warning. (fb9996d, c807403)
- @file/@folder inline references (tk-i2o1): Bare `@path` syntax. (2810bf2)

**Prior this session:**

- Retry-After header parsing (tk-c1ij), cost tracking, compact fixes, UTF-8 panic fix

## Priority Queue

### P3 — Important improvements

tk-75jw (web search), tk-2bk7 (scrollback), tk-r11l (research locations), tk-nyqq (symlink skills)

### P4 — Deferred

tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-8qwn, tk-iegz

## Key References

| Topic                | Location                                    |
| -------------------- | ------------------------------------------- |
| Architecture         | ai/DESIGN.md                                |
| File refs research   | ai/research/file-refs-2026.md               |
| Compaction v2 design | ai/design/compaction-v2.md                  |
| Architecture review  | ai/review/architecture-review-2026-02-06.md |
| TUI/UX review        | ai/review/tui-ux-review-2026-02-06.md       |
| Code quality audit   | ai/review/code-quality-audit-2026-02-06.md  |
| Sprint 15 plan       | ai/SPRINTS.md                               |
| Permissions v2       | ai/design/permissions-v2.md                 |
| TUI design           | ai/design/tui-v2.md                         |
