# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-25 |
| Status     | Runnable        | 2026-01-25 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 89 passing      | 2026-01-25 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Recent Completions

**Composer Bug Fixes - COMPLETE** (856f37b, 333cce4, 5167084, 0ae7cba)

- **Visual line navigation**: Up/down now use visual lines (fixes wrapped line nav)
- **Scroll clamping**: scroll_offset clamps when content shrinks
- **Cursor-at-end line count**: visual_line_count includes extra line for cursor
- **Cursor state consistency**: cursor_char_idx updated on clamp, not just local
- **History draft blobs**: Resolved content saved to prevent blob loss
- **Zero height guard**: scroll_to_cursor handles visible_height=0
- **Editor error**: Clear message when VISUAL/EDITOR not set (no fallback)

**UX & Input Handling - COMPLETE** (68c13ab, cd228bf)

- **Cancel/Quit UX**: Esc cancels agent, Ctrl+C clears input / double-tap quits
- **Progress hint**: Shows "(tokens · Esc to cancel)" during agent runs
- **Session IDs**: `YYYYMMDD-HHMMSS-xxxx` format
- **Bracketed paste**: Enabled for multiline input
- **Paste blobs**: Large pastes (>5 lines or >500 chars) stored as placeholders

**Tool Security & Performance Review - COMPLETE** (579fa57, 0f5559e)

- **web_fetch**: SSRF protection, streaming response
- **glob/grep**: Disable symlink following
- **edit/write**: UTF-8 safe diff truncation
- **read**: Single-pass line reading
- **grep**: Batch results with atomics

## Architecture

**Core Agent:**

- TUI + Agent loop, Multi-provider via llm-connector
- Built-in tools (read, write, glob, grep, bash, edit, list, web_fetch)
- MCP client, Session management (rusqlite), Skill system

**TUI Stack:**

- ratatui + crossterm, inline viewport with insert_before
- Custom Composer (ropey-backed buffer) with blob storage
- Visual line navigation for wrapped and newline-separated text
- Bracketed paste mode for multiline input

## Open Work

| Task     | Priority | Description                               |
| -------- | -------- | ----------------------------------------- |
| tk-mmpr  | P3       | Decompose Agent loop into discrete phases |
| Sprint 6 | Deferred | TUI module refactor                       |

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
