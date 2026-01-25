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

**Tool Security & Performance Review - COMPLETE** (579fa57, 0f5559e)

Security fixes:

- **bash**: 2-minute timeout prevents infinite hangs
- **web_fetch**: SSRF protection (blocks private IPs, localhost, link-local)
- **glob/grep**: Disable symlink following (prevents sandbox escape)
- **edit/write**: UTF-8 safe diff truncation (prevents panic)

Performance fixes:

- **read**: Single-pass line reading, streaming line count (constant memory)
- **grep**: Batch results per-file with atomics (reduced lock contention)
- **web_fetch**: Stream response with size limit (no full load into memory)

**Core Tools SOTA Upgrades (tk-q77r) - COMPLETE** (925a520, f87d705, 4d49c1d)

- **read**: SIMD line counting via `bytecount`, 1MB limit, offset/limit pagination
- **grep**: Rewrote with `grep-searcher` (ripgrep library), 500 match limit
- **glob**: Parallel walking via `ignore::WalkParallel`, 1000 result limit
- **bash**: 100KB output truncation
- **edit**: 5MB file size limit, 50KB diff output cap
- **write**: Skip diff for files >1MB, 50KB diff cap

**Earlier this session:**

- tk-su1n: Large file protection (67f6768)
- tk-1rfr: Web fetch tool (4034606)
- tk-bdsv: Custom system prompt (93760f4)

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
- Custom Composer (ropey-backed buffer)
- Input layout: 3-char left gutter " > ", 1-char right margin
- Visual line navigation for wrapped text

## Remaining Issues

Minor items for future work:

1. **grep.rs**: Results in arbitrary order (parallel) - may want sorted option
2. **No tests**: New SOTA implementations lack unit tests
3. **web_fetch**: Timeout hardcoded at 30s - could expose as parameter

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
