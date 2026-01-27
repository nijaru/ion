# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-26 |
| Status     | Runnable        | 2026-01-26 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 104 passing     | 2026-01-26 |
| Clippy     | 0 warnings      | 2026-01-26 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**Viewport Architecture Investigation** - Researching proper inline viewport handling.

### Key Findings

1. Current 15-line fixed viewport causes gaps (startup, after completion, after sending message)
2. Ratatui PR #1964 adds `set_viewport_height()` but not merged, has issues with `scrolling-regions`
3. Codex CLI went through same journey: legacy inline TUI → TUI2 (alt screen) → back to "terminal-native"
4. Their finding: "cooperating with terminal scrollback leads to terminal-dependent behavior, resize failures, content loss"

### Options Under Consideration

| Option                                  | Scrollback | Complexity | Notes                       |
| --------------------------------------- | ---------- | ---------- | --------------------------- |
| A: Fullscreen (like Codex default)      | No         | Low        | Loses native search         |
| B: Inline + PR #1964                    | Yes        | Medium     | Needs fork or wait          |
| C: Raw crossterm                        | Yes        | High       | Full control                |
| D: Ratatui widgets + manual positioning | Yes        | Medium     | Keep widgets, skip viewport |

### Blockers

- No perfect solution for scrollback + dynamic viewport
- Need to decide: accept tradeoffs or implement custom solution

### Design Doc

See `ai/design/viewport-requirements.md` for full requirements.

## Priority Queue

**P2 - Bugs & UX:**

- tk-dxo5: Viewport gaps (root cause of multiple bugs)
- tk-bmd0: Option+Arrow word navigation on Ghostty
- tk-c73y: Token display mismatch
- tk-wtfi: Filter input improvements

**P2 - Features:**

- tk-80az: Image attachment
- tk-ik05, tk-hk6p: Autocomplete

**P3 - Polish:**

- tk-4gm9: Settings selector UI
- tk-9zri: Auto-backticks around pastes config
- tk-6ydy: Tool output format review
- tk-jqe6: Group parallel tool calls
- tk-le7i: Retry countdown timer

## Architecture

| Module    | Health     | Notes                           |
| --------- | ---------- | ------------------------------- |
| tui/      | NEEDS WORK | Viewport issues, see above      |
| agent/    | GOOD       | Clean turn loop, subagent added |
| provider/ | GOOD       | Multi-provider abstraction      |
| tool/     | GOOD       | Orchestrator + spawn_subagent   |
| session/  | GOOD       | SQLite persistence + WAL mode   |
| skill/    | GOOD       | YAML frontmatter, lazy loading  |
| mcp/      | OK         | Needs tests, cleanup deferred   |

## Recent Session (2026-01-26)

- Fixed double empty lines above progress (render at bottom of viewport)
- Fixed large gap below completed response (position UI at viewport bottom)
- Created viewport-requirements.md design doc
- Researched ratatui, crossterm, Codex CLI, pi-mono, OpenTUI approaches
- Added 6 new tasks for discovered issues

## Config

```
~/.agents/           # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```
