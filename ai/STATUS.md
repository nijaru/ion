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

**TUI v2 Architecture** - Custom bottom area management, native scrollback.

### Direction

Replace `Viewport::Inline` with custom terminal management:

```
Native scrollback (println!)     ← Terminal handles this
├── Chat history, tool output
│
Managed bottom area (crossterm)  ← We control this
├── Selector UI (when open)
├── Progress line
├── Input area (TOP|BOTTOM borders)
└── Status line
```

**Key principles:**

- Native scrollback is native - just print, terminal handles it
- No Viewport abstractions - manage cursor position directly
- Synchronized output (CSI 2026) for flicker prevention

### Tasks

| ID      | Task                                                 | Priority |
| ------- | ---------------------------------------------------- | -------- |
| tk-xp90 | Research rendering (diffing vs redraw, flicker SOTA) | P2       |
| tk-i5s8 | TUI v2: Custom bottom area management                | P2       |
| tk-trb2 | Change input borders to TOP\|BOTTOM                  | P3       |

### Research Documents

- `ai/design/tui-v2-architecture.md` - Full design doc
- `ai/research/inline-tui-patterns-2026.md` - Pattern research
- `ai/research/input-lib-evaluation.md` - Library spike findings
- `ai/research/tui-state-of-art-2026.md` - SOTA survey

## Priority Queue

**P2 - TUI v2:**

- tk-xp90: Research rendering (diffing vs redraw, flicker SOTA)
- tk-i5s8: TUI v2 implementation
- tk-dxo5: Viewport gaps (will be fixed by TUI v2)

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
