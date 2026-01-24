# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-24 |
| Status     | Runnable        | 2026-01-24 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 75 passing      | 2026-01-24 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**AGENTS.md Support (tk-ncfd) - IN PROGRESS**

Designing context/system prompt architecture:

- Design doc: `ai/design/context-system.md`
- Layered approach: ion-local → global standard → project
- Cross-agent standard: `~/.config/agents/` (see github.com/nijaru/global-agents-config)

**Context % Display - COMPLETE**

Fixed status line showing 0k for max context:

- Now uses model metadata when available, falls back to compaction config (200k)
- Shows percentage when max is known (6ee2bf1)

**Cursor Position (tk-lx9z) - COMPLETE**

Fixed cursor wrapping + added margins (438f1fd, 5ab8c6b)

## Architecture

**Core Agent:**

- TUI + Agent loop
- Multi-provider via llm-connector
- Built-in tools (read, write, glob, grep, bash, edit, list)
- MCP client, Session management (rusqlite)
- Skill system

**TUI Stack:**

- ratatui + crossterm, inline viewport with insert_before
- Custom Composer (ropey-backed buffer)
- Input layout: 3-char left gutter " > ", 1-char right margin

## Known Issues

| Issue              | Status | Notes                                           |
| ------------------ | ------ | ----------------------------------------------- |
| No AGENTS.md       | Active | tk-ncfd - design complete, implementation next  |
| Wrapped navigation | Open   | tk-gg9m - up/down should follow visual lines    |
| Context window     | Open   | tk-76ua - compaction should use model's context |

## Config Locations (Proposed)

```
~/.config/agents/           # Cross-agent standard
├── AGENTS.md               # Global instructions
├── skills/                 # SKILL.md files
└── subagents/              # Subagent definitions

~/.ion/                     # Ion state only
├── config.toml
├── sessions.db
└── cache/
```
