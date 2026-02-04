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

- **Gemini OAuth (tk-toyu)**: Ensuring `models/` prefix, mapping `generation_config`, optional `x-goog-user-project` header (now only if `ION_GEMINI_PROJECT` set), and fallback endpoints for 5xx. Needs re-test after rebuild (fish `unset` doesn’t work).
- **ChatGPT OAuth (tk-uqt6)**: Responses API client now sets `store=false`, with fallback between `/backend-api/codex` and `/backend-api/codex/v1`. Needs re-test after rebuild.

## Open Blockers

| Provider | Issue                                  | Next Step                     |
| -------- | -------------------------------------- | ----------------------------- |
| Gemini   | 403 license error (project-bound)      | Rebuild + ensure no project header |
| ChatGPT  | 400 store must be false                | Rebuild with store=false in payload |

## Top Priorities

1. Rebuild and test Gemini OAuth without project header
2. Rebuild and test ChatGPT Responses API

## Key References

| Topic                 | Location                                      |
| --------------------- | --------------------------------------------- |
| Gemini OAuth research | ai/research/gemini-oauth-subscription-auth.md |
| OAuth design          | ai/design/oauth-subscriptions.md              |
| Architecture overview | ai/DESIGN.md                                  |
