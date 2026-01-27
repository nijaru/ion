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

TUI input fixes complete. Next: P2 features (cache_control, etc.)

## Priority Queue

**P2 - Important:**

| Issue                   | Notes                    |
| ----------------------- | ------------------------ |
| Anthropic cache_control | Not sent in API requests |
| Kimi k2.5 API error     | OpenRouter, low priority |

**P3 - Polish:**

| Issue           | Notes                    |
| --------------- | ------------------------ |
| Ctrl+R history  | Fuzzy search like shells |
| Pretty markdown | Tables like Claude Code  |

## Session Fixes (2026-01-27)

**Fixed (this session):**

- Cursor position on wrapped lines (was using Ratatui word-wrap, now char-wrap)
- macOS Cmd+Arrow for line start/end (SUPER modifier)
- macOS Option+Arrow for word navigation (ALT modifier)
- Input borders changed to TOP|BOTTOM only

**Fixed (earlier):**

- Paste not working (Event::Paste was swallowed in main.rs)
- Editor not opening with args ("code --wait" now works)
- Selector count display (now shows "1/125" not "125/346")
- Enabled scrolling-regions + synchronized output

**Not Fixed:**

- cache_control not sent in API requests

## TUI Architecture Notes

User intent clarified:

- Chat history: `println!()` to stdout, terminal handles scrollback natively
- Bottom UI (progress, input, status): We manage, stays at bottom
- Don't care if Viewport::Inline or raw crossterm, as long as it works
- If current approach can be fixed, fine. If needs replacement, do that.

**Rendering:**

- ComposerWidget uses custom `render_char_wrapped()` for text (not Ratatui's word-wrap)
- This ensures cursor calculation matches rendering exactly

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
