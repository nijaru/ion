# Review: OpenAI Responses API + TUI Tool Display

**Date:** 2026-02-18
**Diff:** /tmp/ion-review-diff.txt
**Status:** 497 passing, clippy clean at review time

---

## Area 1: Provider Layer

### WARN: `src/provider/openai_responses/convert.rs:39-49` — temperature sent with reasoning models

`build_request` always serializes `temperature` if the caller provides it, even when `reasoning` is also set. OpenAI's Responses API explicitly rejects `temperature` when `reasoning` is active (o1/o3 family). The field is `#[serde(skip_serializing_if = "Option::is_none")]`, so `None` is safe, but if the calling code passes `Some(0.7)` alongside `thinking.enabled = true`, the API will return a 400 error.

Fix: zero out temperature when reasoning is enabled:

```rust
temperature: if reasoning.is_some() { None } else { request.temperature },
```

### WARN: `src/provider/openai_responses/types.rs:50-55` — `InputImage` serializes to wrong shape for Responses API

`ResponseContent::InputImage` uses `#[serde(tag = "type", rename_all = "snake_case")]` which emits `{"type":"input_image","image_url":"data:..."}`. The OpenAI Responses API expects the image object nested under `"image_url"` as `{"type":"input_image","image_url":{"url":"data:..."}}`. The flat string form is the Chat Completions shape, not the Responses API shape. Vision calls will fail silently (no compile error, API 400 at runtime).

Fix: make `InputImage` hold a struct with a `url` field, or use a custom serializer.

### NIT: `src/provider/openai_responses/client.rs:153` — drain loop ignores ordering

The drain of leftover `tool_builders` at stream end iterates a `HashMap`, so tool calls emitted from the drain path have non-deterministic ordering. In practice the drain only fires if `response.completed` / `response.failed` was never received (abnormal termination), so this is low impact, but worth noting.

### NIT: `src/provider/chatgpt/client.rs:24` — trace log added to ChatGPT client duplicates OpenAI client pattern

The `tracing::debug!` for tool calls in the ChatGPT client is a useful addition. The trace line `parsed = ?parsed.as_ref().map(std::mem::discriminant)` is correct and does not allocate the full debug repr. No issues.

---

## Area 2: TUI Layer

### WARN: `src/tui/message_list.rs:606-607` — `toggle_tool_expansion` rebuilds from `meta.header` which already contains the old result line

`ToolMeta::header` is captured at result-receive time via `entry.content_as_markdown()`, which is the entry's text **before** the result is appended. The toggle then rebuilds as `format!("{}\n{}", meta.header, result_content)`. This is correct for a single toggle because `header` only has the tool call line (e.g., `read(src/main.rs)`).

However, if the entry's `parts` are ever modified outside of `toggle_tool_expansion` after `tool_meta` is set (e.g., a second result appended to the same entry via the fallback path), `meta.header` will be stale and the rebuilt entry will show duplicate or missing content. The fallback path at line 757 does set `tool_meta` after `last.append_text`, which means `meta.header` only captures the pre-result text — this is fine for the intended use. Confidence 70% — flagging as uncertain.

### WARN: `src/tui/message_list.rs:365-373` — `tool_name_from_entry` parses the already-formatted display string

`tool_name_from_entry` recovers the tool name by splitting on `'('` from the rendered markdown text. After the grep display format change to `grep(src, "pattern...")`, this parsing is still correct because the name is always before the first `(`. But the function is fragile: if any tool's display name contains `(` before the paren (unlikely now, but possible with future tool renaming via `display_name`), the extracted name will be wrong, silently misrouting `format_result_content`.

This is pre-existing, but the new `display_name` indirection makes it slightly more fragile. Low urgency.

### WARN: `src/tui/session/lifecycle.rs:116-122` — fallback uses `rposition` which may resolve to index 0 even when no tool entry exists

When `tool_entry_map` does not have the `tool_call_id`, the fallback:

```rust
let idx = self.message_list.entries
    .iter()
    .rposition(|e| e.sender == Sender::Tool)
    .unwrap_or(0);
```

`unwrap_or(0)` resolves to entry 0 if there are no `Tool` entries at all. Entry 0 is the first message (typically a user message). The subsequent `entry.tool_meta = Some(meta)` and `entry.append_text` will then silently corrupt the first user message with a tool result. This is only reachable when `tool_call_id` is not found in the map (i.e., malformed session data), but the outcome is data corruption rather than a clean skip.

Fix: guard on `entry.sender == Sender::Tool` before mutating, or return early when `idx == 0 && entries[0].sender != Sender::Tool`.

### WARN: `src/tui/render/layout.rs:84` — layout height changes when `last_task_summary` is cleared mid-session

`has_active_progress = self.is_running || self.last_task_summary.is_some()` means `progress_height` drops by `PROGRESS_HEIGHT` (1 row) when `last_task_summary` is cleared (at start of next task, `tasks.rs:31`). This causes the UI top row to shift by 1, which triggers a `clear_from` sweep. If this happens mid-frame it can cause a 1-row flicker. Previously `progress_height` was always `PROGRESS_HEIGHT + 0 = 1`, so this is a new behavior. Confidence ~75% — may be intentional and handled by the reflow trigger.

### NIT: `src/tui/message_list.rs:107-108` — `display_name` is a no-op

```rust
pub(crate) fn display_name(tool_name: &str) -> &str {
    tool_name
}
```

This is clearly a placeholder for future renaming logic. The function is called in three places. No behavior impact, but it adds indirection for zero benefit today. Either implement it or inline it.

### NIT: `src/tui/run.rs:562-573` — `build_chat_lines` called twice near session load

In `reprint_loaded_session` the trailing-blank-strip logic now duplicates the same strip that happens inside `render_chat_scrollback`. Both call `build_chat_lines` and strip trailing empties. The approach is consistent but worth consolidating if `build_chat_lines` becomes more expensive.

### NIT: `src/tui/chat_renderer.rs:138` — condition order changed, comment explains it

The new condition `line.starts_with(" ✓") || line.starts_with(" ✗") || line.starts_with(" ⎿")` is checked before `syntax_name` to prevent mis-routing. The comment is clear and the logic is correct.

---

## Summary

| Severity | Count | Items                                                                                                                                |
| -------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------ |
| WARN     | 5     | temperature+reasoning, image serialization shape, fallback index 0 corruption, layout height flicker, tool_name_from_entry fragility |
| NIT      | 3     | display_name no-op, duplicate strip, drain ordering                                                                                  |

**Must fix before shipping vision use with OpenAI:** The `InputImage` serialization shape (WARN #2) and the temperature+reasoning conflict (WARN #1) will cause silent 400 errors at runtime.

**Must fix for session replay correctness:** The `rposition.unwrap_or(0)` fallback (WARN #3) can corrupt user messages in malformed session data.

The rest are minor or low-confidence concerns.
