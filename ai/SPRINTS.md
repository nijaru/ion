# Sprint Plan: ion

Sources:

- `ai/design/dogfood-readiness-2026-02.md`
- `ai/design/tui-lib-spec.md` (supersedes `tui-v3-architecture-2026-02.md`)

Generated: 2026-02-11 | Updated: 2026-02-22

| Sprint                                                              | Goal                                                                        | Status               |
| ------------------------------------------------------------------- | --------------------------------------------------------------------------- | -------------------- |
| [16-dogfood-tui-stability](sprints/16-dogfood-tui-stability.md)     | Make TUI behavior stable for daily dogfooding                               | done (via steps 1–8) |
| [17-dogfood-agent-usability](sprints/17-dogfood-agent-usability.md) | Make the agent reliable and safe for sustained work                         | pending              |
| [18-dogfood-hardening](sprints/18-dogfood-hardening.md)             | Lock in stability with lean regression controls                             | pending              |
| 19-tui-lib-phase1                                                   | `crates/tui/`: geometry, style, cell buffer, terminal (Phase 1 of lib spec) | not started          |
| 20-tui-lib-phase2                                                   | `crates/tui/`: event loop, App+Effect, hello world app (Phase 2)            | not started          |
| 21-tui-lib-phase3                                                   | `crates/tui/`: Taffy layout, Text, Block, Row/Col (Phase 3)                 | not started          |
| 22-tui-lib-phase4                                                   | `crates/tui/`: Input widget with full keybindings (Phase 4)                 | not started          |
| 23-tui-lib-phase5                                                   | `crates/tui/`: List (virtual scroll), Scroll, Overlay (Phase 5)             | not started          |
| 24-tui-lib-phase6                                                   | `crates/tui/`: Canvas, theme system, testing utilities (Phase 6)            | not started          |
| 25-ion-tui-migration                                                | ion: build ConversationView, StreamingText, ToolCallView on crates/tui      | not started          |

> **Note**: Sprints 19–22 from the old plan (tui-v3-architecture-2026-02.md, RNK-first) are retired.
> The rnk cleanup (steps 1–8) is done. New sprints 19–25 follow the lib spec phases.

## Completed

| Sprint | Goal                     | Completed  |
| ------ | ------------------------ | ---------- |
| 0-10   | Foundation               | 2026-01    |
| 11     | TUI v2: Remove ratatui   | 2026-01-27 |
| 12     | Clippy Pedantic          | 2026-01-29 |
| 13     | Agent Loop Decomposition | 2026-01-31 |
| 14     | TUI Refactoring          | 2026-02-04 |
| 15     | Code Quality + UX        | 2026-02-06 |
