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

TUI input bugs and viewport architecture.

## P1 Bugs

| Bug                         | Status  | Notes                                  |
| --------------------------- | ------- | -------------------------------------- |
| Cursor broken after resize  | **NEW** | `last_width` stale on resize           |
| Whitespace-only msg allowed | **NEW** | Should reject empty/whitespace content |

## Priority Queue

**P1 - Architecture:**

| Issue               | Notes                                    |
| ------------------- | ---------------------------------------- |
| Viewport/scrollback | Native terminal scroll vs managed        |
| Resize handling     | Width changes break cursor, need refresh |

**P2 - Important:**

| Issue                 | Notes                   |
| --------------------- | ----------------------- |
| Pretty markdown       | Tables like Claude Code |
| Anthropic cache       | 50-100x cost savings    |
| Kimi k2.5 API error   | OpenRouter, investigate |
| Image attachment      | @image:path syntax      |
| File/cmd autocomplete | @ and / triggers        |

**P3 - Polish:**

| Issue             | Notes                    |
| ----------------- | ------------------------ |
| Ctrl+R history    | Fuzzy search like shells |
| Settings selector | UI for config            |

## Session Fixes (2026-01-27)

**Fixed:**

- Cursor position on wrapped lines (word-wrap algorithm now matches Ratatui)
- Option+Arrow word navigation (handles Alt+b/f sent by terminals)
- Cmd+Arrow visual line navigation (SUPER modifier + visual line methods)
- Input borders changed to TOP|BOTTOM only
- Event debug logging (ION_DEBUG_EVENTS=1)

**Key insight:** Terminals send Alt+b/f for Option+Arrow, not Arrow+ALT modifier.

## TUI Architecture

**Current approach:** Viewport::Inline with `insert_before` for history.

**Key decisions from research:**

1. Keep native terminal scrollback (don't manage own)
2. Use ratatui's `scrolling-regions` feature for flicker-free insert
3. Use synchronized output unconditionally
4. Trust ratatui cell diffing (~2% overhead, 98% is I/O)

**Rendering:**

- Uses Ratatui's `Paragraph::wrap(Wrap { trim: false })` for word-wrap
- `build_visual_lines()` computes line boundaries with same algorithm
- `calculate_cursor_pos()` uses `build_visual_lines()` for consistency
- Input box: TOP|BOTTOM borders only

**Keyboard handling:**

- Alt+b/f → word left/right (Option+Arrow on macOS)
- SUPER+Arrow → visual line start/end (Cmd+Arrow on macOS)
- Ctrl+a/e → line start/end (Emacs)

## Architecture

| Module    | Health | Notes                      |
| --------- | ------ | -------------------------- |
| tui/      | OK     | Resize bugs, viewport work |
| agent/    | GOOD   | Clean turn loop            |
| provider/ | GOOD   | Multi-provider abstraction |
| tool/     | GOOD   | Orchestrator + spawn       |
| session/  | GOOD   | SQLite persistence + WAL   |
| skill/    | GOOD   | YAML frontmatter           |
| mcp/      | OK     | Needs tests                |

## Config

```
~/.agents/           # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```
