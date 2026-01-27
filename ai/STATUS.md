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

**Viewport Architecture** - Research complete, clear path forward identified.

### Key Finding

**Codex CLI doesn't use `Viewport::Inline`** - they implemented custom terminal management with:

1. Manual viewport area via `set_viewport_area()`
2. Scroll regions (DECSTBM) for history insertion
3. `scrolling-regions` ratatui feature for flicker-free updates

This bypasses the fixed viewport limitation entirely.

### Recommended Approach

| Phase | Task                       | Effort  |
| ----- | -------------------------- | ------- |
| 1     | Enable scrolling-regions   | Trivial |
| 2     | Custom terminal wrapper    | Medium  |
| 3     | Dynamic height calculation | Easy    |
| 4     | Synchronized updates       | Easy    |

### Research Documents

- `ai/research/viewport-investigation-2026-01.md` - Full investigation
- `ai/research/codex-tui-analysis.md` - Codex approach (recommended)
- `ai/research/pi-mono-tui-analysis.md` - Pi-mono analysis
- `ai/research/opentui-analysis.md` - OpenTUI analysis
- `ai/design/viewport-requirements.md` - Requirements doc

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
