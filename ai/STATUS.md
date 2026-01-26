# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-25 |
| Status     | Runnable        | 2026-01-25 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 98 passing      | 2026-01-25 |
| Clippy     | 0 warnings      | 2026-01-25 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Sprint

**Sprint 9: Feature Parity & Extensibility** - see ai/SPRINTS.md

| Priority | Task                       | Status      |
| -------- | -------------------------- | ----------- |
| 1        | Web fetch tool             | DONE        |
| 2        | Skills YAML frontmatter    | DONE        |
| 3        | Skills progressive load    | DONE        |
| 4        | Subagents                  | IN PROGRESS |
| 5        | Anthropic caching          | -           |
| 6        | Image attachment           | -           |
| 7        | Skill/command autocomplete | -           |
| 8        | File path autocomplete     | -           |

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
| skill/    | GOOD   | YAML frontmatter, lazy loading |
| mcp/      | OK     | Needs tests, cleanup deferred  |

## Recent Completions

**Codebase Review Verified** (2026-01-25)

- All CRITICAL/IMPORTANT issues from Sprint 7 review confirmed fixed
- Documentation updated in ai/review/SUMMARY.md
- No major refactoring needed - files are cohesive

**Sprint 9 Progress** (4800afb)

- Web fetch: HTML to text conversion via html2text
- Skills: YAML frontmatter parsing (agentskills.io spec)
- Skills: Progressive loading (load prompt on demand)

**Sprint 8 Fixes** (2b00458)

- JSON regex non-greedy, message queue poison recovery
- Session reload shows tools, plan cleared on /clear

## Config

```
~/.config/agents/    # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```

## Key Gaps vs Competitors

| Gap                     | Priority | Notes                       |
| ----------------------- | -------- | --------------------------- |
| ~~Web fetch~~           | ~~HIGH~~ | DONE - html2text conversion |
| ~~Skills spec~~         | ~~HIGH~~ | DONE - YAML frontmatter     |
| ~~Progressive load~~    | ~~HIGH~~ | DONE - lazy skill loading   |
| Subagents               | MEDIUM   | Claude Code, OpenCode have  |
| Anthropic caching       | MEDIUM   | Cost savings                |
| Autocomplete (/, //, @) | MEDIUM   | UX polish                   |
| Timer in progress bar   | LOW      | Show elapsed time           |
| Thinking display        | LOW      | "thought for Xs" indicator  |
