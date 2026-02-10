# ion Status

## Current State

| Metric    | Value                           | Updated    |
| --------- | ------------------------------- | ---------- |
| Phase     | Feature work                    | 2026-02-09 |
| Status    | Non-streaming usage fix shipped | 2026-02-09 |
| Toolchain | stable                          | 2026-01-22 |
| Tests     | 434 passing                     | 2026-02-09 |
| Clippy    | clean                           | 2026-02-09 |

## Session Summary (2026-02-09)

**Non-streaming complete() usage tracking (72eede9):**

- Added `CompletionResponse` type wrapping `Message + Usage`
- Changed `LlmApi::complete()` return type across all 4 backends (Anthropic, OpenAI-compat, ChatGPT Responses, Gemini OAuth)
- All 3 callers now emit `AgentEvent::ProviderUsage`: `complete_with_retry`, `Designer::plan`, `summarize_messages`
- Added `PromptTokensDetails` to OpenAI non-streaming response for cached_tokens parity
- Added `api_usage` field to `SummarizationResult` and `CompactionResult`
- `/cost` now reflects designer, compaction, and non-streaming agent API calls

**Earlier: Agent quality + caching improvements (12 commits), TUI render pipeline refactor**

## Priority Queue

### P3

- tk-9tig: Custom slash commands via // prefix

### P4 — Deferred

tk-r11l, tk-nyqq, tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-iegz, tk-mmup, tk-3fm2

## Key References

| Topic               | Location                                    |
| ------------------- | ------------------------------------------- |
| Architecture        | ai/DESIGN.md                                |
| Architecture review | ai/review/architecture-review-2026-02-06.md |
| TUI/UX review       | ai/review/tui-ux-review-2026-02-06.md       |
| Code quality audit  | ai/review/code-quality-audit-2026-02-06.md  |
| Cache/prompt review | ai/review/cache-prompt-review-2026-02-09.md |
| Sprint 16 plan      | ai/SPRINTS.md                               |
| Permissions v2      | ai/design/permissions-v2.md                 |
| TUI design          | ai/design/tui-v2.md                         |
| Render pipeline     | ai/design/tui-render-pipeline.md            |
