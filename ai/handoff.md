# Handoff (2026-03-27)

## Current state

- Build: passing
- Tests: passing
- Native runtime: proven end-to-end on representative live smoke
- TUI startup path: fixed
- Primary product path: native ion + canto + direct API
- Secondary path: ACP exists but is not the design center and is still incomplete

## What is done

### Core runtime

- Native startup, turn submission, streaming, tool calls, persistence, and resume are working.
- Representative cross-provider handoff coverage is in place.
- `/compact`, `/clear`, `/cost`, and `/help` are implemented.
- Layered project instructions are implemented.

### TUI

- Startup header and committed rows print to native terminal scrollback.
- Plane B now owns only:
  - in-flight content
  - blank spacer
  - progress line
  - textarea
  - status line
- Startup no longer overwrites the shell launch line or forces unwanted scroll.
- Startup and resume banners are provider/model-free so they cannot go stale after runtime switches.
- Startup connection notices are rendered as normal system rows with the standard bullet and dim styling.
- Provider picker is now native-only, with aligned columns and actionable credential hints.
- Provider selection now opens the model picker instead of emitting a separate provider transcript row; the visible runtime change happens once the model is chosen, which keeps the same behavior for native and ACP switches.
- Model picker search now ranks close matches first, caches normalized search keys, prints a header row above the list, renders values-only context/input/output columns, and rounds prices to cents.
- Picker chrome is capitalized consistently across model and resume pickers.
- The footer/status path still needs a separate pass to stop showing `No model configured` when a model is already selected.
- Composer focus is restored, so plain text input works again.
- Safe double-tap semantics are back under tests:
  - `Esc` cancels running turns, double-tap clears idle input
  - `Ctrl+C` clears input or double-tap quits when empty
  - `Ctrl+D` double-tap quits when empty
- The old inline-layout task was retired after live verification; remaining TUI work is status/footer polish, not architecture.

### Storage / session model

- Persistence errors are surfaced instead of swallowed.
- Session recency updates correctly.
- Knowledge recall is scoped by workspace.
- Durable transcript roles stay separate from runtime/session events.
- Main runtime transcript/session naming now uses `agent`; child-agent transcript rows use `subagent`.

## What is still open

### Highest-value current work

- `tk-9s9c` — decide long-term provider-by-provider model catalog strategy
- `tk-6gvt` — consolidate `ai/` and task memory
- `tk-73o2` — align status/progress/resume UX
- `tk-810x` — status line polish
- `tk-2w0w` — relocate the mode indicator

### ACP follow-up

- `tk-o0iw` — initial session context at open
- `tk-6zy3` — token usage mapping
- `tk-2ffy` — stderr handling
- `tk-st4q` — ion as ACP agent

### Lower priority

- `tk-0dwv` — session tree navigation
- `tk-lggk` — AskUser UI
- `tk-lya7` — token usage colors
- `tk-w5uj` — git diff stats in footer

## Provider picker rules

- Only show providers ion can actually use today on the native path:
  - `anthropic`
  - `openai`
  - `openrouter`
  - `gemini`
  - `ollama`
- Do not show ACP/subscription entries in the picker until that UX is truly ready.
- Do not list unsupported providers like Vertex unless the backend actually supports them.
- Credential detail should be actionable:
  - `Ready`
  - `Missing • set <ENV_VAR>`
  - `Local`
- Picker order should be:
  - set APIs
  - set local endpoints
  - unset APIs
  - unset local endpoints
  - alphabetical within each bucket

## Important files

- `cmd/ion/main.go` — startup print path and runtime wiring
- `internal/app/render.go` — Plane B layout and picker rendering
- `internal/app/picker.go` — provider/model picker data
- `internal/app/events.go` — committed row print path and hotkey semantics
- `internal/app/model.go` — composer focus, pending-action timeout state
- `internal/backend/canto/backend.go` — actual native provider support
- `internal/backend/registry/models.go` — provider model-list fetch + cache
- `internal/backend/registry/registry.go` — metadata lookup + cache
- `ai/specs/model-catalog-strategy.md` — current evaluation of catwalk vs direct provider endpoints for model discovery

## Notes

- Current native provider support is exactly what `internal/backend/canto/backend.go` implements:
  - `anthropic`, `openai`, `openrouter`, `gemini`, `ollama`
- Vertex is not supported today, so it should not appear in the picker.
- OpenRouter `/model` is fixed in code: ion now fetches `https://openrouter.ai/api/v1/models` directly and caches the result locally.
- Provider changes clear any stale model before save so the provider/model pair always reflects the final chosen runtime.
- Re-selecting the already active provider preserves the current model and opens the model picker preselected to that model.
- The broader unresolved question is strategy for the rest of the providers. That work is now tracked in `tk-9s9c` and `ai/specs/model-catalog-strategy.md`.
- `tk-g1it`, `tk-6hv3`, and `tk-eyg1` are complete; do not treat them as active blockers.
