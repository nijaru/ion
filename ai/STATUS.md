# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | TUI v2 Complete | 2026-01-27 |
| Status    | Bootstrap Ready | 2026-01-29 |
| Toolchain | stable          | 2026-01-22 |
| Tests     | 128 passing     | 2026-01-29 |
| Clippy    | clean           | 2026-01-29 |

## Top Priorities

1. **OAuth subscription support** (tk-uqt6) - Use ChatGPT Plus/Pro & Google AI subscriptions
   - Design: `ai/design/oauth-subscriptions.md`
   - Unblocks: Free usage with existing subscriptions
   - Reference: Codex CLI (Rust), Gemini CLI (TypeScript)

2. **Provider layer replacement** (tk-aq7x) - Replace llm-connector with native HTTP
   - Design: `ai/design/provider-replacement.md`
   - Unblocks: Anthropic caching, OpenRouter routing

3. **App struct decomposition** - Extract TaskState, AgentContext from App
   - Lower priority, do incrementally while bootstrapping

## OAuth Implementation

New feature: use consumer subscriptions instead of API credits.

| Task                 | ID      | Status | Blocked By |
| -------------------- | ------- | ------ | ---------- |
| OAuth infrastructure | tk-7zp8 | open   | -          |
| ChatGPT Plus/Pro     | tk-3a5h | open   | tk-7zp8    |
| Google AI            | tk-toyu | open   | tk-7zp8    |

**Value:** $20/month flat vs pay-per-token API credits.

## Open Bugs

| ID      | Issue                            | Root Cause                                   |
| ------- | -------------------------------- | -------------------------------------------- |
| tk-7aem | Progress line tab switch dupe    | Missing focus event handling                 |
| tk-1lso | Kimi errors on OpenRouter        | llm-connector parsing (fix in provider work) |
| tk-2bk7 | Resize clears pre-ion scrollback | Needs decision on preservation strategy      |

## Module Health

| Module    | Health   | Notes                      |
| --------- | -------- | -------------------------- |
| tui/      | GOOD     | v2 complete, stabilizing   |
| agent/    | GOOD     | Clean turn loop            |
| provider/ | REFACTOR | Adding OAuth + native HTTP |
| tool/     | GOOD     | Orchestrator + spawn       |
| session/  | GOOD     | SQLite persistence + WAL   |
| skill/    | GOOD     | YAML frontmatter           |
| mcp/      | OK       | Needs tests                |
| auth/     | NEW      | OAuth subscription support |

## Key References

| Topic                | Location                          |
| -------------------- | --------------------------------- |
| OAuth subscriptions  | ai/design/oauth-subscriptions.md  |
| TUI architecture     | ai/design/tui-v2.md               |
| Provider replacement | ai/design/provider-replacement.md |
| Plugin design        | ai/design/plugin-architecture.md  |
| Competitive analysis | ai/research/agent-survey.md       |
