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

**Core Tools Review (tk-q77r) - COMPLETE** (925a520, f87d705, 4d49c1d)

Comprehensive review and SOTA upgrades to all built-in tools:

- **read**: SIMD line counting via `bytecount`, 1MB limit, offset/limit pagination
- **grep**: Rewrote with `grep-searcher` (ripgrep library), 500 match limit
- **glob**: Parallel walking via `ignore::WalkParallel`, 1000 result limit
- **bash**: 100KB output truncation (stdout+stderr combined)
- **edit**: 5MB file size limit, 50KB diff output cap
- **write**: Skip diff for files >1MB, 50KB diff cap

**Large File Protection (tk-su1n) - COMPLETE** (67f6768)

Read tool now has 1MB size limit and offset/limit for line-based reading.

**Web Fetch Tool (tk-1rfr) - COMPLETE** (4034606)

New `web_fetch` tool for HTTP GET requests with URL validation and response truncation.

**Custom System Prompt (tk-bdsv) - COMPLETE** (93760f4)

Users can now set `system_prompt` in config.toml to override default agent prompt.

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

## Remaining Tool Issues

Issues noted during review to address later:

1. **read.rs**: `count_lines_fast` reads entire file to count lines - could use memmap for truly huge files
2. **grep.rs**: Results come in arbitrary order (parallel) - may want sorted option
3. **No tests**: New SOTA implementations lack unit tests
4. **web_fetch**: Timeout hardcoded at 30s - should expose as parameter

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
