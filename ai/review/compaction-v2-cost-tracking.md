# Code Review: Compaction v2 + Cost Tracking

**Commits:** 827c699..38a8939
**Date:** 2026-02-07
**Status:** 320 tests pass, clippy clean, builds clean

---

## Critical (must fix)

### [ERROR] src/compaction/summarization.rs:148-149 - Byte-index slicing on String can panic on multi-byte UTF-8

The `thinking` field is a `String` that can contain any UTF-8 content. Slicing with `&thinking[..500]` indexes by bytes, not characters. If byte 500 falls in the middle of a multi-byte character (e.g., CJK, emoji, accented characters in code comments), this panics at runtime.

Same issue at lines 160-161 (`&args_str[..200]`) and 172-173 (`&content[..500]`).

The `args_str` case is lower risk since `serde_json::Value::to_string()` produces JSON with non-ASCII escaped as `\uXXXX`, but `thinking` and `content` can absolutely contain raw multi-byte UTF-8.

```rust
// Line 148-149: panics on multi-byte char at byte boundary
let abbreviated = if thinking.len() > 500 {
    format!("{}... [truncated]", &thinking[..500])
```

-> Use char-boundary-safe truncation. Either:

```rust
// Option A: use char_indices to find the boundary
let end = thinking.char_indices()
    .nth(500)
    .map_or(thinking.len(), |(i, _)| i);
format!("{}... [truncated]", &thinking[..end])

// Option B: floor to char boundary (nightly, or manual)
let end = thinking.floor_char_boundary(500);
```

Apply the same fix to all three truncation sites.

---

## Important (should fix)

### [WARN] src/compaction/summarization.rs:1-6 vs src/agent/mod.rs:417 - Summarization uses the user's active model, not a small/fast one

The module doc says "uses a small/fast LLM to produce a structured summary" but `compact_with_summarization` is called with `&session.model`, which is whatever the user selected (could be Claude Opus, GPT-4o, etc.). This means:

1. Summarization cost scales with the most expensive model
2. Latency can be significant on large models
3. The 8000 max_tokens output on an expensive model is wasteful

-> Consider hardcoding a fast/cheap model for summarization (e.g., `claude-sonnet-4-20250514`, `gpt-4o-mini`) or making it configurable. At minimum, update the doc to match reality.

### [WARN] src/agent/mod.rs:157-176 - `compact_with_summary` method is dead code

`Agent::compact_with_summary` is defined as a public method but never called anywhere in the codebase. The agent loop calls `compact_with_summarization` directly. The `/compact` slash command calls `compact_messages` (sync, Tier 1+2 only).

-> Remove it or wire it up to the `/compact` command if Tier 3 is desired there too.

### [WARN] src/tui/session/update.rs:161-166 - `format_k` closure duplicates and is less precise than `format_tokens`

The `format_k` closure in the CompactionStatus handler uses integer division (`n / 1000`) giving "147k", while the existing `format_tokens` util uses `{:.1}k` giving "147.2k". This is a different display format for the same concept, and the closure is defined inline rather than reusing the existing utility.

-> Use `format_tokens` from `crate::tui::util` for consistency.

### [WARN] src/compaction/mod.rs:123-125 - `compact_with_summarization` takes `&mut Vec<Message>` instead of `&mut [Message]`

The function signature requires `&mut Vec<Message>` but it only needs `&mut Vec` for the Tier 3 path where it does `*messages = new_messages`. The Tier 1+2 path via `prune_messages` only needs `&mut [Message]`. This forces callers to have a `Vec` even if they only need mechanical pruning.

This is a minor API issue. The current callers all have Vecs, so it works, but it's a departure from the project convention of preferring `&[T]` over `Vec<T>`.

-> Acceptable as-is given the Tier 3 replacement semantics. Document why `Vec` is needed.

---

## Notes (verify / low confidence)

### [NIT] src/compaction/summarization.rs:69 - Prompt + conversation concatenated in single allocation

The format string `format!("{SUMMARIZATION_PROMPT}\n\n---\n\n{conversation_text}")` creates one large string. For very long conversations, `conversation_text` could be substantial. This is fine for correctness but worth noting that the entire old conversation is serialized to text, then sent as a single user message. No truncation of the conversation text itself before sending to the LLM -- only individual blocks are truncated.

If the old conversation is huge (e.g., 100k tokens of messages), the summarization request itself could exceed the model's context window. Consider capping the conversation text size or adding a safety check.

### [NIT] src/tui/session/update.rs:116-121 - Cost calculation doesn't account for token types some providers report differently

The cost calculation treats `input_tokens` as all charged at the input rate. Some providers (Anthropic) report cache reads/writes separately, which is handled. But `input_tokens` from Anthropic already excludes cached tokens from the total. If a provider reports `input_tokens` inclusive of cache (and also reports cache separately), costs would be double-counted.

Current implementation is likely correct for Anthropic/OpenAI but worth verifying against each provider's billing semantics.

### [NIT] src/tool/builtin/compact.rs - Sentinel pattern means tool result is misleading

The compact tool returns "Compaction will be performed after this tool call completes." as a tool result, which gets pushed into messages _before_ compaction runs. After compaction, this placeholder message persists in the conversation. The LLM sees this in the next turn, which is slightly confusing but harmless since compaction would remove/summarize it in subsequent rounds.

---

## Summary

The changes are well-structured overall. The tiered compaction pipeline is clean, the cost tracking plumbing is thorough, and the code fits existing patterns well. The critical issue is the UTF-8 byte-boundary panic in string truncation -- this will crash on non-ASCII content in thinking/tool outputs. The summarization model selection is the most impactful design concern.
