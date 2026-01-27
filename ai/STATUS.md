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

**Viewport Architecture** - Research complete, evaluating rustyline-async.

### Recommended: rustyline-async

Use `rustyline-async` crate instead of custom viewport management:

| Feature                         | Status                |
| ------------------------------- | --------------------- |
| Concurrent output while editing | `SharedWriter` (prod) |
| Multi-line input                | Supported             |
| Async/tokio                     | Native                |
| Thread-safe                     | `Send + Sync + Clone` |

**Why not alternatives:**

- reedline's `external_printer` is experimental ("future improvement")
- Custom Codex-style is more work and bug-prone
- rustyline-async solves our exact "output while typing" problem

### Next Steps

1. Spike: Evaluate rustyline-async integration with ratatui widgets
2. If viable, refactor TUI input handling
3. Fallback: Codex-style custom terminal wrapper

### Research Documents

- `ai/research/inline-tui-patterns-2026.md` - Comprehensive patterns
- `ai/research/tui-state-of-art-2026.md` - State of the art
- `ai/research/viewport-investigation-2026-01.md` - Initial investigation
- `ai/research/codex-tui-analysis.md` - Codex approach (fallback)

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
