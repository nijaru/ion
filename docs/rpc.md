# Headless JSONL and stdio RPC

Both clients use the same coding application and require an already configured model and credentials. Follow the [setup sequence](../README.md#set-up-a-model) first; requests can incur provider charges.

- `ion --json run 'PROMPT'` submits a headless request and emits JSONL progress records.
- `ion rpc` accepts LF-terminated JSON commands on stdin and publishes JSONL replies and events on stdout.

For CLI grammar, use `ion run --help` and `ion rpc --help`. The current RPC command dispatcher is in [`rpc.rs`](../crates/ion-cli/src/rpc.rs), with read-only access and binding changes in [`rpc/access.rs`](../crates/ion-cli/src/rpc/access.rs) and [`rpc/bindings.rs`](../crates/ion-cli/src/rpc/bindings.rs). These unreleased interfaces may change.

## RPC input and attachments

Commands are JSON objects with a `type` and optional string `id` for reply correlation. For example:

```json
{"type":"prompt","id":"review","message":"Explain this","attachments":[{"type":"file","path":"notes.md"}]}
```

`prompt`, `steer` and `follow_up` take an optional typed `attachments` array:

- `{"type":"file","path":"notes.md"}` detects a supported image or UTF-8 text file.
- `{"type":"image_file","path":"screenshot.png"}` requires an image from a local file.
- `{"type":"image","mime_type":"image/png","data":"BASE64"}` supplies image bytes. Replace `BASE64` with valid base64 for an image matching the declared MIME type.

Paths are literal and relative to the Session directory; RPC does not expand `~`. Each command frame, including JSON, base64 and its final LF, is limited to 8 MiB. A final newline is required; an incomplete frame is rejected. Malformed sources and unknown prompt/attachment fields, including the obsolete `images` field, are rejected rather than silently ignored. Attachment preparation is atomic: an invalid attachment does not submit the rest of the prompt. Accepted messages retain prepared snapshots rather than live source paths; see [saved input](sessions.md#saved-input-and-attachments).

## Streaming content

JSONL `text_delta` and `thinking_delta` events carry a zero-based `block` index. Text and thinking have independent block numbering within each response. Append deltas by channel and block, not to one shared string; updates can interleave. `assistant_committed` replaces provisional content, including text not streamed earlier. `response_restarted` discards provisional content. Block indices are not durable Session coordinates.

A `tool_output_progress` event contains bounded, lossy UTF-8 stdout and stderr previews for a direct tool call, not deltas or a settled result. Replace the previous preview for the same occurrence rather than appending it.

The event projection is defined in [`agent_events.rs`](../crates/ion-cli/src/agent_events.rs). Progress is not a durable acceptance receipt or proof that a tool effect succeeded. Read committed outcomes and reconcile interruptions before retrying work.

## Backpressure and shutdown

Keep reading stdout while submitting commands. Publication and admission are bounded. A `publication_gap` requires reconciliation with current state and committed history; do not assume that every submitted command received custody or that missing progress means no effect occurred. See [`rpc/connection.rs`](../crates/ion-cli/src/rpc/connection.rs) for the current capacity and gap fields.

`abort` requests cancellation; it is not a promise of immediate settlement or rollback. EOF or transport failure stops intake and joins owned work. Final output can still wait for a reader. See [CLI shutdown signals](terminal.md#cli-shutdown-signals) and [recovery limits](sessions.md#files-and-effects-stay-current).
