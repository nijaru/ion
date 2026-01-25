# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-25 |
| Status     | Runnable        | 2026-01-25 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 87 passing      | 2026-01-25 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Recent Completions

**UX & Input Handling - COMPLETE** (68c13ab, cd228bf)

- **Cancel/Quit UX**: Esc cancels agent (deliberate), Ctrl+C clears input / double-tap quits
- **Progress hint**: Shows "(tokens · Esc to cancel)" during agent runs
- **Session IDs**: Switched from UUID to `YYYYMMDD-HHMMSS-xxxx` (timestamp + 4-char random)
- **Bracketed paste**: Enabled for proper multiline paste handling
- **Paste blobs**: Large pastes (>5 lines or >500 chars) stored as `[Pasted text #N]`
- **Editor fallback**: Tries vim, nvim, nano, vi, emacs if VISUAL/EDITOR not set
- **Scroll fix**: scroll_offset now clamps when content shrinks (prevents empty space)
- **Help modal**: Height increased to 24 lines

**Tool Security & Performance Review - COMPLETE** (579fa57, 0f5559e)

- **bash**: User-controlled timeout (no hardcoded limit)
- **web_fetch**: SSRF protection (blocks private IPs, localhost, link-local)
- **glob/grep**: Disable symlink following (prevents sandbox escape)
- **edit/write**: UTF-8 safe diff truncation (prevents panic)
- **read**: Single-pass line reading, streaming line count
- **grep**: Batch results per-file with atomics (reduced lock contention)
- **web_fetch**: Stream response with size limit

## Architecture

**Core Agent:**

- TUI + Agent loop
- Multi-provider via llm-connector
- Built-in tools (read, write, glob, grep, bash, edit, list, web_fetch)
- MCP client, Session management (rusqlite)
- Skill system
- AGENTS.md instruction loading

**TUI Stack:**

- ratatui + crossterm, inline viewport with insert_before
- Custom Composer (ropey-backed buffer) with blob storage
- Input layout: 3-char left gutter " > ", 1-char right margin
- Visual line navigation for wrapped text
- Bracketed paste mode for multiline input

## Open Work

| Task     | Priority | Description                                        |
| -------- | -------- | -------------------------------------------------- |
| tk-o8qs  | P2       | Test viewport multiline input edge cases           |
| tk-mmpr  | P3       | Decompose Agent loop into discrete phases          |
| Sprint 6 | Deferred | TUI module refactor (2325 lines → 6 focused files) |

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
