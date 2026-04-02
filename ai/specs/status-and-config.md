# Status and Config

## Scope

Current source of truth for:

- progress line behavior
- status line behavior
- user-editable runtime config
- model metadata display rules

This file replaces the older mixed `config-and-metadata.md` note as the active spec.

## Progress line

The progress line is a live state surface, not a transcript surface.

Current responsibilities:

- configuration warnings
- running state
- completion state
- error state
- cancellation state
- short per-turn stats

Current direction:

- one-line only
- spinner while running
- semantic color only for success, error, warning
- keep token/time stats compact
- do not dump reasoning traces by default

Related tasks:

- `tk-i207`
- `tk-gmhw`

## Status line

Current shape:

`[MODE] • provider • model • usage • cwd • branch`

Rules:

- left-aligned, no balanced/right-aligned layout tricks
- mode is the main visual anchor
- metadata stays secondary/dim
- provider/model are live runtime truth, not startup-banner truth
- when the fast preset is active, surface it as a compact `[FAST]` marker
- reasoning effort may appear only when it is a real runtime setting

Keep the status line compact. Do not turn it into a dense settings/control bar unless there is a strong usability reason.

## User config

User-facing config lives in:

- `~/.ion/config.toml`

Current important fields:

- `provider`
- `model`
- `fast_model`
- `fast_reasoning_effort`
- `summary_model`
- `summary_reasoning_effort`
- `endpoint`
- `auth_env_var`
- `extra_headers`
- `reasoning_effort`

Rules:

- ion should not invent provider/model defaults on startup
- explicit user actions may update config
- env vars remain startup overrides, not persistent writes
- keep user config small

## Model metadata display

Current picker display rules:

- `Free` for known zero cost
- `—` for unknown cost
- `$X.XX` for known paid USD cost

Do not localize provider-model prices by user locale.
Use canonical USD display in the picker.

## Model presets

Current direction:

- `provider` / `model` / `reasoning_effort` are the primary preset slot
- `fast_model` / `fast_reasoning_effort` define the fast preset slot
- `summary_model` / `summary_reasoning_effort` stay config-only and are used for cheap transforms like compaction and titles
- `primary` and `fast` are the UI-visible presets
- a preset may carry its own default reasoning level
- if a preset is not explicitly configured, ion should resolve a deterministic provider default from the live model catalog

Model naming rules:

- prefer `primary` over `deep`
- prefer `fast` over `cheap` in the UI
- use `summary` for internal cheap transforms, not `cheap`

## Model discovery

Model discovery is covered in:

- `ai/specs/model-catalog-strategy.md`

Do not duplicate provider-fetch policy here.

## Important files

- `internal/app/render.go`
- `internal/app/model.go`
- `internal/config/config.go`
- `internal/backend/registry/`
