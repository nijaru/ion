# `go-host`

Real rewrite attempt for ion's terminal host using:

- `charm.land/bubbletea/v2`
- `charm.land/bubbles/v2`

This is not a toy mock. The goal is to evaluate whether Bubble Tea v2 is a better foundation for ion's host UI than the current custom Rust TUI.

## Current scope

The first vertical slice focuses on the exact area that has been unstable in Rust:

- transcript viewport
- fixed footer/status region
- multiline composer
- inline terminal mode
- resize behavior
- user turn submission
- async assistant reply simulation
- backend/session boundary inside the host code

## Run

```bash
cd go-host
go run ./cmd/ion-go
```

## Controls

- `ctrl+s`: submit
- `enter`: newline
- `pgup` / `pgdn` or mouse wheel: scroll transcript
- `ctrl+c`: quit

## Intent

If this branch proves Bubble Tea v2 is a materially better host foundation, the next step is to replace the fake assistant loop with a real agent session boundary and continue the rewrite for real.
