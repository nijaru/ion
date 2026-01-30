# ion Status

## Current State

| Metric    | Value         | Updated    |
| --------- | ------------- | ---------- |
| Phase     | Provider Done | 2026-01-29 |
| Status    | Ready to Test | 2026-01-29 |
| Toolchain | stable        | 2026-01-22 |
| Tests     | 202 passing   | 2026-01-29 |
| Clippy    | clean         | 2026-01-29 |

## Just Completed

**Code review fixes** (2026-01-29):

- AuthConfig Debug redacts credentials
- HTTP client fails fast on invalid auth headers
- Gemini tool calls use function name and unique IDs

**Provider layer replacement** - native HTTP implementations:

- Anthropic Messages API with `cache_control` support
- OpenAI-compatible with provider quirks (OpenRouter routing, Kimi reasoning)
- Removed llm-connector dependency

## Current Focus

**OAuth testing** - infrastructure complete, needs real-world verification:

```bash
ion login chatgpt   # ChatGPT Plus/Pro
ion login gemini    # Google AI (free tier available)
```

## Decision Needed

**OAuth Client IDs:** Using public client IDs from Codex CLI and Gemini CLI.

1. Keep borrowed IDs - works now, may break if upstream changes
2. Register our own - more stable, requires developer accounts

## Open Bugs

| ID      | Issue                         | Root Cause                   |
| ------- | ----------------------------- | ---------------------------- |
| tk-7aem | Progress line tab switch dupe | Missing focus event handling |
| tk-u25b | Errors not visible            | Chat rendering timing        |
| tk-2bk7 | Resize clears scrollback      | Needs preservation strategy  |

## Module Health

| Module    | Files | Lines | Health | Notes                              |
| --------- | ----- | ----- | ------ | ---------------------------------- |
| provider/ | 18    | ~2500 | GOOD   | Native HTTP, 3 backends            |
| tui/      | 20    | ~6700 | OK     | render.rs large (820), needs split |
| agent/    | 6     | ~1500 | GOOD   | Clean turn loop                    |
| tool/     | 15    | ~2500 | GOOD   | Orchestrator + spawn               |
| auth/     | 5     | ~800  | NEW    | OAuth complete, needs testing      |
| session/  | 3     | ~600  | GOOD   | SQLite + WAL                       |
| skill/    | 3     | ~400  | GOOD   | YAML frontmatter                   |
| mcp/      | 2     | ~300  | OK     | Needs tests                        |

## Top Priorities

1. Test OAuth with real subscriptions
2. Fix error visibility bug (tk-u25b)
3. Split large TUI files

## Key References

| Topic                 | Location                         |
| --------------------- | -------------------------------- |
| Architecture overview | ai/DESIGN.md                     |
| OAuth design          | ai/design/oauth-subscriptions.md |
| Module organization   | ai/design/module-structure.md    |
