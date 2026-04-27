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

## Config, State, and Trust

Global Ion files live under `~/.ion/`.

| File | Owner | Purpose |
| --- | --- | --- |
| `~/.ion/config.toml` | user | Stable preferences: defaults, custom endpoints, policy/subagent paths, cost limits, verbosity. |
| `~/.ion/state.toml` | ion | Mutable runtime choices: selected provider/model/preset/thinking, recent pickers, UI state. |
| `~/.ion/trusted_workspaces.json` | user/ion | Workspace trust decisions. Separate from config so security state is auditable. |
| `~/.ion/policy.yaml` | user/admin | Optional durable tool policy rules. |
| `~/.ion/data/` | ion | Sessions, caches, model metadata, checkpoints. |

Current implementation still stores selected provider/model in `config.toml`.
Target direction: move volatile selections to `state.toml` and keep
`config.toml` for stable defaults and explicit provider definitions.

Stable config fields:

- `default_provider`
- `default_model`
- `default_reasoning_effort`
- `fast_default_model`
- `fast_default_reasoning_effort`
- `summary_model`
- `summary_reasoning_effort`
- `endpoint`
- `auth_env_var`
- `extra_headers`
- `policy_path`
- `subagents_path`
- `retry_until_cancelled`
- `workspace_trust`
- `tool_verbosity`
- `thinking_verbosity`

Thinking capability overrides are stable config, not mutable state. Unknown
custom endpoints should default to sending no thinking/reasoning parameter.
When a custom model needs provider-specific controls, define a per-model
capability override rather than hardcoding assumptions from its provider name:

```toml
[model_capabilities."local-api:qwen3.6:27b"]
thinking = "budget" # none | effort | budget | boolean
levels = ["off", "low", "medium", "high"]
default = "auto"
budgets = { low = 1024, medium = 4096, high = 8192 }
```

Raw numeric budgets are config-only unless repeated user behavior proves they
need a first-class TUI control.

Mutable state fields:

- `provider`
- `model`
- `reasoning_effort`
- `active_preset`
- `recent_models`
- `recent_providers`

Current local endpoint config shape:

```toml
provider = "local-api"
model = "qwen3.6:27b-uncensored"
endpoint = "http://fedora:8080/v1"
reasoning_effort = "auto"
```

Use `local-api` for no-auth OpenAI-compatible servers such as llama.cpp.
Use `openai-compatible` when a custom endpoint requires an API key/token.

Rules:

- ion should not invent provider/model defaults on startup
- explicit user actions may update state
- env vars remain startup overrides, not persistent writes
- keep stable config small
- do not write trust into config

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
- subagent personas use `primary` or `fast`; `subagents_path` only changes where persona Markdown files load from
- `primary` and `fast` are the UI-visible presets
- a preset may carry its own default reasoning level
- reasoning/thinking levels must be filtered by selected model capability;
  unsupported values should fall back to the highest supported level at or
  below the selected value, with a short visible notice
- if a preset is not explicitly configured, ion should resolve a deterministic provider default from the live model catalog

Model naming rules:

- prefer `primary` over `deep`
- prefer `fast` over `cheap` in the UI
- use `summary` for internal cheap transforms, not `cheap`

Model picker rules:

- the model picker keeps `Configured presets` at the top of the list
- `Configured presets` surfaces only explicitly configured primary and fast models
- resolver fallback defaults must not appear as user favorites or configured presets
- missing catalog metadata renders unknown context/input/output columns, not preset labels in metric columns
- `Tab` swaps between provider and model pickers
- `PgUp` / `PgDn` page through long provider/model picker lists

## Model discovery

Model discovery is covered in:

- `ai/specs/model-catalog-strategy.md`

Do not duplicate provider-fetch policy here.

## Important files

- `internal/app/render.go`
- `internal/app/model.go`
- `internal/config/config.go`
- `internal/backend/registry/`
