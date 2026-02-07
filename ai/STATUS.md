# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Compaction v2 design complete | 2026-02-07 |
| Status    | Research + design done        | 2026-02-07 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 314 passing                   | 2026-02-06 |
| Clippy    | clean                         | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests)           | 2026-02-06 |

## Session Summary (2026-02-07)

**Completed:**

- Google provider fix: routed via OpenAI-compatible endpoint (Generative Language API)
- Streaming text display: incremental rendering with hold-back-2 strategy
- Sprint 15: all 14/14 tasks done, marked complete
- Compaction research: surveyed 7 agents, 6 academic papers → `ai/research/compaction-techniques-2026.md`
- Compaction v2 design: Tier 3 LLM summarization + compact tool → `ai/design/compaction-v2.md`

**Key findings from research:**

- JetBrains: observation masking matches LLM summarization in 4/5 cases
- Our Tier 1/2 is already the right foundation
- Tier 3 should use small/cheap model with structured 7-section prompt
- Agent-invokable compact tool enables proactive context management

## Next Session

1. **Implement compaction v2** (tk-k28w) — design at `ai/design/compaction-v2.md`, 12 tasks in 3 phases
2. **Cost tracking** (tk-kxup, P3) — provider usage wired, needs `ModelPricing` integration

## Priority Queue

### P2 — Core functionality

| Task    | Title                | Status |
| ------- | -------------------- | ------ |
| tk-k28w | LLM-based compaction | Design |

### P3 — Important improvements

tk-kxup (cost tracking), tk-i2o1 (@file refs), tk-75jw (web search), tk-c1ij (retry-after), tk-g8xo (session cleanup), tk-2bk7 (scrollback), tk-jqe6 (parallel tool grouping), tk-r11l (research locations), tk-nyqq (symlink skills)

### P4 — Deferred

tk-epd1, tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3

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
