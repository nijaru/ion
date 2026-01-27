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

**TUI Flicker Prevention** - Implemented scrolling-regions and synchronized output.

### Completed

Research (tk-xp90) concluded: keep ratatui with `scrolling-regions` feature, use synchronized output.

**Implementation done:**

- Enabled `scrolling-regions` feature in Cargo.toml
- Added `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` around render loop
- `insert_before` now uses scroll regions for flicker-free history insertion

### Remaining Tasks

| ID      | Task                                | Priority | Status |
| ------- | ----------------------------------- | -------- | ------ |
| tk-i5s8 | TUI: Custom bottom area management  | P2       | Open   |
| tk-trb2 | Change input borders to TOP\|BOTTOM | P3       | Open   |

### Research Documents

- `ai/design/tui-architecture.md` - Full design doc (updated with research findings)
- `ai/research/tui-rendering-research.md` - Diffing vs redraw, flicker SOTA
- `ai/research/input-lib-evaluation.md` - Library spike findings

## Priority Queue

**P2 - TUI Redesign:**

- ~~tk-xp90: Research rendering (DONE)~~
- tk-i5s8: TUI implementation (supersedes tk-dxo5)

**P2 - Bugs & UX:**

- tk-bmd0: Option+Arrow word navigation on Ghostty
- tk-c73y: Token display mismatch
- tk-wtfi: Filter input improvements

**P2 - Features:**

- tk-80az: Image attachment
- tk-ik05, tk-hk6p: Autocomplete

**P3 - Polish:**

- tk-trb2: Input borders to TOP|BOTTOM
- tk-4gm9: Settings selector UI
- tk-9zri: Auto-backticks around pastes config
- tk-6ydy: Tool output format review
- tk-jqe6: Group parallel tool calls
- tk-le7i: Retry countdown timer

## Architecture

| Module    | Health   | Notes                           |
| --------- | -------- | ------------------------------- |
| tui/      | IMPROVED | scroll-regions + sync output    |
| agent/    | GOOD     | Clean turn loop, subagent added |
| provider/ | GOOD     | Multi-provider abstraction      |
| tool/     | GOOD     | Orchestrator + spawn_subagent   |
| session/  | GOOD     | SQLite persistence + WAL mode   |
| skill/    | GOOD     | YAML frontmatter, lazy loading  |
| mcp/      | OK       | Needs tests, cleanup deferred   |

## Recent Session (2026-01-27)

- Enabled `scrolling-regions` feature for ratatui (flicker-free `insert_before`)
- Added synchronized output (CSI 2026) to render loop
- Research concluded: keep ratatui, trust cell diffing, use scroll regions

## Previous Session (2026-01-26)

- Deep research: pi-mono, Codex CLI, OpenTUI, reedline, rustyline-async
- Created 6 research docs in ai/research/
- Created ai/design/tui-architecture.md with research findings

## Config

```
~/.agents/           # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```
