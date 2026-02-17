# OpenAI Responses API Provider Review

**Date:** 2026-02-16
**Files:** `src/provider/openai_responses/{mod,types,convert,client}.rs`
**Verdict:** Good foundation, 2 bugs to fix before merging

---

## Critical (must fix)

### [ERROR] client.rs:139-142 - Duplicate tool call emission in streaming

Every tool call will be emitted **twice** during streaming. The OpenAI Responses API sends both `response.function_call_arguments.done` and `response.output_item.done` for each function call. The code handles both events and emits `StreamEvent::ToolCall` for each:

- Line 121-137: Emits on `ParsedEvent::ToolCallDone` (from `arguments.done`)
- Line 139-142: Emits again on `ParsedEvent::ToolCall` (from `output_item.done`)

The comment on line 140 says "only use if we didn't already emit via arguments.done" but there is no tracking to enforce this. Every tool call with prior delta events will fire twice.

**Fix:** Track emitted call_ids in a `HashSet<String>`. On `ToolCall` from `output_item.done`, skip if already emitted. Or simply remove the `ParsedEvent::ToolCall` arm and rely solely on `ToolCallDone`.

```rust
// Option A: Track emitted IDs
let mut emitted_calls: HashSet<String> = HashSet::new();
// ... in ToolCallDone:
emitted_calls.insert(call_id.clone());
// ... in ToolCall:
if !emitted_calls.contains(&call.id) {
    let _ = tx.send(StreamEvent::ToolCall(call)).await;
}

// Option B (simpler): Remove ParsedEvent::ToolCall arm entirely,
// since ToolCallDone already covers it. Keep output_item.done
// only for text extraction in parse_response_event.
```

### [ERROR] convert.rs:156-168 - Missing `strict: false` in tool definitions

The OpenAI Responses API defaults `strict` to `true` for function tools. When `strict: true`, all schemas must have `additionalProperties: false` and all properties must be in `required`. The tool schemas in this codebase (e.g., `bash`, `read`, `edit`) have optional properties without `additionalProperties: false`, which will cause **400 errors** from the API.

**Fix:** Add `"strict": false` to each tool definition:

```rust
fn build_tools(request: &ChatRequest) -> Vec<Value> {
    request.tools.iter().map(|tool| {
        serde_json::json!({
            "type": "function",
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
            "strict": false,
        })
    }).collect()
}
```

### [ERROR] client.rs:30-63 - `complete()` drops tool calls from response

The non-streaming `complete()` only extracts text via `extract_output_text()`. If the model responds with function calls (which it will, since tools are sent), they are silently dropped. The response will contain only text (possibly empty).

**Fix:** Parse the `output` array for both text and function_call items, building `ContentBlock::ToolCall` entries alongside text. See how `openai_compat/client.rs` handles this with `convert_response`.

---

## Important (should fix)

### [WARN] client.rs:35-55 - Duplicated usage parsing

The usage extraction in `complete()` is a copy-paste of `convert::extract_usage()`. Use the existing function.

**Fix:**

```rust
let usage = value
    .get("usage")
    .map(extract_usage)  // reuse from convert.rs
    .unwrap_or_default();
```

Requires making `extract_usage` `pub(crate)`.

### [WARN] Code duplication with chatgpt/ module

These functions are nearly identical between `openai_responses/convert.rs` and `chatgpt/convert.rs`:

| Function                       | Lines (openai_responses) | Lines (chatgpt) |
| ------------------------------ | ------------------------ | --------------- |
| `build_instructions_and_input` | 46-153                   | 34-139          |
| `build_tools`                  | 156-169                  | 142-155         |
| `extract_output_text`          | 275-305                  | 195-225         |
| `extract_tool_call`            | 307-324                  | 227-244         |

The chatgpt module is simpler (no reasoning, no usage, no incremental streaming), and the openai_responses module extends it. Consider extracting the shared Responses API wire format code into a shared module (e.g., `provider/responses_common/`) that both can use.

### [WARN] types.rs:32 - `role` field on `ResponseInputItem::Message` is `String`

The `role` field should be `&'static str` since it only takes `"user"` or `"assistant"`. This avoids heap allocation on every message conversion.

**Fix:**

```rust
Message {
    role: &'static str,  // "user" or "assistant"
    content: Vec<ResponseContent>,
},
```

### [WARN] convert.rs:17-26 - `budget_tokens` from ThinkingConfig is ignored

The `Reasoning` struct is always built with `effort: "medium"` regardless of the `budget_tokens` value in `ThinkingConfig`. The budget could be mapped to effort levels (low/medium/high) to honor the user's configuration.

### [WARN] convert.rs:190 - `name` field missing on `function_call_arguments.delta`

Per the GitHub issue openai/openai-python#2723, the `name` field on `response.function_call_arguments.delta` events can be `None`/missing. The code handles this with `unwrap_or("")`, which is correct. But on the `done` event (line 201), `name` is also unreliable. The fix already works because `ToolBuilder` tracks name from earlier events, but the fallback path at line 125-137 relies on the `done` event's `name`, which may be empty. If a `done` event arrives without prior deltas (unlikely but possible), you'd get a tool call with an empty name.

---

## Minor (nit)

### [NIT] convert.rs:296 - `let if` chain (edition 2024 feature)

Line 296 uses `if content.get(...) == Some("output_text") && let Some(text) = ...`. This is a let-chain (stabilized in edition 2024). It works, but is worth noting as it won't compile on older editions if the project ever downgrades.

### [NIT] Redundant `TextConfig` always set to `text`

`build_request` always sends `text: Some(TextConfig { format: Some(TextFormat { kind: "text" }) })`. Since `"text"` is the default format, this field can be omitted entirely.

---

## What works well

- Clean separation: types.rs (wire format), convert.rs (mapping), client.rs (HTTP)
- Good test coverage for parsing (12 tests covering all event types)
- Proper use of `ToolBuilder` from shared types for incremental accumulation
- `SseParser` reuse from the shared HTTP module
- `crate::` imports throughout (matches project conventions)
- No `pub use` re-exports except the client struct in mod.rs
- Error handling follows project pattern (anyhow for app)
