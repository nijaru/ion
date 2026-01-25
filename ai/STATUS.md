# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-25 |
| Status     | Runnable        | 2026-01-25 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 89 passing      | 2026-01-25 |
| Clippy     | 13 warnings     | 2026-01-25 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Sprint

**Sprint 7: Codebase Review & Refactor** - see ai/SPRINTS.md

| Task | Description             | Status         |
| ---- | ----------------------- | -------------- |
| S7-1 | Fix clippy warnings     | PENDING (next) |
| S7-2 | Review tui/ module      | PENDING        |
| S7-3 | Review agent/ module    | PENDING        |
| S7-4 | Review provider/ module | PENDING        |
| S7-5 | Review misc modules     | PENDING        |
| S7-6 | Performance profiling   | PENDING        |
| S7-7 | Consolidate & plan      | PENDING        |

## Recent Completions

**Composer Bug Fixes** (856f37b, 333cce4, 5167084, 0ae7cba)

- Visual line navigation, scroll clamping, cursor-at-end line count
- Cursor state consistency, history draft blobs, zero height guard
- Editor error message (no fallback)

**UX & Input Handling** (68c13ab, cd228bf)

- Cancel/Quit UX, progress hint, session IDs, bracketed paste, paste blobs

**Tool Security & Performance** (579fa57, 0f5559e)

- SSRF protection, symlink following disabled, UTF-8 safe truncation
- Single-pass reading, batch grep results

## Architecture

**Core:** TUI + Agent loop, multi-provider, built-in tools, MCP client, sessions, skills

**TUI:** ratatui + crossterm, inline viewport, custom Composer with blob storage

## Config

```
~/.config/agents/    # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```
