# `go-host`

Real rewrite attempt for ion's terminal host using:

- `charm.land/bubbletea/v2`
- `charm.land/bubbles/v2`

This is not a toy mock. The goal is to evaluate whether Bubble Tea v2 is a better foundation for ion's host UI than the current custom Rust TUI by building the host loop for real.

## Current scope

The current vertical slice focuses on the exact area that has been unstable in Rust:

- transcript viewport
- fixed footer/status region
- multiline composer
- inline terminal mode
- resize behavior
- user turn submission
- streamed backend events
- transcript tool entries
- backend/session boundary inside the host code

## Run

```bash
cd go-host
go run ./cmd/ion-go
```

## Controls

- `ctrl+s`: submit
- `enter`: newline
- `pgup` / `pgdn`: scroll transcript
- `home` / `end`: jump transcript top/bottom
- mouse wheel: scroll transcript
- `ctrl+c`: quit

## Intent

If this branch continues to feel materially better than the Rust host path, the next steps are:

1. replace the fake backend with a real session/backend boundary
2. shape that boundary for ACP or a native ion runtime
3. keep extending the Go host until we can make the architecture call on an all-Go future from real execution experience
