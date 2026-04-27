---
date: 2026-04-27
summary: Focused review of native core-loop contract enforcement after resume/tool-call failures.
status: active
---

# Core Loop Review

## Verdict

The current empty-assistant protection is not only a replay bandaid. Canto now has both creation-side prevention and projection-side sanitation:

- creation-side prevention: non-streaming and streaming agent paths skip assistant commits without content, reasoning, thinking blocks, or tool calls.
- projection-side sanitation: effective history drops invalid legacy, snapshot, and post-snapshot assistant rows before provider requests.
- Ion follow-up coverage now targets the reported failure shape: first turn persists text, second turn uses a tool, resumed session sends a third turn.

## Findings

| Area | Status | Notes |
| --- | --- | --- |
| Canto write-side assistant commits | OK | `agent/loop.go` and `agent/stream.go` both call `hasAssistantPayload` before appending assistant messages. |
| Canto effective history | OK | `session/rebuilder.go` normalizes effective entries and drops invalid assistant rows, including snapshot-derived rows. |
| Ion live smoke coverage | Improved | `TestLiveSmokeTurnAndToolCall` now reopens the persisted tool session and sends a follow-up turn before optional provider-switch coverage. |
| Fedora/local-api live verification | Blocked | `http://fedora:8080/v1` was unreachable from this process; curl to `/models` failed to connect. |
| Broader contract review | Pending | Need a second pass over duplicate persistence, terminal-state durability, and TUI replay formatting against the new contract. |

## Next Slice

1. Re-run the live local-api smoke when Fedora is reachable.
2. Add or confirm deterministic tests for no duplicate model-visible transcript persistence.
3. Review terminal-state replay paths for cancellation, provider error, provider limit, and retry status.
4. Only after those are green, resume Pi/Codex reference polish and slash/TUI work.
