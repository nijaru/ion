# ion Status

## Current State

| Metric    | Value             | Updated    |
| --------- | ----------------- | ---------- |
| Phase     | OAuth Integration | 2026-01-29 |
| Status    | Testing Needed    | 2026-01-29 |
| Toolchain | stable            | 2026-01-22 |
| Tests     | 135 passing       | 2026-01-29 |
| Clippy    | clean             | 2026-01-29 |

## Current Focus

**OAuth subscription support implemented** - needs real-world testing.

```bash
# Test ChatGPT Plus/Pro
ion login openai
ion  # Should show ChatGPT Plus in provider picker

# Test Google AI
ion login google
ion  # Should show Google AI (OAuth) in provider picker
```

## OAuth Implementation

| Task                 | ID      | Status  | Notes                          |
| -------------------- | ------- | ------- | ------------------------------ |
| OAuth infrastructure | tk-7zp8 | done    | PKCE, callback server, storage |
| ChatGPT Plus/Pro     | tk-3a5h | testing | Needs real subscription test   |
| Google AI            | tk-toyu | testing | Needs real subscription test   |

**New modules:**

- `src/auth/` - OAuth PKCE flow, token storage
- CLI: `ion login openai`, `ion login google`, `ion logout`
- Providers: `ChatGptPlus`, `GoogleAi` variants

## Top Priorities

1. **Test OAuth with real subscriptions** - Verify login flow works end-to-end
2. **Provider layer replacement** (tk-aq7x) - Replace llm-connector with native HTTP
3. **App struct decomposition** - Extract TaskState, UiState from App

## Open Bugs

| ID      | Issue                            | Root Cause                                   |
| ------- | -------------------------------- | -------------------------------------------- |
| tk-7aem | Progress line tab switch dupe    | Missing focus event handling                 |
| tk-1lso | Kimi errors on OpenRouter        | llm-connector parsing (fix in provider work) |
| tk-2bk7 | Resize clears pre-ion scrollback | Needs decision on preservation strategy      |

## Module Health

| Module    | Health | Notes                         |
| --------- | ------ | ----------------------------- |
| auth/     | NEW    | OAuth infrastructure complete |
| tui/      | GOOD   | v2 complete, OAuth integrated |
| agent/    | GOOD   | Clean turn loop               |
| provider/ | GOOD   | OAuth providers added         |
| tool/     | GOOD   | Orchestrator + spawn          |
| session/  | GOOD   | SQLite persistence + WAL      |
| skill/    | GOOD   | YAML frontmatter              |
| mcp/      | OK     | Needs tests                   |

## Key References

| Topic                | Location                                  |
| -------------------- | ----------------------------------------- |
| **OAuth research**   | ai/research/oauth-implementations-2026.md |
| OAuth design         | ai/design/oauth-subscriptions.md          |
| Refactoring roadmap  | ai/design/refactoring-roadmap.md          |
| TUI architecture     | ai/design/tui-v2.md                       |
| Provider replacement | ai/design/provider-replacement.md         |
