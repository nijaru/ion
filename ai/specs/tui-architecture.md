# TUI Architecture

## Current model

ion uses a two-plane inline terminal UI.

### Plane A

Committed history in terminal scrollback.

Includes:

- startup header
- committed system rows
- committed user rows
- committed agent rows
- committed tool rows
- committed error rows

Plane A is not owned by `View()`.

### Plane B

Ephemeral live UI rendered by Bubble Tea.

Includes, in order:

1. in-flight streaming content
2. one blank spacer
3. progress line
4. top separator
5. composer
6. bottom separator
7. status line

## Transcript roles and styling

Current visible conventions:

- user rows: `›`
- system rows: `•`
- agent rows: `•`
- error rows: `×`

Current problems still worth fixing:

- tool-call rows are too transport-shaped and JSON-like
- long tool output needs a better collapsed presentation
- duplicate error visibility between scrollback and progress line needs a clearer rule

## Progress line

Current responsibilities:

- configuration warnings
- running state
- completion state
- error state
- cancel state
- short per-turn stats

Current running shape:

- Bubble Tea spinner
- one-space spinner spacing
- token counters
- elapsed time

Current gaps:

- retry status is not designed yet
- tool/thinking visibility policy is not settled
- line should stay informative without becoming a control panel

## Tool and thinking display policy

Current state:

- thinking text can still surface in Plane B
- tool calls are visible in transcript
- raw tool arguments are too literal
- output truncation policy is not yet a deliberate user-facing setting

Desired direction:

- default to concise progress-line status for thinking
- do not dump reasoning traces by default
- always show the command/tool being run
- show only a bounded subset of tool output by default
- allow configurable verbosity later

Tracked by:

- `tk-h4bp`
- `tk-gmhw`

## Picker behavior

Current picker surfaces:

- provider picker
- model picker
- thinking picker
- session picker

Rules:

- the model picker should privilege `primary` and `fast` presets at the top
- full model search remains fuzzy
- provider scope is catalog-driven
- provider scope shows native providers only
- `Local API` is always visible
- `Custom API` is hidden unless configured or active
- `summary` stays config-only
- `fast` / `primary` are the only UI-exposed presets

## Hotkey source of truth

See `ai/specs/tui-hotkeys.md`.

## Important files

- `internal/app/model.go`
- `internal/app/events.go`
- `internal/app/render.go`
- `internal/app/picker.go`
- `internal/app/session_picker.go`
