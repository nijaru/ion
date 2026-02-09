# ion Status

## Current State

| Metric    | Value                   | Updated    |
| --------- | ----------------------- | ---------- |
| Phase     | Feature work            | 2026-02-09 |
| Status    | TUI pipeline refactored | 2026-02-09 |
| Toolchain | stable                  | 2026-01-22 |
| Tests     | 421 passing             | 2026-02-09 |
| Clippy    | clean                   | 2026-02-09 |

## Session Summary (2026-02-09)

**TUI render pipeline refactor (918f2d4..c4f9af1):**

- Replaced 4 scattered `Option` fields (`chat_row`, `startup_ui_anchor`, `last_ui_start`, `header_inserted`) with `ChatPosition` enum state machine (Empty/Header/Tracking/Scrolling)
- Extracted prepare-plan-render frame pipeline: `prepare_frame()`, `plan_chat_insert()`, `render_frame()`
- Deleted `calculate_ui_height` (redundant with `compute_layout`), added `UiLayout::height()`
- `compute_layout` no longer takes `last_top` param — reads position state directly
- All `ScrollUp` operations wrapped in synchronized updates
- 16 new tests (ChatPosition accessors, plan_chat_insert arithmetic, UiLayout::height)
- Design doc: ai/design/tui-render-pipeline.md

**Earlier this session: 7 render hardening fixes (81aa4ad..4990244)**

**Previous: Session resume fixes (079de4a, 1fe6671), TUI layout refactor (tk-5lfp)**

## Priority Queue

### P3

- tk-9tig: Custom slash commands via // prefix (skill menu)

### P4 — Deferred

tk-r11l, tk-nyqq, tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-8qwn, tk-iegz, tk-mmup

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
| Render pipeline     | ai/design/tui-render-pipeline.md            |
