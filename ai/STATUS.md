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

- **Gemini OAuth (tk-toyu)**: Switched to Antigravity OAuth (fixed port 51121 `/oauth-callback`), added `loadCodeAssist` project resolution + request wrapper `project`. Needs re-test after rebuild.
- **ChatGPT OAuth (tk-uqt6)**: Responses API client now sets `store=false` and sends `function_call_output.output` as string (not object). Needs re-test after rebuild.

## Open Blockers

| Provider | Issue                                  | Next Step                     |
| -------- | -------------------------------------- | ----------------------------- |
| Gemini   | 403 license error (project-bound)      | Re-test with Antigravity OAuth + project wrapper |
| ChatGPT  | 400 invalid_type for input[*].output   | Rebuild with string output fix |

## Top Priorities

1. Rebuild and test Gemini OAuth without project header
2. Rebuild and test ChatGPT Responses API

## Key References

| Topic                 | Location                                      |
| --------------------- | --------------------------------------------- |
| Gemini OAuth research | ai/research/gemini-oauth-subscription-auth.md |
| OAuth design          | ai/design/oauth-subscriptions.md              |
| Architecture overview | ai/DESIGN.md                                  |
