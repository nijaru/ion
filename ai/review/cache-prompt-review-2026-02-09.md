# Correctness Review: System Prompt, MCP Flag, Prompt Caching, OpenAI Cache Tokens

**Date:** 2026-02-09
**Scope:** 8 commits covering system prompt expansion, MCP tool template flag, Anthropic prompt caching, OpenAI cache token parsing
**Result:** 1 ERROR, 1 WARN, 0 NIT

## Findings

### [ERROR] src/provider/anthropic/response.rs:40 - `input_tokens` missing `#[serde(default)]` causes silent message_delta parse failure

The `Usage` struct requires `input_tokens` for deserialization (no `#[serde(default)]`), but the Anthropic API's `message_delta` event only includes `output_tokens` in standard responses:

```json
{
  "type": "message_delta",
  "delta": { "stop_reason": "end_turn" },
  "usage": { "output_tokens": 15 }
}
```

When `serde_json::from_str::<AnthropicStreamEvent>` encounters this, the entire event fails to deserialize. The error is caught by the `Err(e)` branch in `client.rs:105-110` and logged as a warning, so it doesn't crash -- but the **final output token count is silently lost**.

**Impact:** Usage/cost tracking shows 0 or 1 output tokens instead of the actual count. The `message_start` event provides `output_tokens: 1` (initial), but the `message_delta` with the real total is dropped.

**Fix:** Add `#[serde(default)]` to `input_tokens` in `response::Usage`:

```rust
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
}
```

**Confidence:** 90% -- Anthropic docs clearly show `message_delta` usage with only `output_tokens` for standard/tool-use cases. The web search example shows all fields, suggesting inconsistency across features. If the API always sends `input_tokens` in practice (undocumented), this is not a bug but a fragility.

### [WARN] src/agent/context.rs:106 - `try_lock` cache invalidation can silently fail

`set_has_mcp_tools` uses `try_lock()` on the tokio Mutex to invalidate the render cache. If the lock is held (e.g., by a concurrent `get_system_prompt` or `assemble` call), invalidation silently fails and the stale cached prompt (without the MCP section) persists until the next plan/skill change triggers a re-render.

```rust
if let Ok(mut cache) = self.render_cache.try_lock() {
    *cache = None;
}
```

**Impact:** Currently low -- `set_has_mcp_tools` is only called once during setup (`setup.rs:167`) before the agent is shared via Arc, so contention is impossible. But if this method is ever called at runtime, the cache could serve stale prompts.

**Fix option A:** Include `has_mcp_tools` in the `RenderCache` key and check it during cache validation (robust, self-healing).
**Fix option B:** Document that `set_has_mcp_tools` must only be called during setup (minimal change).

**Confidence:** 95% on the mechanism, low practical risk today.

## Verified Correct

| Area                                                 | Verdict                                                                                                                                                                                                                                                                                                                                |
| ---------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cache breakpoint on second-to-last real user message | Correct. Iterates messages in reverse, skips `role != "user"`, skips tool results (ToolResult-only content), counts real user turns, places breakpoint on `user_turn_count == 2`. Edge cases handled: single user message (no cache), all tool results (no cache), image-only user messages (correctly detected as real user content). |
| Tool result filtering (`is_user_content` check)      | Correct. `matches!(b, ContentBlock::Text { .. } \| ContentBlock::Image { .. })` correctly distinguishes real user content from ToolResult blocks. ToolResult variant has no `cache_control` field, so it can't accidentally receive one.                                                                                               |
| Last tool cache breakpoint                           | Correct. Placed on `tool_vec.last_mut()` after converting all tools. Only fires when tools are non-empty.                                                                                                                                                                                                                              |
| System blocks not cached independently               | Correct by design. Comment explains tool breakpoint creates a cache prefix covering system + tools.                                                                                                                                                                                                                                    |
| `AtomicBool` with `Relaxed` ordering                 | Sufficient. `has_mcp_tools` is set once during init and read later during render. No synchronization with other state is needed -- the render path reads it after acquiring the render_cache mutex which provides the necessary happens-before.                                                                                        |
| Minijinja `{% if has_mcp_tools %}`                   | Correct. Minijinja treats `false` as falsy, so the block is excluded when `has_mcp_tools` is false. Template syntax is valid (verified by test that constructor panics on invalid syntax).                                                                                                                                             |
| OpenAI cache token parsing                           | Correct. `prompt_tokens_details` is `Option` with `#[serde(default)]`, `cached_tokens` defaults to 0. Correctly mapped to `cache_read_tokens` in `Usage`.                                                                                                                                                                              |
| `session_rx` handler (update.rs:201)                 | Correct. The comment explains the fix: `AgentEvent::Finished` already saved the session with the summary, so re-saving here would overwrite `last_task_summary` with `None`. The handler correctly re-saves the updated session (which preserves the summary since it comes from the agent).                                           |
| System prompt content                                | No logic issues. New sections (Task Execution, Tool Usage) are instructional text only.                                                                                                                                                                                                                                                |
