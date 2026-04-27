---
date: 2026-04-27
summary: Target design for Ion's native Canto backend adapter.
status: active
---

# Ion Native Backend Spine

## Purpose

Make `internal/backend/canto` a narrow adapter over Canto's native loop instead of a second loop.

## Current Problem Shape

`CantoBackend.SubmitTurn` currently mixes:

- lazy session materialization
- session metadata updates
- turn context/cancel ownership
- Canto event subscription
- proactive compaction
- Canto `SendStream`
- stream chunk translation
- terminal error emission

`translateEvents` also manually decodes Canto lifecycle payloads and emits product status. This is workable for early integration, but it has too much implicit ordering and too many hidden writer paths for the core loop reliability target.

## Target Responsibilities

The backend adapter owns exactly four things:

1. Bootstrap Canto with Ion config, tools, policy, memory, provider, and prompt processors.
2. Ensure/select the Ion storage session that Canto will use.
3. Submit normal turns through Canto `Runner.SendStream`.
4. Translate Canto events and stream chunks into Ion `internal/session.Event`.

It does not own durable model-visible transcript writes.

## SubmitTurn Target Phases

### Phase 1: Validate Backend

Inputs:

- caller context
- user text

Checks:

- runner initialized
- session handle present
- no impossible config state

No durable writes except errors that already belong to existing initialized state.

### Phase 2: Materialize Session

Rules:

- Only model-visible turns materialize lazy sessions.
- Slash commands and picker state changes never call this path.
- Metadata update is allowed after materialization but must stay metadata-only.

Outputs:

- materialized storage session
- stable session ID

### Phase 3: Open Event Subscription

Rules:

- Subscribe before calling `SendStream` so live UI does not miss early events.
- Subscription lifetime is bound to the turn context.
- The adapter has one event translator goroutine per turn.

Open design question:

- whether repeated `Watch` calls per turn are the best Canto API, or whether a long-lived subscription should be owned by `Open`. Do not change this without Canto review.

### Phase 4: Start Turn Execution

Rules:

- Proactive compaction is a pre-turn host policy using Canto compaction primitives.
- Normal turn execution is `Runner.SendStream`.
- Stream chunks are display-only deltas; durable assistant commits come from Canto session events.
- Provider errors emit Ion `Error` only after Canto has had a chance to append its terminal event.

### Phase 5: Cleanup

Rules:

- cancel function is cleared exactly once.
- close waits for event translator and runner goroutines.
- `Close` must not close the public event channel before in-flight terminal events are drained.

## Event Translation Table

| Canto event/chunk | Ion event | Display durability |
| --- | --- | --- |
| `TurnStarted` | `TurnStarted`, status `Thinking...` | live/status only |
| stream content chunk | `AgentDelta` | live only |
| stream reasoning/thinking chunk | `ThinkingDelta` | live only, compact by default |
| assistant `MessageAdded` | ideally `AgentMessage` when committed | rendered live; Canto persists model-visible row |
| `ToolStarted` | `ToolCallStarted`, status `Running ...` | live tool row; Canto persists lifecycle |
| `ToolOutputDelta` | `ToolOutputDelta` | live only |
| tool result message / completion | `ToolResult` | rendered live; Canto persists model-visible tool result |
| `TurnCompleted` success | `TurnFinished`, status `Ready` | terminal status |
| `TurnCompleted` error | `Error` or status/error pair | terminal display; Canto owns durable terminal |
| child/subagent events | child Ion events | display/subagent breadcrumb only |

Design concern:

- Current translation emits `AgentMessage{Message:""}` on `TurnCompleted` as a commit signal. The refactor should prefer a committed assistant event derived from Canto message events or a clearly named `AssistantCommitted` host event. Empty-message-as-signal is too easy to confuse with invalid assistant content.

## Payload Decoding Rule

Anonymous event payload decoding is allowed only in one translator package/file and must have table-driven tests. If Canto exposes typed accessors for lifecycle events, use those instead.

## Cancellation Semantics

Ion cancellation path:

1. User cancels.
2. Backend cancels turn context.
3. Canto loop stops and records terminal state where possible.
4. Ion persists a display-only cancellation notice if Canto does not produce one.
5. Session remains resumable.

The backend adapter must not return ready just because `CancelTurn` returned nil. Ready requires the event loop to settle or a deliberately UI-only cancellation marker.

## Close Semantics

`Close` should:

- cancel in-flight turn
- close MCP clients
- wait for turn goroutines
- close public event channel once

It should not:

- leak goroutines
- race `TurnFinished` / `Error` against event-channel close
- mutate durable transcript

## Test Plan

- submit turn starts subscription before runner send
- close waits for in-flight goroutines
- cancellation does not leave queued events unread
- `TurnCompleted` does not emit an empty assistant display entry
- Canto tool event payloads translate with IDs/names/results intact
- provider error produces one visible error and leaves backend reusable

## Implementation Slices

1. Introduce private turn-spine helpers without changing behavior.
2. Replace empty `AgentMessage` commit signal with explicit translator behavior.
3. Centralize event payload decoding and add tests.
4. Harden close/cancel tests.
5. Run deterministic backend tests, then Fedora/local-api `-p` smoke.
