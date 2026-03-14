# Session Interface

## Summary

The host should talk to one canonical interface, tentatively `AgentSession`.

This interface is ACP-shaped, but uses ion-owned Go types.

## Interface Shape

Expected capabilities:

- open or create a session
- submit a user turn
- cancel an in-flight turn
- resume/load a session
- close the session
- stream typed events back to the host

## Event Model

Core event categories:

- session metadata loaded
- status/progress changed
- plan updated
- assistant text delta
- assistant message committed
- tool call started
- tool result committed
- objective function result (Passed/Failed + Metric)
- approval request opened/resolved
- turn started / turn finished
- recoverable or fatal error

### Swarm Multiplexing

To support parallel agent execution (Swarms) and recursive task trees (RLM), all events carry optional identity fields:

- `AgentID`: Identifies the sub-agent or worker.
- `TraceID`: Identifies the task branch or execution tree.

The host UI is responsible for rendering these events into a coherent parallel display (e.g., nested progress lines or threaded bubbles).

## Rules

- Bubble Tea messages are transport for UI updates, not the domain model.
- Raw ACP JSON structs should not become host state.
- The session interface must support both native ion runtime and external ACP-backed agents.
