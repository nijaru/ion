---
date: 2026-04-30
summary: Contract for safe busy-turn input, queued follow-ups, and active tool-boundary steering.
status: active
---

# Active-Turn Steering Contract

## Decision

Ion keeps **queued follow-up** as the default busy-turn behavior. Opt-in
**active-turn steering** is available only at active tool boundaries, where the
backend can safely inject user guidance before the next provider request.

Do not implement steering as a UI-only trick. Ion cannot mutate a provider request after it has been sent, and it must not insert user text into provider-visible history between an assistant tool call and its required tool result.

## Terms

| Term | Meaning | Status |
| --- | --- | --- |
| Queued follow-up | User text submitted while a turn is busy; it becomes the next normal user turn after the current turn reaches a terminal state. | Implemented and default. |
| Boundary steering | User text submitted while tools are active; the backend consumes it before the next provider request within the same active loop. | Implemented as opt-in `busy_input = "steer"`. |
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

The implemented native contract:

1. Ion accepts steering only when the current TUI state has active tool calls.
2. The Canto backend queues steering text in the active native session.
3. A Canto prompt mutator consumes queued steering at the next provider request
   boundary.
4. The mutator appends non-transcript `ExternalInput` pending/consumed events
   and a model-visible `ContextAdded` steering block after tool results.
5. If no safe tool boundary is visible, Ion downgrades to queued follow-up.

This is intentionally narrower than arbitrary mid-stream steering. It does not
try to steer final assistant streaming, compaction, ACP, or inactive sessions.

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
	SteeringQueued      SteeringOutcome = "queued"
	SteeringAccepted    SteeringOutcome = "accepted"
	SteeringUnsupported SteeringOutcome = "unsupported"
)

type SteeringResult struct {
	Outcome SteeringOutcome
	Notice  string
}

type SteeringSession interface {
	SteerTurn(ctx context.Context, text string) (SteeringResult, error)
}
```

Ion should treat `unsupported` and uncertain errors as queue-worthy unless the
user explicitly asked to cancel/reissue.

## Persistence Ownership

| Data | Owner |
| --- | --- |
| Pending queued follow-ups in current TUI process | Ion app state |
| Pending steering before provider boundary | Ion native backend process state |
| Consumed active-turn steering event | Canto session `ExternalInput` events |
| Provider-visible converted steering context | Canto `ContextAdded` projection |
| Visible queue or steering notice | Ion display projection |
| Consumed steering marker | Canto session `ExternalInput` consumed event |

## Tests

- Submitting text during streaming of a final assistant response stays queued.
- Submitting text while tools are active can steer when `busy_input = "steer"`.
- Steering consumption appends pending/consumed `ExternalInput` events and one
  `ContextAdded` steering block.
- Re-running the mutator does not duplicate already consumed steering.
- Provider-visible history remains valid because steering context is appended
  only at prompt-build boundaries, after required tool results are present.

## Remaining Work

- Add a live/tmux smoke that proves the second provider request sees steering
  after a real long-running tool call.
- Decide whether this Ion-owned mutator should move upstream into Canto if a
  second host needs the same primitive.
- Consider interrupt/reissue as a separate command or setting later.
