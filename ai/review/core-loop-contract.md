---
date: 2026-04-27
summary: Contract invariants for the native Canto/Ion core loop.
status: active
---

# Core Loop Contract

## Scope

This contract covers the native path:

`Ion TUI / print CLI -> CantoBackend -> Canto agent/session -> provider API`

ACP, subagents, sandboxing, policy classifiers, escalation notifiers, tree navigation, and model-routing expansion are outside this contract unless their events cross the native loop boundary.

## Contract

### 1. Provider-Visible History Is Always Valid

- Canto must never append an assistant model message with no provider-visible payload.
- Valid assistant payload means at least one of:
  - non-empty content
  - non-empty reasoning
  - thinking blocks
  - tool calls
- Canto effective history must also sanitize invalid legacy/snapshot/imported rows before any provider request.
- Tool result messages must reference the matching assistant tool-call ID and retain tool name where available.
- Failed tool completions must carry structured error state as well as provider-visible output, so replay can preserve error UI without parsing text.
- System/developer/context entries must stay in provider-compatible positions after projection.

### 2. Canto Owns Model-Visible Transcript Persistence

- Canto writes model-visible user, assistant, tool-call, and tool-result messages.
- Ion must not persist duplicate model-visible user/assistant/tool transcript events.
- Ion may persist UI-local system/status/routing/subagent metadata only when it is intentionally not part of the provider-visible transcript.
- Slash commands and local UI changes must not create or mutate model-visible transcript history.

### 3. Event Ordering Is Stable

- `TurnStarted` precedes turn-visible streaming/tool/status events.
- Assistant commit is emitted before `TurnFinished`.
- Tool call started precedes matching tool output/result.
- Tool results preserve the ID needed to match pending UI entries and provider history.
- Streaming tool output deltas preserve the tool-use ID needed to attach interleaved output to the right pending row.
- Terminal status events must not race ahead of durable message/tool/error persistence.

### 4. Terminal States Are Durable And Resumable

Every turn ends in exactly one durable terminal shape:

- success: final assistant message and `TurnFinished`
- user cancellation: durable cancellation entry and resumable session
- provider error: durable error entry and resumable session
- provider limit/budget stop: durable stop/error entry plus routing/status metadata
- tool error: tool-result error state, not provider-history corruption
- compaction failure: visible error, no hidden retry loop that mutates history unpredictably

After any terminal state, the session can be resumed and can accept a new user turn without sending invalid provider history.

### 5. Replay Equals Live

- `--continue`, `--resume <id>`, and `/resume` must use the same display-entry renderer as live transcript entries.
- Resume replay must not duplicate user, assistant, tool, system, status, or subagent entries.
- Routine tool display compaction is a UI transform only; provider-visible history keeps the real result.
- Restored transcript spacing must remain readable and equivalent to live transcript spacing.

### 6. Approval Pause/Resume Is Deterministic

- Approval requests pause tool execution without losing pending assistant/tool state.
- Approval decisions attach to the intended request ID only.
- Approval failure surfaces as a session error and does not clear unrelated pending tools.
- Permission modes may change approval policy, but may not change transcript validity or event ordering.

### 7. Print CLI Exercises The Same Loop

- `ion -p`, `ion --print`, `--continue -p`, and `--resume <id> -p` use the native Canto loop.
- Text output returns the final assistant response.
- JSON output is stable enough for smoke tests.
- Piped stdin works as prompt input, and prompt-plus-stdin appends a `<stdin>` context block.

### 8. Runtime Switches Are Atomic Enough For UX

- Newly opened runtime/session handles must close if state save, replay loading, or validation fails before the switch is committed to the model.
- The previous runtime should remain open until the new runtime is fully validated.
- Runtime switch notices and replay rows stay display-only; they must not become model-visible transcript.

## Open Questions

- Thinking block ordering belongs in the contract once Canto has typed provider capability translation.
- Token/cost accounting is contract-adjacent: token usage must persist and replay, but exact provider accounting belongs to provider adapters.
- Event JSONL output is not required yet; add only when an integration needs streaming event semantics.

## Required Regression Coverage

- Canto write-side tests for empty assistant prevention in streaming and non-streaming paths.
- Canto projection tests for legacy/snapshot/post-snapshot invalid assistant rows.
- Ion tests for no duplicate model-visible transcript persistence.
- Ion real-store replay tests for success, cancellation, provider error, retry status, and tool result history.
- Ion backend tests for assistant-before-turn-finished and tool ID propagation.
- Ion app lifecycle tests for runtime switch/resume failure cleanup.
- Live/local smoke for one real tool call, approval, persistence, resume, and follow-up turn.
