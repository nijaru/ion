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

**BLOCKER: Cursor position bug on wrapped lines (tk-fbbf)**

- Cursor is ON just-typed char, not AFTER it
- Cannot delete last character on wrapped lines
- Blocks basic editing - HIGH PRIORITY

**TUI Architecture** - User intent clarified:

- Chat history: `println!()` to stdout, terminal handles scrollback natively
- Bottom UI (progress, input, status): We manage, stays at bottom
- Don't care if Viewport::Inline or raw crossterm, as long as it works
- If current approach can be fixed, fine. If needs replacement, do that.

### Pending Fixes

| ID      | Task                                | Priority | Status  |
| ------- | ----------------------------------- | -------- | ------- |
| tk-fbbf | Cursor off-by-one on wrapped lines  | P1       | BLOCKER |
| tk-trb2 | Input borders to TOP\|BOTTOM only   | P2       | Open    |
| tk-268g | Anthropic cache_control in requests | P2       | Open    |
| tk-1lso | Kimi k2.5 API error investigation   | P2       | Open    |

## Session Fixes (2026-01-27)

**Fixed:**

- Paste not working (Event::Paste was swallowed in main.rs)
- Editor not opening with args ("code --wait" now works)
- Selector count display (now shows "1/125" not "125/346")
- Enabled scrolling-regions + synchronized output

**Not Fixed:**

- Cursor position on wrapped lines (blocking)
- Input borders still ALL instead of TOP|BOTTOM
- cache_control not sent in API requests

## Priority Queue

**P1 - Blocking:**

- tk-fbbf: Cursor bug (can't edit on wrapped lines)

**P2 - TUI:**

- tk-trb2: Input borders to TOP|BOTTOM
- tk-i5s8: TUI architecture (if current approach unfixable)

**P2 - Features:**

- tk-268g: Anthropic cache_control
- tk-80az: Image attachment
- tk-ik05, tk-hk6p: Autocomplete

**P3 - Polish:**

- tk-g3dt: Ctrl+R history search
- tk-fsto: Pretty markdown tables

## Architecture

| Module    | Health | Notes                         |
| --------- | ------ | ----------------------------- |
| tui/      | BUGGY  | Cursor bug on wrapped lines   |
| agent/    | GOOD   | Clean turn loop               |
| provider/ | GOOD   | Multi-provider abstraction    |
| tool/     | GOOD   | Orchestrator + spawn_subagent |
| session/  | GOOD   | SQLite persistence + WAL      |
| skill/    | GOOD   | YAML frontmatter              |
| mcp/      | OK     | Needs tests                   |

## Config

```
~/.agents/           # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```
