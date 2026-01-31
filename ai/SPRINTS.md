# Sprint Plan: ion Stabilization & UX

## Status

| Sprint | Goal                           | Status   |
| ------ | ------------------------------ | -------- |
| 0-10   | See ai/sprints/archive-0-10.md | COMPLETE |
| 11     | TUI v2: Remove ratatui         | COMPLETE |
| 12     | Clippy Pedantic Refactoring    | COMPLETE |
| 13     | Agent Loop Decomposition       | COMPLETE |

## Current Focus

None active. See tk for backlog.

See ai/STATUS.md for details.

## Sprint 11: TUI v2 - Remove ratatui, Pure Crossterm

**Goal:** Replace ratatui with direct crossterm for proper native scrollback.
**Status:** COMPLETE (2026-01-27)
**Design:** ai/design/tui-v2.md

### Key Changes

- Removed ratatui dependency
- Direct crossterm terminal control
- Native scrollback for chat history
- Dynamic UI height for input area
- insert_before pattern for chat insertion

### Commits

See git log for 2026-01-26 to 2026-01-27.

## Sprint 12: Clippy Pedantic Refactoring

**Goal:** Enable clippy::pedantic for higher code quality.
**Status:** COMPLETE (2026-01-29)
**Design:** ai/sprints/12-clippy-pedantic.md

### Key Changes

- 97 pedantic lints enabled
- Pattern cleanup (must_use, needless_pass_by_value, etc.)
- Documentation improvements
