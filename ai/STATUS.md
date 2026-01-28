# ion Status

## Current State

| Metric     | Value             | Updated    |
| ---------- | ----------------- | ---------- |
| Phase      | TUI v3 Design     | 2026-01-27 |
| Status     | Research complete | 2026-01-27 |
| Toolchain  | stable            | 2026-01-22 |
| Tests      | 108 passing       | 2026-01-27 |
| Visibility | **PUBLIC**        | 2026-01-22 |

## TUI Architecture Evolution

### v1: Viewport::Inline(15) - ABANDONED

- Fixed 15-line viewport caused gaps and cursor bugs
- Dynamic input height didn't fit fixed viewport

### v2: Direct crossterm, native scrollback - ISSUES FOUND

- Chat printed to native scrollback via println
- Bottom UI managed with cursor positioning
- **Problems discovered:**
  - Can't re-render scrollback content (terminal owns it)
  - Resize causes terminal rewrap we can't control
  - Scroll/print/clear logic conflicts
  - Header not showing, visual artifacts

### v3: Managed history with exit dump - DESIGNED

- Manage ALL rendering ourselves (chat + UI)
- Keep chat history in memory, render visible portion
- Re-render on resize at new width (like Claude Code)
- Page Up/Down for scrolling during session
- On exit: dump formatted history to native scrollback
- See: `ai/design/tui-v3.md`

## What Claude Code Does (Observed)

1. **During session**: Renders chat from memory (not native scrollback)
2. **On resize**: ~1s debounce, re-renders at new width
3. **On exit**: Dumps history to native scrollback, cleans up UI
4. Result: Terminal prompt appears cleanly, history searchable

## Next Steps (TUI v3 Implementation)

1. **Add chat_scroll_offset** to App for virtual scrolling
2. **Implement render_chat_area()** - format + render visible portion
3. **Handle Page Up/Down** for scrolling
4. **Debounce resize** with full re-render
5. **Exit cleanup** - clear screen, dump history

## Known Working (from v2 work)

- [x] Cursor positioning after " > " prompt
- [x] Display-width word wrap (unicode_width)
- [x] Word wrap matches cursor calculation (build_visual_lines)
- [x] Width decrease detection (ClearType::All)
- [x] UI height change detection (last_ui_start)

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | WIP    | v3 design ready           |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Key Design Docs

- `ai/design/tui-v3.md` - Managed history architecture
- `ai/design/tui-v2.md` - Previous attempt (reference)
- `ai/research/inline-viewport-scrollback-2026.md` - Research on approaches
