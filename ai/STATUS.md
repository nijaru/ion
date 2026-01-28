# ion Status

## Current State

| Metric     | Value            | Updated    |
| ---------- | ---------------- | ---------- |
| Phase      | TUI v2 Implement | 2026-01-27 |
| Status     | Ready to build   | 2026-01-27 |
| Toolchain  | stable           | 2026-01-22 |
| Tests      | 108 passing      | 2026-01-27 |
| Visibility | **PUBLIC**       | 2026-01-22 |

## TUI Architecture

| Version | Approach                            | Status                         |
| ------- | ----------------------------------- | ------------------------------ |
| v1      | Viewport::Inline(15) fixed height   | ABANDONED - gaps, cursor bugs  |
| v2      | Direct crossterm, native scrollback | ACTIVE - simple, correct model |

## TUI v2 Architecture

See `ai/design/tui-v2.md` for full plan.

**Core model:**

1. Chat → `println!()` to native scrollback (scroll/search work)
2. Bottom UI → cursor positioning + clear/redraw
3. Resize → clear screen, reprint all chat from `message_list`
4. Exit → clear bottom UI only, chat stays in scrollback

**What we already have:**

- `draw_direct()` - bottom UI rendering
- `render_progress_direct()`, `render_input_direct()`, `render_status_direct()`

**What we need to add:**

- Resize handler that reprints all chat
- Remove ratatui dependency
- Port selectors to direct crossterm

## Key Design Decisions

- Native scrollback for chat (scroll/search work)
- Reprint everything on resize (no debounce, Rust is fast)
- Display history (`message_list`) separate from agent context
- Compaction doesn't affect visible chat

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | WIP    | v2 implementation         |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Key Files

- `ai/design/tui-v2.md` - TUI architecture and implementation plan
- `ai/research/ratatui-vs-crossterm-v3.md` - Framework comparison research
