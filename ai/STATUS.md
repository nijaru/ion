# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-27 |
| Status     | Runnable        | 2026-01-27 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 105 passing     | 2026-01-27 |
| Clippy     | 0 warnings      | 2026-01-27 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**TUI v2 Design** - Drop ratatui, use crossterm directly. See `ai/design/tui-v2.md`.

## Key Decision

`Viewport::Inline(15)` is the root cause of viewport bugs (gaps, fixed height). Solution: remove ratatui entirely, use crossterm for direct terminal I/O.

**Architecture:**

```
Native scrollback (stdout)     Managed bottom area (crossterm)
├── Header (ion, version)      ├── Progress (1 line)
├── Chat history               ├── Input (dynamic height)
├── Tool output                └── Status (1 line)
└── Blank line after each
```

## Open Questions (Need Research)

| #   | Question      | Summary                                                        |
| --- | ------------- | -------------------------------------------------------------- |
| Q1  | Diffing       | Is cell/line diffing needed, or is synchronized output enough? |
| Q2  | Sync output   | Does CSI 2026 alone prevent flicker?                           |
| Q3  | Resize        | How to handle terminal resize cleanly?                         |
| Q4  | Streaming     | How to render streaming responses before complete?             |
| Q5  | Selectors     | How to handle modal UI without Viewport?                       |
| Q6  | llm-connector | Replace with own client for Kimi reasoning field, etc?         |

## Session Fixes (2026-01-27)

- Cursor position on wrapped lines
- Option+Arrow word navigation (Alt+b/f)
- Cmd+Arrow visual line navigation
- Input borders TOP|BOTTOM only
- Whitespace-only message rejection
- Cursor invalidation on resize

## Module Health

| Module    | Health | Notes                          |
| --------- | ------ | ------------------------------ |
| tui/      | REWORK | Dropping ratatui for crossterm |
| agent/    | GOOD   | Clean turn loop                |
| provider/ | OK     | llm-connector limitations      |
| tool/     | GOOD   | Orchestrator + spawn           |
| session/  | GOOD   | SQLite persistence + WAL       |
| skill/    | GOOD   | YAML frontmatter               |
| mcp/      | OK     | Needs tests                    |

## Next Session

1. Research open questions Q1-Q6
2. Prototype TUI v2 Phase 1
3. Test streaming response rendering
