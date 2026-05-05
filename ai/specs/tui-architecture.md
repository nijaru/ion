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

1. in-flight streaming content: thinking marker, live assistant text, active tools
2. one blank spacer
3. progress line
4. top separator
5. composer
6. bottom separator
7. status line

Assistant text streams in Plane B as plain wrapped text. Do not run the full
Markdown renderer on each token delta; incomplete Markdown fragments like open
code fences and half-written lists should stay readable while the turn is still
active. When the assistant message commits to Plane A, render it once through
the Goldmark/GFM Markdown renderer.

### Resize behavior

Inline mode cannot fully own historical scrollback. Full-width shell rows that
were drawn at a wide terminal size can reflow into extra physical rows when the
terminal narrows, especially when moving between displays. Ion keeps live shell
chrome wrap-safe by rendering rows at `terminal_width - 1`, and on width shrink
it clears and redraws the visible screen instead of printing blank scrollback
rows. Do not use `tea.Printf`/`tea.Println` for resize cleanup; those commands
commit unmanaged Plane A scrollback.

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

- assistant text streams live in Plane B as plain wrapped text
- tool calls are visible in transcript
- committed assistant rows use the Markdown renderer
- tool and thinking output visibility are runtime settings that update without
  restarting the TUI

Desired direction:

- keep thinking hidden by default, with collapsed/full modes available through
  `/settings thinking full|collapsed|hidden`
- always show the command/tool being run
- keep routine tool output compact by default, with full output available
  through tool/read/write/bash display settings
- preserve exact provider-visible history; display compaction is UI-only

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

## Slash command surface

Slash commands are a product control surface, not a general feature inventory.
The command catalog is the only source for dispatch, help, picker rows,
autocomplete, active-turn availability, and feature visibility.

Default visible groups:

- session: `/new`, `/clear`, `/resume`, `/session`, `/compact`
- branching: `/tree`, `/fork`; add `/clone` only when it becomes distinct from
  current-point `/fork`
- runtime: `/provider`, `/model`, `/thinking`, `/primary`, `/fast`
- display and inspection: `/settings`, `/tools`, `/cost`, `/status`
- mode: `/mode`; old direct aliases may dispatch but should not dominate help

Reference posture:

- Pi supports `/tree`, `/fork`, and `/clone`; this is the closest fit for Ion's
  session model.
- Codex's `/goal` is feature-gated and depends on long-running task state.
- Claude's `/fork` is now either a branch alias or an experimental forked
  subagent, depending on configuration.

Ion direction:

- prioritize `/tree` and `/fork` readability before adding new command names
- treat `/fork [label]` as the current-point branch/duplicate flow today
- add `/clone` only after earlier-turn forking exists and current-point
  duplication needs a clearer separate command
- add background task commands before `/goal`
- keep `/goal` deferred until goals are durable session/workflow metadata with
  status, pause/resume, token/time accounting, and recovery behavior
- keep future `/side` ephemeral and explicit rather than silently polluting the
  main transcript

## Hotkey source of truth

See `ai/specs/tui-hotkeys.md`.

## Important files

- `internal/app/model.go`
- `internal/app/events.go`
- `internal/app/render.go`
- `internal/app/picker.go`
- `internal/app/session_picker.go`
