# ion Status

## Current State

| Metric    | Value         | Updated    |
| --------- | ------------- | ---------- |
| Phase     | OAuth Testing | 2026-01-29 |
| Status    | Ready to Test | 2026-01-29 |
| Toolchain | stable        | 2026-01-22 |
| Tests     | 135 passing   | 2026-01-29 |
| Clippy    | clean         | 2026-01-29 |

## Current Focus

**OAuth subscription support implemented** - ready for real-world testing.

```bash
ion login chatgpt   # ChatGPT Plus/Pro
ion login gemini    # Google AI (free tier available)
```

## Provider Naming

| ID        | Type  | Display   | Description          |
| --------- | ----- | --------- | -------------------- |
| `openai`  | API   | OpenAI    | API key              |
| `chatgpt` | OAuth | ChatGPT   | Sign in with ChatGPT |
| `google`  | API   | Google AI | API key              |
| `gemini`  | OAuth | Gemini    | Sign in with Google  |

## Recent Changes

- OAuth infrastructure complete (PKCE, callback server, storage)
- CLI: `ion login/logout chatgpt/gemini`
- Providers: `ChatGpt`, `Gemini` variants
- TUI integration with provider picker

## Top Priorities

1. **Test OAuth** with real subscriptions
2. **Provider layer replacement** (tk-aq7x) - native HTTP
3. **App struct decomposition** - TaskState, UiState

## Future: Extensibility

| Task                                         | ID      | Priority |
| -------------------------------------------- | ------- | -------- |
| Extensible API providers (config + plugin)   | tk-o0g7 | p3       |
| Extensible OAuth providers (config + plugin) | tk-a2s8 | p4       |

## Open Bugs

| ID      | Issue                            | Root Cause                   |
| ------- | -------------------------------- | ---------------------------- |
| tk-7aem | Progress line tab switch dupe    | Missing focus event handling |
| tk-1lso | Kimi errors on OpenRouter        | llm-connector parsing        |
| tk-2bk7 | Resize clears pre-ion scrollback | Needs preservation strategy  |

## Module Health

| Module    | Health | Notes                         |
| --------- | ------ | ----------------------------- |
| auth/     | NEW    | OAuth complete, needs testing |
| tui/      | GOOD   | OAuth integrated              |
| agent/    | GOOD   | Clean turn loop               |
| provider/ | GOOD   | OAuth providers added         |
| tool/     | GOOD   | Orchestrator + spawn          |
| session/  | GOOD   | SQLite + WAL                  |
| skill/    | GOOD   | YAML frontmatter              |
| mcp/      | OK     | Needs tests                   |

## Key References

| Topic                | Location                                  |
| -------------------- | ----------------------------------------- |
| OAuth design         | ai/design/oauth-subscriptions.md          |
| OAuth research       | ai/research/oauth-implementations-2026.md |
| Refactoring roadmap  | ai/design/refactoring-roadmap.md          |
| Provider replacement | ai/design/provider-replacement.md         |
