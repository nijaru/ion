# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-27 |
| Status     | Runnable        | 2026-01-27 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 107 passing     | 2026-01-27 |
| Clippy     | 1 warning       | 2026-01-27 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**TUI v2 Implementation** - Drop ratatui, use crossterm directly. See `ai/design/tui-v2.md`.

**Phase 1 Progress:**

- [x] Research Q1-Q6 (see ai/research/tui-\*.md)
- [x] Create terminal.rs wrapper
- [ ] Migrate chat_renderer to StyledLine
- [ ] Create v2 render path in main.rs
- [ ] Remove ratatui dependency

## Research Decisions (2026-01-27)

| Question        | Decision                                                       |
| --------------- | -------------------------------------------------------------- |
| Q1-Q2: Diffing  | No diffing for bottom UI (5-15 lines). Sync output sufficient. |
| Q3: Resize      | Width = full redraw, Height = position adjust only             |
| Q4: Streaming   | Buffer in managed area, commit to scrollback on complete       |
| Q5: Selectors   | Replace bottom UI temporarily (no alternate screen)            |
| Q6: HTTP Client | Replace llm-connector with custom client (phases)              |

## Key Files

| File                                           | Purpose                                        |
| ---------------------------------------------- | ---------------------------------------------- |
| `ai/design/tui-v2.md`                          | Architecture and implementation plan           |
| `ai/research/tui-diffing-research.md`          | Q1-Q2 research                                 |
| `ai/research/tui-resize-streaming-research.md` | Q3-Q4 research                                 |
| `ai/research/tui-selectors-http-research.md`   | Q5-Q6 research                                 |
| `src/tui/terminal.rs`                          | New crossterm wrapper (StyledSpan, StyledLine) |

## Migration Scope

14 files use ratatui. Key files by size:

- render.rs (787 lines) - all UI rendering
- chat_renderer.rs - message formatting
- composer/mod.rs (1086 lines) - input handling
- main.rs - terminal setup and loop

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | REWORK | TUI v2 in progress        |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Next Session

1. Create StyledLine adapter in chat_renderer.rs
2. Add ION_TUI_V2=1 toggle for v2 render path
3. Test basic flow with new rendering
