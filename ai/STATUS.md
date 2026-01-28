# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | TUI v2 Complete | 2026-01-27 |
| Status     | Testing         | 2026-01-27 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 113 passing     | 2026-01-27 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## TUI Architecture

| Version | Approach                            | Status                     |
| ------- | ----------------------------------- | -------------------------- |
| v1      | Viewport::Inline(15) fixed height   | ABANDONED - gaps, cursor   |
| v2      | Direct crossterm, native scrollback | COMPLETE - ratatui removed |

## TUI v2 Architecture

**Core model:**

1. Chat → insert_before pattern (ScrollUp + print above UI)
2. Bottom UI → cursor positioning + clear/redraw at height-ui_height
3. Resize → clear screen, reprint all chat from `message_list`
4. Exit → clear bottom UI only, chat stays in scrollback

**Implemented:**

- `draw_direct()` - bottom UI rendering
- `render_progress_direct()`, `render_input_direct()`, `render_status_direct()`
- `render_selector_direct()` for picker modals
- `render_markdown()` using pulldown-cmark
- `parse_ansi_line()` for ANSI SGR parsing
- Progress line styling (cyan spinner, dim elapsed time)

**Dependencies removed:**

- ratatui, tui-markdown, ansi-to-tui

**Dependencies added:**

- pulldown-cmark

## Known Issues

- Version line in header still shows 3-space indent (needs investigation)
- Kimi k2.5 API returns malformed JSON (tk-1lso)

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | GOOD   | v2 complete, testing      |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Key Files

- `ai/design/tui-v2.md` - TUI architecture and implementation plan
- `ai/research/ratatui-vs-crossterm-v3.md` - Framework comparison research
