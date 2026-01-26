# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-26 |
| Status     | Runnable        | 2026-01-26 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 98 passing      | 2026-01-26 |
| Clippy     | 0 warnings      | 2026-01-26 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Sprint

**Sprint 9: Feature Parity & Extensibility** - see ai/SPRINTS.md

| Priority | Task                       | Status  |
| -------- | -------------------------- | ------- |
| 1        | Web fetch tool             | DONE    |
| 2        | Skills YAML frontmatter    | DONE    |
| 3        | Skills progressive load    | DONE    |
| 4        | Subagents                  | DONE    |
| 5        | Anthropic caching          | BLOCKED |
| 6        | Image attachment           | -       |
| 7        | Skill/command autocomplete | -       |
| 8        | File path autocomplete     | -       |

**Target:** Pi + Claude Code feature blend

## Blockers

- **Anthropic caching (tk-268g)**: llm-connector crate doesn't expose cache_control field

## Architecture

| Module    | Health | Notes                           |
| --------- | ------ | ------------------------------- |
| tui/      | GOOD   | Well-structured, 6 submodules   |
| agent/    | GOOD   | Clean turn loop, subagent added |
| provider/ | GOOD   | Multi-provider abstraction      |
| tool/     | GOOD   | Orchestrator + spawn_subagent   |
| session/  | GOOD   | SQLite persistence              |
| skill/    | GOOD   | YAML frontmatter, lazy loading  |
| mcp/      | OK     | Needs tests, cleanup deferred   |

## Recent Completions

**Sprint 9 (2026-01-26)**

- Subagents: spawn_subagent tool, registry from ~/.agents/subagents/
- Thinking display: "thinking" → "thought for Xs", content hidden from chat
- (Earlier) Web fetch, YAML frontmatter, progressive skill loading

## Config

```
~/.agents/           # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```

## Key Gaps vs Competitors

| Gap                     | Priority   | Notes                        |
| ----------------------- | ---------- | ---------------------------- |
| ~~Web fetch~~           | ~~HIGH~~   | DONE                         |
| ~~Skills spec~~         | ~~HIGH~~   | DONE - YAML frontmatter      |
| ~~Progressive load~~    | ~~HIGH~~   | DONE                         |
| ~~Subagents~~           | ~~MEDIUM~~ | DONE - spawn_subagent tool   |
| ~~Thinking display~~    | ~~LOW~~    | DONE - "thought for Xs"      |
| Anthropic caching       | MEDIUM     | BLOCKED - llm-connector      |
| Autocomplete (/, //, @) | MEDIUM     | UX polish - next focus       |
| Image attachment        | MEDIUM     | @image:path syntax (tk-80az) |
