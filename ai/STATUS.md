# ion Status

## Current State

| Metric     | Value            | Updated    |
| ---------- | ---------------- | ---------- |
| Phase      | TUI v2 Migration | 2026-01-27 |
| Status     | WIP - compiles   | 2026-01-27 |
| Toolchain  | stable           | 2026-01-22 |
| Tests      | 107 passing      | 2026-01-27 |
| Visibility | **PUBLIC**       | 2026-01-22 |

## Active Work: TUI v2 Migration

**Goal:** Drop `Viewport::Inline(15)` which causes gaps/fixed height bugs. Use direct crossterm rendering.

**Completed:**

- [x] Research Q1-Q6 (see ai/research/tui-\*.md)
- [x] Create terminal.rs with StyledSpan/StyledLine types
- [x] Add ratatui→crossterm conversion functions
- [x] Remove ratatui Terminal/Viewport from main.rs
- [x] Add draw_direct() method in render.rs
- [x] Add calculate_ui_height() method

**In Progress:**

- [ ] Test basic rendering flow
- [ ] Fix cursor positioning
- [ ] Port selector UI (model/provider/session pickers)
- [ ] Port help overlay
- [ ] Remove remaining ratatui usage

## Key Files Changed

| File                  | Changes                                               |
| --------------------- | ----------------------------------------------------- |
| `src/main.rs`         | Removed ratatui Terminal/Viewport, uses direct stdout |
| `src/tui/terminal.rs` | StyledSpan, StyledLine, conversion functions          |
| `src/tui/render.rs`   | Added draw*direct(), render*\*\_direct() methods      |
| `src/tui/mod.rs`      | Made terminal module public                           |

## Architecture (TUI v2)

```
Native scrollback (stdout)     Managed bottom area (crossterm)
├── Header (ion, version)      ├── Progress (1 line)
├── Chat history               ├── Input (dynamic height)
├── Tool output                └── Status (1 line)
└── Blank line after each
```

**Rendering flow in main.rs:**

1. `app.take_chat_inserts()` - get new chat lines (ratatui Lines)
2. `print_lines_to_scrollback()` - convert and print to stdout
3. `app.draw_direct()` - render bottom UI with crossterm

## Known Issues

1. **Cursor positioning** - cursor_pos calculation may need adjustment
2. **Selector UI** - not ported, uses ratatui Frame
3. **Help overlay** - not ported, uses ratatui Frame
4. **Untested** - needs manual testing

## Research Decisions

| Question        | Decision                                                 |
| --------------- | -------------------------------------------------------- |
| Q1-Q2: Diffing  | No diffing for bottom UI. Sync output sufficient.        |
| Q3: Resize      | Width = full redraw, Height = position adjust only       |
| Q4: Streaming   | Buffer in managed area, commit to scrollback on complete |
| Q5: Selectors   | Replace bottom UI temporarily (no alternate screen)      |
| Q6: HTTP Client | Replace llm-connector with custom client (later phase)   |

## Next Steps

1. **Test the current build** - `cargo run` and verify basic rendering
2. **Fix cursor** - ensure cursor is positioned correctly in input
3. **Port selectors** - model/provider/session pickers need crossterm rendering
4. **Port help overlay** - or simplify to inline text
5. **Remove ratatui** - once all rendering is crossterm-based

## Files Still Using ratatui

```
src/tui/render.rs - draw() still exists (old path), draw_direct() is new
src/tui/chat_renderer.rs - returns Vec<Line> (ratatui type)
src/tui/highlight.rs - uses ratatui styling
src/tui/composer/mod.rs - ComposerWidget uses ratatui
src/tui/filter_input.rs - uses ratatui
src/tui/model_picker.rs, provider_picker.rs, session_picker.rs - use ratatui
```

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | WIP    | v2 migration in progress  |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |
