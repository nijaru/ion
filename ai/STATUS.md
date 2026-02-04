# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | Code Cleanup   | 2026-02-04 |
| Status    | Clean          | 2026-02-04 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 303 passing    | 2026-02-04 |
| Clippy    | pedantic clean | 2026-01-31 |

## Recently Completed

**Code Cleanup** (2026-02-04):

- Moved subscription OAuth providers to `src/provider/subscription/` with warning documentation
- Fixed `take_tail()` double iteration in `message_list.rs` (single-pass via reverse)
- Verified `store.rs` queries already optimized (CTE+ROW_NUMBER for list_recent)
- TUI line handling audit: confirmed clean (selector, help overlay, progress bar)

**Subscription OAuth Organization**:

- `src/provider/subscription/mod.rs` - Module with warning about unofficial nature
- `src/provider/subscription/chatgpt.rs` - ChatGPT Responses API client
- `src/provider/subscription/gemini.rs` - Gemini OAuth client (Antigravity flow)

## Open Blockers

| Provider | Issue                                | Status                     |
| -------- | ------------------------------------ | -------------------------- |
| Gemini   | 403 license error (project-bound)    | Needs testing with new org |
| ChatGPT  | 400 invalid_type for input[*].output | Needs testing with new org |

## Top Priorities

1. Test Gemini OAuth with new subscription/gemini.rs location
2. Test ChatGPT OAuth with new subscription/chatgpt.rs location
3. Implement plugin system when ready (deferred)

## Key References

| Topic                 | Location                                      |
| --------------------- | --------------------------------------------- |
| Gemini OAuth research | ai/research/gemini-oauth-subscription-auth.md |
| OAuth design          | ai/design/oauth-subscriptions.md              |
| Architecture overview | ai/DESIGN.md                                  |
| Plugin architecture   | ai/design/plugin-architecture.md              |
