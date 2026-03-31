# Instructions and Skills

## Current answer

ion supports layered project instructions.

ion does not yet support first-class skills as a product feature.

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

## What is not implemented

These are not shipped ion features yet:

- skill registry
- skill activation or selection UX
- skill-aware slash commands
- skill-specific prompt injection from user-facing ion config
- skill-specific tool bundles or runtime capabilities

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

## Near-term direction

If ion adds skills, they should be treated as a distinct surface:

1. core prompt
2. runtime context
3. project instructions
4. optional skills
5. task/mode reminders

Do not collapse skills into the same bucket as `AGENTS.md`.

## Open work

- `tk-lmhg` — audit current support and define the target UX/runtime model for skills

## Relevant files

- `internal/backend/instructions.go`
- `internal/backend/canto/prompt.go`
- `ai/specs/system-prompt.md`
- `ai/plans/archive/system-prompt-refactor-2026-03-27.md`
