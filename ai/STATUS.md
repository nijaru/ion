# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-24 |
| Status     | Runnable        | 2026-01-24 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 86 passing      | 2026-01-24 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Recent Completions

**AGENTS.md Support (tk-ncfd) - COMPLETE** (c0fd614)

Implemented instruction loading from multiple locations:

- `~/.ion/AGENTS.md` (ion-specific)
- `~/.config/agents/AGENTS.md` (cross-agent standard)
- `./AGENTS.md` or `./CLAUDE.md` (project-level)
- Mtime-based caching, combined into system prompt

**Visual Line Navigation (tk-gg9m) - COMPLETE** (2b629fd)

Up/down keys now follow visual lines including wraps.

**Context Window (tk-76ua) - COMPLETE** (53757ee)

Compaction now uses model's actual context window from metadata.

## Architecture

**Core Agent:**

- TUI + Agent loop
- Multi-provider via llm-connector
- Built-in tools (read, write, glob, grep, bash, edit, list)
- MCP client, Session management (rusqlite)
- Skill system
- AGENTS.md instruction loading

**TUI Stack:**

- ratatui + crossterm, inline viewport with insert_before
- Custom Composer (ropey-backed buffer)
- Input layout: 3-char left gutter " > ", 1-char right margin
- Visual line navigation for wrapped text

## Config Locations

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
