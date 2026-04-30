---
date: 2026-04-30
summary: Contract for safe busy-turn input, queued follow-ups, and future active-turn steering.
status: active
---

# Active-Turn Steering Contract

## Decision

Ion keeps **queued follow-up** as the default busy-turn behavior. True **active-turn steering** is allowed only after Canto exposes a durable boundary-step contract.

Do not implement steering as a UI-only trick. Ion cannot mutate a provider request after it has been sent, and it must not insert user text into provider-visible history between an assistant tool call and its required tool result.

## Terms

| Term | Meaning | Status |
| --- | --- | --- |
| Queued follow-up | User text submitted while a turn is busy; it becomes the next normal user turn after the current turn reaches a terminal state. | Implemented and default. |
| Boundary steering | User text submitted while a turn is busy; Canto may consume it before the next provider request within the same active loop, after tool-result ordering is valid. | Future backend contract. |
| Interrupt and reissue | User text cancels the active turn, persists cancellation, then starts a new normal turn with the user's steering text. | Optional later mode. |

## Safe Semantics

### Queued follow-up

Keep this path simple:

1. Ion stores busy-turn user input in UI state.
2. Ion shows queued text above the composer/progress shell.
3. `Ctrl+G` recalls queued text into the composer before it is sent.
4. When the active turn finishes, Ion submits the first queued turn as a normal user turn.

This remains the fallback for every ambiguous or unsafe steering case.

### Boundary steering

Boundary steering can only happen at a provider-call boundary. It is valid when the active agent loop has not ended and Canto is about to build another provider request, such as after tool results have been appended.

The future Canto contract should:

1. Accept steering text against the active turn.
2. Persist a non-provider-visible steering event tied to that turn.
3. Consume pending steering only when provider-visible history is valid.
4. Convert consumed steering into provider-visible context in Canto, not Ion.
5. Persist a consumed marker so resume does not apply the same steering twice.
6. Return `queued` instead of `accepted` if the active loop will not make another provider request.

### Interrupt and reissue

This is honest but disruptive. It should be a separate explicit mode or command, not the default Enter behavior:

1. Cancel the active turn.
2. Persist the cancellation terminal state.
3. Submit the user's text as the next normal turn.

## Required Invariants

- Never mutate an already-sent provider request.
- Never insert user text between an assistant tool call and the matching tool result.
- Never let Ion write model-visible steering rows directly.
- Never show a steering/queued message twice in the transcript.
- Resume must preserve pending and consumed steering state.
- If steering cannot be proven safe, downgrade to queued follow-up.

## Interface Shape

The host-facing contract can stay small:

```go
type SteeringOutcome string

const (
	SteeringQueued          SteeringOutcome = "queued"
	SteeringAccepted        SteeringOutcome = "accepted"
	SteeringUnsupported     SteeringOutcome = "unsupported"
	SteeringInterruptNeeded SteeringOutcome = "interrupt_needed"
)

type SteeringResult struct {
	Outcome SteeringOutcome
	Notice  string
}

type SteeringSession interface {
	SteerTurn(ctx context.Context, text string) (SteeringResult, error)
}
```

Ion should treat `unsupported`, `interrupt_needed`, and uncertain errors as queue-worthy unless the user explicitly asked to cancel/reissue.

## Persistence Ownership

| Data | Owner |
| --- | --- |
| Pending queued follow-ups in current TUI process | Ion app state |
| Durable active-turn steering event | Canto |
| Provider-visible converted steering context | Canto effective history / prompt projection |
| Visible queue or steering notice | Ion display projection |
| Consumed steering marker | Canto |

## Tests Before Enabling `/settings busy-input steer`

- Submitting text during streaming of a final assistant response stays queued.
- Submitting text while tools are running is consumed only after all required tool results are appended.
- Steering consumed before a second provider step appears exactly once in the next provider request.
- Resume with pending steering either consumes it once or preserves it as queued; it never disappears silently.
- Resume with consumed steering does not reapply it.
- Provider-visible history remains valid across assistant tool calls, tool results, steering context, errors, cancellation, and compaction.

## Implementation Order

1. Keep current queued follow-up UI as the default.
2. Add Canto steering event/projection tests before exposing an Ion setting.
3. Add `SteerTurn` or equivalent to the native backend only after Canto owns durable semantics.
4. Add `/settings busy-input queue|steer` only after the native contract is tested.
5. Consider interrupt/reissue as a separate command or setting later.
