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

## Priority 1: Anthropic Caching (CRITICAL)

**Task:** tk-268g

**Why critical:** Cache ENTIRE conversation prefix, not just system prompt.

- Turn N: everything from turns 1 to N-1 cached at 90% discount
- Long session (100k+ history): pay full price only for new delta (~2k)
- **50-100x cost savings**, not 10x

**Current blocker:** llm-connector doesn't expose cache_control

**Solution:** Direct Anthropic client with reqwest

- Full control over cache_control on all content blocks
- Can cache system prompt, AGENTS.md, conversation history
- Matches what Claude Code does

**Implementation:**

1. Create `src/provider/anthropic.rs` - direct API client
2. Implement streaming with cache_control support
3. Mark system + history content blocks with `cache_control: ephemeral`
4. Keep llm-connector for other providers

## Active Sprint

**Sprint 9: Feature Parity & Extensibility**

| Priority | Task                       | Status |
| -------- | -------------------------- | ------ |
| 1        | Web fetch tool             | DONE   |
| 2        | Skills YAML frontmatter    | DONE   |
| 3        | Skills progressive load    | DONE   |
| 4        | Subagents                  | DONE   |
| 5        | **Anthropic caching**      | **P1** |
| 6        | Image attachment           | -      |
| 7        | Skill/command autocomplete | -      |
| 8        | File path autocomplete     | -      |

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

| Gap                   | Priority     | Notes                                  |
| --------------------- | ------------ | -------------------------------------- |
| **Anthropic caching** | **CRITICAL** | 50-100x cost savings, needs direct API |
| Image attachment      | MEDIUM       | @image:path syntax (tk-80az)           |
| Autocomplete          | LOW          | UX polish, not blocking                |
