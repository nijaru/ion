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
- approval request opened/resolved
- turn started / turn finished
- recoverable or fatal error

## Rules

- Bubble Tea messages are transport for UI updates, not the domain model.
- Raw ACP JSON structs should not become host state.
- The session interface must support both native ion runtime and external ACP-backed agents.
