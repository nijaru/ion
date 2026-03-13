# Go Host Architecture

## Summary

The active host is a Bubble Tea v2 application in `go-host/` responsible for inline terminal interaction only.

It owns:

- transcript rendering
- multiline composer behavior
- footer/status/progress presentation
- scrolling and resize behavior
- mapping backend session events into UI state

It does not own:

- provider logic
- native tool execution policy
- memory/context orchestration
- ACP wire protocol semantics

## Structure

- Bubble Tea `Model` owns host UI state.
- Transcript state is built from committed transcript items plus optional in-flight assistant output.
- Footer state is derived from:
  - current turn state
  - progress/plan events
  - draft/composer state
  - backend/session metadata
- Backends emit typed domain events that the host adapts into Bubble Tea messages.

## UI Model

The host should remain single-pane and transcript-first until the session boundary is stable.

Required visible regions:

1. static header / working context
2. transcript viewport
3. progress/status row
4. multiline composer
5. bottom status line

## Immediate Priorities

1. make the host feel like ion rather than a framework sample
2. keep multiline composer growth/shrink stable
3. preserve scroll position correctly during streaming
4. render tools and approvals cleanly in transcript
5. avoid mixing Bubble Tea internals with domain state
