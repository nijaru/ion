# ion Status

## Current State

| Metric     | Value             | Updated    |
| ---------- | ----------------- | ---------- |
| Phase      | TUI v3 Design     | 2026-01-27 |
| Status     | Ready to refactor | 2026-01-27 |
| Toolchain  | stable            | 2026-01-22 |
| Tests      | 108 passing       | 2026-01-27 |
| Visibility | **PUBLIC**        | 2026-01-22 |

## TUI Architecture Evolution

| Version | Approach                            | Status                              |
| ------- | ----------------------------------- | ----------------------------------- |
| v1      | Viewport::Inline(15) fixed height   | ABANDONED - gaps, cursor bugs       |
| v2      | Direct crossterm, native scrollback | ISSUES - can't re-render scrollback |
| v3      | Managed history, exit dump          | DESIGNED - ready to implement       |

## Next Session: TUI v3 Implementation

**Start with Phase 0: Codebase Reorganization**

See `ai/design/tui-v3.md` for full plan.

```
src/tui/
├── render/
│   ├── mod.rs          # Render loop coordination
│   ├── chat.rs         # Chat area rendering (v3 core)
│   ├── bottom_ui.rs    # Input, status, progress
│   └── legacy.rs       # Old ratatui path (temporary)
├── viewport.rs         # Format cache, virtual scroll
├── ...
```

**Then Phase 1: Managed Chat Rendering**

- FormattedCache with width-based invalidation
- Virtual scroll with Page Up/Down
- Render visible portion only

## What Claude Code Does (Reference)

1. Renders chat from memory (not native scrollback)
2. Re-renders on resize (~1s debounce)
3. On exit: dumps history to native scrollback
4. Result: clean terminal, searchable history

## Key Design Decisions

- Display history (`message_list`) separate from agent context
- Compaction doesn't affect visible chat
- Only render visible lines (O(visible) not O(total))
- Cache formatted lines, invalidate on width change

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | WIP    | v3 refactor next          |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Key Files

- `ai/design/tui-v3.md` - Full implementation plan
- `ai/design/tui-v2.md` - Previous attempt (reference)
- `ai/research/inline-viewport-scrollback-2026.md` - Research
