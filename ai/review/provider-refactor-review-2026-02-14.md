# Provider Refactor Review

**Date:** 2026-02-14
**Commit:** 90877ad (HEAD~1)
**Verdict:** Clean refactor, no correctness issues. A few quality items below.

## Summary

Build: clean. Tests: 468 passing. Clippy: clean.

Module organization is consistent across all 4 backends (client.rs + convert.rs + types.rs).
ToolCallAccumulator is well-designed with a minimal API matching both usage patterns.
Tests were moved correctly with no regressions.

## Findings

### WARN: Stale doc references to deleted subscription module

- `src/provider/chatgpt/mod.rs:6` -- "see subscription module docs"
- `src/provider/gemini/mod.rs:6` -- "see subscription module docs"
- subscription/ was deleted in this refactor
- Fix: Remove or rewrite the reference

### WARN: ToolCallAccumulator::drain_finished doc comment is inaccurate

- `src/provider/stream.rs:38` -- doc says "with the given finish reason" but method takes no finish_reason param
- Fix: Change to "Drain all accumulated builders, emitting completed tool calls."

### WARN: HttpClient::with_extra_headers sets auth in default_headers, then build_headers() sets it again per-request

- `src/provider/http/client.rs:151-167` -- bakes auth into default_headers
- `src/provider/http/client.rs:91,125` -- build_headers() adds auth again per-request
- Not a bug (per-request headers override defaults for same key), but wasteful
- Fix: Either skip auth in build_headers when default_headers are set, or don't bake auth into default_headers in with_extra_headers. Low priority.

### WARN: extract_output_text clones Vec<Value> unnecessarily

- `src/provider/chatgpt/convert.rs:200,207` -- `.cloned()` on `&Vec<Value>`
- Clones potentially large JSON arrays when references would work
- Fix: Use `&[Value]` references instead of cloning:
  ```rust
  let empty = Vec::new();
  let output = value.get("output").and_then(Value::as_array).unwrap_or(&empty);
  let items = if output.is_empty() {
      value.get("content").and_then(Value::as_array).unwrap_or(&empty)
  } else {
      output
  };
  ```

### NIT: Gemini stream debug-logs full serialized request

- `src/provider/gemini/client.rs:73-75` -- `serde_json::to_string_pretty(&gemini_request)`
- Serializes entire conversation for debug logging; wasteful if debug is enabled
- Not on the critical path (gated behind tracing::debug), low priority

### NIT: Gemini stream() ignores tool calls (function_call parts)

- `src/provider/gemini/client.rs:95` -- only extracts text, not function_calls
- Pre-existing behavior, not introduced by this refactor
- Worth noting for future tool support on Gemini streaming

## What was done well

- Consistent module layout: all backends follow client.rs + convert.rs (+ types.rs where needed)
- ToolCallAccumulator API is minimal and covers both patterns (Anthropic: insert/remove, OpenAI: get_or_insert/drain)
- Tests moved intact with correct adjustments (removed `client.` prefix, `self` param)
- ChatGPT and Gemini now benefit from HttpClient's timeouts, rate limiting, and Accept header
- Visibility is properly scoped: `pub(crate)` on convert functions, `pub` only on client types
- `crate::` used for cross-module imports; `super::` only for sibling access within same parent
