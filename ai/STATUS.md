# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | OAuth Testing  | 2026-02-04 |
| Status    | In Progress    | 2026-02-04 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 299 passing    | 2026-01-31 |
| Clippy    | pedantic clean | 2026-01-31 |

## In Progress

**OAuth Subscription Auth** (2026-02-04):

- **Gemini OAuth (tk-toyu)**: Ensuring `models/` prefix, mapping `generation_config`, optional `x-goog-user-project` header (now only if `ION_GEMINI_PROJECT` set), and fallback endpoints for 5xx. Needs re-test after changes.
- **ChatGPT OAuth (tk-uqt6)**: Added Codex CLI authorize params (id_token_add_organizations, codex_cli_simplified_flow, originator) and ChatGPT-Account-ID extraction/header. Needs real subscription re-test.

## Open Blockers

| Provider | Issue                                  | Next Step                     |
| -------- | -------------------------------------- | ----------------------------- |
| Gemini   | 500 errors despite matching Gemini CLI | Compare full request/headers  |
| ChatGPT  | Untested with new endpoint             | Test + may need Responses API |

## Top Priorities

1. Test Gemini OAuth with real subscription
2. Test ChatGPT OAuth with new endpoint
3. If ChatGPT fails, implement Responses API format

## Key References

| Topic                 | Location                                      |
| --------------------- | --------------------------------------------- |
| Gemini OAuth research | ai/research/gemini-oauth-subscription-auth.md |
| OAuth design          | ai/design/oauth-subscriptions.md              |
| Architecture overview | ai/DESIGN.md                                  |
