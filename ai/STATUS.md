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

TUI input completely fixed. All P1 blockers resolved.

## Session Fixes (2026-01-27)

**Fixed:**

- Cursor position on wrapped lines (word-wrap algorithm now matches Ratatui)
- Option+Arrow word navigation (handles Alt+b/f sent by terminals)
- Cmd+Arrow visual line navigation (SUPER modifier + visual line methods)
- Input borders changed to TOP|BOTTOM only
- Event debug logging (ION_DEBUG_EVENTS=1)

**Key insight:** Terminals send Alt+b/f for Option+Arrow, not Arrow+ALT modifier.

## Priority Queue

**P2 - Important:**

| Issue                 | Notes                   |
| --------------------- | ----------------------- |
| Anthropic cache       | 50-100x cost savings    |
| Kimi k2.5 API error   | OpenRouter, investigate |
| Image attachment      | @image:path syntax      |
| File/cmd autocomplete | @ and / triggers        |

**P3 - Polish:**

| Issue             | Notes                    |
| ----------------- | ------------------------ |
| Ctrl+R history    | Fuzzy search like shells |
| Pretty markdown   | Tables like Claude Code  |
| Settings selector | UI for config            |

## TUI Architecture

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
| tui/      | GOOD   | Input bugs fixed           |
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
