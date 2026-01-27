# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-27 |
| Status     | Runnable        | 2026-01-27 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 104 passing     | 2026-01-27 |
| Clippy     | 0 warnings      | 2026-01-27 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**BLOCKER: Cursor position bug on wrapped lines**

- Cursor visual position is wrong on wrapped lines
- Gets worse with more wraps (3rd line = multiple chars off)
- User CAN delete but visual feedback is confusing
- Likely mismatch between `calculate_cursor_pos` and Ratatui's `Paragraph::wrap`

## Priority Queue

**P1 - Blocking (must fix for basic usability):**

| Issue                           | Notes                                              |
| ------------------------------- | -------------------------------------------------- |
| Cursor off-by-one on wrapped    | BLOCKER - visual position wrong, worse on 3rd line |
| Cmd+Left/Right (macOS line nav) | Goes to buffer start/end instead of line start/end |
| Option+Left/Right (macOS word)  | Does not work - escape sequence issue              |

**P2 - Important:**

| Issue                     | Notes                            |
| ------------------------- | -------------------------------- |
| Input borders TOP\|BOTTOM | User requests, better copy-paste |
| Anthropic cache_control   | Not sent in API requests         |
| Kimi k2.5 API error       | OpenRouter, low priority         |

**P3 - Polish:**

| Issue           | Notes                    |
| --------------- | ------------------------ |
| Ctrl+R history  | Fuzzy search like shells |
| Pretty markdown | Tables like Claude Code  |

## Session Fixes (2026-01-27)

**Fixed:**

- Paste not working (Event::Paste was swallowed in main.rs)
- Editor not opening with args ("code --wait" now works)
- Selector count display (now shows "1/125" not "125/346")
- Enabled scrolling-regions + synchronized output

**Not Fixed:**

- Cursor position on wrapped lines (blocking)
- macOS Cmd+Arrow and Option+Arrow keys
- Input borders still ALL instead of TOP|BOTTOM
- cache_control not sent in API requests

## TUI Architecture Notes

User intent clarified:

- Chat history: `println!()` to stdout, terminal handles scrollback natively
- Bottom UI (progress, input, status): We manage, stays at bottom
- Don't care if Viewport::Inline or raw crossterm, as long as it works
- If current approach can be fixed, fine. If needs replacement, do that.

## Architecture

| Module    | Health | Notes                      |
| --------- | ------ | -------------------------- |
| tui/      | BUGGY  | Cursor + keyboard issues   |
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
