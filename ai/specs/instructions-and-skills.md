# Instructions and Skills

## Current answer

ion supports layered project instructions.

ion exposes a read-only local `/skills [query]` browser for installed
`~/.ion/skills` bundles. It also supports an opt-in model-visible
`read_skill(name)` tool behind `skill_tools = "read"`. The default eight-tool
coding surface is unchanged and no skill inventory is added to the prompt.
Marketplace install, skill activation, `manage_skill`, and self-extension are
still deferred. The design target is explicit-install, progressive-disclosure
skills on top of Canto's general skill primitives, not always-on prompt bloat or
an ungated self-extension tool.

## What is implemented

### Built-in prompt layers

ion currently composes prompt input from:

1. core ion system prompt
2. runtime/session context
3. repo-local instruction files

Implementation:

- base prompt: `internal/backend/canto/prompt.go`
- instruction layering: `internal/backend/instructions.go`

### Repo-local instruction files

Current loader behavior:

- walks from repo root to current working directory
- loads the first matching instruction file in each directory
- supported names today:
  - `AGENTS.md`
  - `CLAUDE.md`

Important limitation:

- `GEMINI.md` is not currently loaded
- no runtime concept of “skill activation” exists

### Local skills browser

`/skills [query]` lists locally installed skill metadata from `~/.ion/skills`.
It is a host command only:

- no skill inventory is added to the model prompt
- no skill body is injected
- no marketplace fetch or install is performed

### Opt-in read_skill tool

`skill_tools = "read"` in `~/.ion/config.toml` registers `read_skill(name)`.
The tool reads an installed local `SKILL.md` body by explicit name and returns
the skill name, description, allowed-tools metadata, and instructions. It is a
read-category tool, so read mode can expose it when the gate is enabled.

Important boundaries:

- default sessions still expose only the eight coding tools
- no installed skill list is injected into the core prompt
- the model must know or discover a skill name through an explicit host surface
  such as `/skills`
- `manage_skill` is not implemented and is not implied by this gate

## What is not implemented

These are not shipped ion features yet:

- skill registry
- skill activation or selection UX
- CLI skill commands
- built-in slash command registry beyond the core actions
- user-defined slash command or skill aliases
- skill-specific prompt injection from user-facing ion config
- skill-specific tool bundles or runtime capabilities
- model-visible `manage_skill`
- marketplace install/update

## Why this distinction matters

Project instructions and skills solve different problems.

### Project instructions

- repo-scoped
- always-on within that repo/path
- define local conventions, workflows, and constraints

### Skills

- task-scoped or mode-scoped
- optional
- activated deliberately
- should not be part of the always-on core prompt by default

## Ownership

Canto owns framework mechanisms:

- agentskills-compatible registry loading and validation
- skill metadata, body loading, and optional routing
- reusable `read_skill` / `manage_skill` primitives
- tool-scope helpers such as `allowed-tools`

Ion owns product policy:

- where skills live under `~/.ion/skills` and project-local locations
- whether skills are visible, enabled, or installed
- slash/CLI commands such as `/skills` and future `ion skill ...`
- marketplace staging, source display, trust prompts, and install UX
- whether `read_skill` or `manage_skill` are exposed to the model

Ion should not make skill state part of normal startup unless the user has
explicitly enabled it.

## Target behavior

Skills are a distinct surface:

1. core prompt
2. runtime context
3. project instructions
4. optional skills
5. task/mode reminders
6. built-in slash commands
7. user-defined `//` command or skill aliases

Do not collapse skills into the same bucket as `AGENTS.md`.

The default prompt must not include a skill inventory. A user with many skills
should not pay an L1 token cost every session. Discovery should happen through
an explicit host surface (`/skills`, CLI browse/search) or a bounded Canto
router once the user enables model-visible skills.

Command syntax direction:

- `/foo` is for built-in ion actions and user-facing runtime commands
- `//foo` is reserved for user-defined command or skill aliases
- keep the command surface textual and discoverable; do not bury stateful actions only behind hotkeys

Model-visible tool direction:

- `read_skill(name)` is implemented behind `skill_tools = "read"`.
- `manage_skill` is not a default tool. It may only be exposed for a trusted
  user-local skill directory, under write-capable modes, with the same policy
  and approval posture as file writes.
- Self-extension nudges are deferred until `manage_skill` is safe, observable,
  and easy to undo.

Marketplace direction:

- No automatic remote install as a side effect of a model turn.
- Install into a staging area first, validate with the agentskills parser, show
  source/path/summary, then require explicit user confirmation.
- Treat marketplace skills as instructions plus optional local resources, not
  executable packages.
- Never run fetched scripts during install.
- Prefer curated/local sources before broad public registries.

Model preset direction:

- `primary` is the default daily driver
- `fast` is the cheaper/faster preset exposed in the UI
- `summary` is internal/config-only and used for compaction, titles, and other cheap transforms
- the UI should not expose every raw model; fuzzy slash commands can reach them when needed

## Open work

- Add CLI list/search if the TUI browser proves useful.
- Add safe install staging before any marketplace integration.
- Gate `manage_skill` separately; neither `read_skill` nor `manage_skill`
  belongs in the default eight-tool coding surface.

## Relevant files

- `internal/backend/instructions.go`
- `internal/backend/canto/prompt.go`
- `internal/skills/skills.go`
- `ai/specs/system-prompt.md`
- `ai/research/skills-progressive-disclosure-sota-2026-04.md`
