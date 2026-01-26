# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-25 |
| Status     | Runnable        | 2026-01-25 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 89 passing      | 2026-01-25 |
| Clippy     | 0 warnings      | 2026-01-25 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Sprint

**Sprint 9: Feature Parity & Extensibility** - see ai/SPRINTS.md

| Priority | Task                       | Status  |
| -------- | -------------------------- | ------- |
| 1        | Web fetch tool             | PENDING |
| 2        | Skills YAML frontmatter    | PENDING |
| 3        | Skills progressive load    | PENDING |
| 4        | Subagents                  | PENDING |
| 5        | Anthropic caching          | PENDING |
| 6        | Image attachment           | PENDING |
| 7        | Skill/command autocomplete | PENDING |
| 8        | File path autocomplete     | PENDING |

**Target:** Pi + Claude Code feature blend

## Architecture

**Current structure is appropriate.** No major reorganization needed.

| Module    | Health | Notes                          |
| --------- | ------ | ------------------------------ |
| tui/      | GOOD   | Well-structured, 6 submodules  |
| agent/    | GOOD   | Clean turn loop, plan support  |
| provider/ | GOOD   | Multi-provider abstraction     |
| tool/     | GOOD   | Orchestrator + approval system |
| session/  | GOOD   | SQLite persistence             |
| skill/    | OK     | Needs YAML frontmatter update  |
| mcp/      | OK     | Needs tests, cleanup deferred  |

## Recent Completions

**Sprint 8 Fixes** (2b00458)

- JSON regex non-greedy, message queue poison recovery
- Session reload shows tools, plan cleared on /clear

**UX Fix** (8d8514c)

- Picker title now shows (selected/filtered) not (filtered/total)

## Config

```
~/.config/agents/    # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```

## Key Gaps vs Competitors

| Gap                     | Priority | Notes                           |
| ----------------------- | -------- | ------------------------------- |
| Web fetch               | HIGH     | All competitors have it         |
| Skills spec compliance  | HIGH     | agentskills.io YAML frontmatter |
| Progressive disclosure  | HIGH     | Only load full skill on demand  |
| Subagents               | MEDIUM   | Claude Code, OpenCode have it   |
| Anthropic caching       | MEDIUM   | Cost savings                    |
| Autocomplete (/, //, @) | MEDIUM   | UX polish                       |
