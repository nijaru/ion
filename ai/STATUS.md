# ion Status

## Current State

| Metric    | Value                             | Updated    |
| --------- | --------------------------------- | ---------- |
| Phase     | Feature work                      | 2026-02-10 |
| Status    | Designer removal complete; sandbox next | 2026-02-10 |
| Toolchain | stable                            | 2026-01-22 |
| Tests     | 434 passing                       | 2026-02-09 |
| Clippy    | clean                             | 2026-02-09 |

## Session Summary (2026-02-10)

**System Design Evaluation** (`ai/review/system-design-evaluation-2026-02.md`):
5-topic research evaluation with empirical evidence. Key decisions:

1. **Designer/plan mode**: Remove. Dead code (mark_task never called), costs API call, industry moving away.
2. **System prompt**: Trim 27% (860->630 tokens). Cut 4 redundant Core Principles lines, condense tool routing, shorten output.
3. **Failure tracking**: Build (~300 LOC). Track errors across compaction. No competitor does this. Recovery-Bench shows 57% accuracy drop without structured failure context.
4. **LSP**: Defer (P4). Nuanced.dev 720-run eval found no consistent improvement. grep+bash covers 80-85%.
5. **Feature gaps**: Ship P1s (sandbox, /resume, headless mode). Model quality >> feature count (Terminal-Bench).

Detailed research in `ai/research/*-2026-02.md` (5 files).

## Session Focus (2026-02-10 PM)

- Completed `tk-qhe1`: removed designer auto-plan mode + dead integrations
- Completed `tk-c1q3`: TUI review pass + fixed dead event match arm in `src/tui/session/update.rs`
- Confirmed `/resume` is already implemented (tracked in `tk-qwp3`)
- Next highest-priority open item: `tk-oh88` (OS sandbox execution)

## Session Focus (2026-02-11)

- Active `tk-86lk`: source-level fixes for resume/clear rendering regressions
- In-session `/resume` now forces full reflow after session selection and avoids incremental duplicate insertion
- `/clear` now clears viewport without `ScrollUp(term_height)` blank-row artifacts
- `ChatPosition::Empty` now tracks content until real overflow (no implicit bottom-pinned insert)

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

### P2

- tk-oh88: Implement OS sandbox execution

### P3

- tk-9tig: Custom slash commands via // prefix

### P4 — Deferred

tk-r11l, tk-nyqq, tk-ltyy, tk-5j06, tk-a2s8, tk-o0g7, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-iegz, tk-3fm2

## Key References

| Topic               | Location                                      |
| ------------------- | --------------------------------------------- |
| Architecture        | ai/DESIGN.md                                  |
| System design eval  | ai/review/system-design-evaluation-2026-02.md |
| Architecture review | ai/review/architecture-review-2026-02-06.md   |
| TUI/UX review       | ai/review/tui-ux-review-2026-02-06.md         |
| Code quality audit  | ai/review/code-quality-audit-2026-02-06.md    |
| Cache/prompt review | ai/review/cache-prompt-review-2026-02-09.md   |
| Sprint 16 plan      | ai/SPRINTS.md                                 |
| Permissions v2      | ai/design/permissions-v2.md                   |
| TUI design          | ai/design/tui-v2.md                           |
| Render pipeline     | ai/design/tui-render-pipeline.md              |
