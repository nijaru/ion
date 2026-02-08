# ion Status

## Current State

| Metric    | Value         | Updated    |
| --------- | ------------- | ---------- |
| Phase     | Feature work  | 2026-02-07 |
| Status    | P3 tasks open | 2026-02-07 |
| Toolchain | stable        | 2026-01-22 |
| Tests     | 365 passing   | 2026-02-07 |
| Clippy    | clean         | 2026-02-07 |

## Session Summary (2026-02-07)

**Completed:**

- Attachment follow-ups (tk-24mu, tk-roct, tk-w51v): Line range syntax (`@file.rs:10-50`), vision model guard (strip images for non-vision models), token budget warning (>25% context). 30 attachment tests total. (fb9996d, c807403)
- @file/@folder inline references (tk-i2o1): Bare `@path` syntax with type detection, sandbox, truncation guards, 19 tests. (2810bf2)

**Prior this session:**

- Retry-After header parsing (tk-c1ij): extract Retry-After from 429 responses (ea12bff, 7199f77)
- Cost tracking, compact fixes, UTF-8 panic fix, dynamic summarization model selection

## Priority Queue

### P3 — Important improvements

tk-g8xo (session cleanup), tk-75jw (web search), tk-2bk7 (scrollback), tk-jqe6 (parallel tool grouping), tk-r11l (research locations), tk-nyqq (symlink skills)

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
