# Code Review: OpenAI Responses API + TUI Display Improvements

**Date:** 2026-02-16
**Scope:** 13 commits, ~1355 lines
**Reviewer:** claude-opus-4-6

## Summary

Clean implementation overall. The new `openai_responses` provider follows the established `chatgpt` pattern well, with meaningful additions (incremental tool call streaming, reasoning support, usage tracking). TUI display name mapping and session replay fix are well-structured. A few issues worth addressing.

## Critical

None.

## Important

### [WARN] `src/provider/openai_responses/convert.rs:105-134` - Assistant message ordering in multi-turn input

When an assistant message has both text and tool calls, `FunctionCall` items are pushed to `input` during iteration but the text `Message` is pushed after the loop. Result: `[FunctionCall, Message{text}]` instead of `[Message{text}, FunctionCall]`.

Pre-existing: identical pattern in `src/provider/chatgpt/convert.rs:85-115`. Low practical impact (tool calls rarely coexist with text in the same message), but technically sends items out of order to the API.

```rust
// Fix: push text message BEFORE processing tool calls, or push FunctionCalls after
```

### [WARN] `src/provider/openai_responses/convert.rs:178-247` - `response.incomplete` not handled

The parser handles `response.completed` and `response.failed` but not `response.incomplete` (sent when max_output_tokens is hit or content filter triggers). The stream would silently end without indicating why output was truncated.

Pre-existing: same gap in `src/provider/chatgpt/convert.rs`.

```rust
// Fix: add to parse_response_event match:
"response.incomplete" => {
    let reason = value
        .get("response")
        .and_then(|r| r.get("incomplete_details"))
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    // Could emit as Usage (with partial usage) + Done, or as a warning
    Some(ParsedEvent::Done)
}
```

### [WARN] `src/tui/message_list.rs:705-786` - `load_from_messages` is dead code

Never called anywhere. The session replay is now handled by `src/tui/session/lifecycle.rs:load_session`. This function also lacks the `display_name` mapping and `format_result_content` logic, so if anyone called it, tool results would display inconsistently.

```
-> Remove or mark #[cfg(test)] if keeping for tests only.
```

### [WARN] `src/tui/session/setup.rs:55` - Trace logging in debug builds may impact performance

`ion=trace` in debug builds means every SSE event in every streaming response gets logged. Only 2 trace call sites now, but this filter would catch any future trace! calls too.

```
-> Consider `ion=debug` for debug builds, `ion=trace` only with ION_LOG=trace or similar.
```

## Nits

### [NIT] `src/provider/openai_responses/convert.rs` + `src/provider/chatgpt/convert.rs` - Significant code duplication

~80% of `build_instructions_and_input`, `extract_output_text`, and `extract_tool_call` are identical between the two providers. The types (`ResponseInputItem`, `ResponseContent`) are also duplicated. Not blocking, but worth extracting a shared `responses_common` module if a third Responses API-based provider ever appears.

### [NIT] `src/provider/openai_responses/types.rs:34-39` - `role` field could be `&'static str`

`ResponseInputItem::Message.role` is always `"user"` or `"assistant"` -- could be `&'static str` instead of `String` to avoid allocation. Same for the chatgpt provider.

### [NIT] `src/tui/chat_renderer.rs:109` - `search` syntax detection is slightly wrong for glob

`display_name` maps both `grep` and `glob` to `"search"`. The renderer tries `detect_syntax(path)` for `"search"`, which would try to infer syntax from grep patterns (harmless, returns None) and glob patterns like `**/*.rs` (would return "Rust"). In practice, glob results use `Collapsed` style so no content lines are syntax-highlighted -- the issue is latent but logically incorrect.

## Observations (no action needed)

- Provider structure is clean: `mod.rs` + `client.rs` + `convert.rs` + `types.rs` mirrors `chatgpt/` exactly
- Good dedup avoidance via `emitted_tool_ids` + `tool_builders` in streaming client
- Test coverage is thorough (14 tests for convert.rs covering all event types and edge cases)
- `display_name` mapping is consistently applied in both live display and session replay paths
- `format_result_content` being made `pub(crate)` and reused in lifecycle.rs is the right call
