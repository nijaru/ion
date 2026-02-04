# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | OAuth Testing  | 2026-02-03 |
| Status    | In Progress    | 2026-02-03 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 299 passing    | 2026-01-31 |
| Clippy    | pedantic clean | 2026-01-31 |

## In Progress

**OAuth Subscription Auth** (2026-02-03):

- **Gemini OAuth (tk-toyu)**: Added Client-Metadata header, removed project field. Using cloudcode-pa.googleapis.com/v1internal:streamGenerateContent. Still getting 500 errors - needs more testing.
- **ChatGPT OAuth (tk-uqt6)**: Fixed endpoint to use `chatgpt.com/backend-api/codex` instead of `api.openai.com/v1`. May need Responses API format instead of Chat Completions. May need ChatGPT-Account-ID header.

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
