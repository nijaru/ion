# System Prompt Refactor Plan (2026-03-27)

Archived on 2026-03-28 after the prompt modularization and eval work completed.

The active current-state prompt guidance lives in:

- `ai/specs/system-prompt.md`

Original plan preserved below.

---

# System Prompt Refactor Plan (2026-03-27)

## Goal

Make ion's prompt system match the strongest patterns from Claude Code, Gemini CLI, Codex, pi, and OpenCode without confusing:
- the core system prompt
- runtime/session context
- project instruction files
- optional skills
- task- or mode-specific prompt fragments

## Findings

### Best patterns

1. **Composable prompt architecture**
   - Claude Code and Gemini CLI treat prompt construction as composition, not one string.
   - This keeps the stable core small and makes targeted changes safer.

2. **Stable core prompt**
   - Best systems keep the always-on core focused on operating policy:
     - identity
     - workflow
     - tool/permission rules
     - response style

3. **Separate project instructions**
   - Repo instruction files are not the same thing as the product's system prompt.
   - Claude Code documents this distinction explicitly.

4. **Prompt eval loops**
   - Gemini and OpenAI both emphasize evals for prompt changes.
   - Prompt quality should be checked with representative behavioral tests, not only by reading the prompt text.

5. **No branding noise**
   - Strong prompts avoid hype, stale model recommendations, and implementation branding unless operationally necessary.

### Anti-patterns to avoid

- one monolithic prompt string
- mixing repo-specific rules into the core product prompt
- encoding provider/model/runtime identity into the stable core
- always-on skill inventories in the base prompt
- prompt changes without behavioral checks

## Target architecture

### Layer 1: Core system prompt

Always-on, stable, product-owned.

Sections:
- identity
- core mandates
- workflow
- tool and approval policy
- response style

### Layer 2: Runtime/session context

Dynamic facts for the current run only.

Examples:
- current working directory
- platform
- current date
- sandbox mode
- approval mode
- git repo presence

Non-goals:
- provider/model/backend branding
- UI footer data

### Layer 3: Project instructions

Loaded from local files such as `AGENTS.md` and `CLAUDE.md`.

Rules:
- separate from the core prompt conceptually
- repo-owned, not product-owned
- authoritative for repo-specific style/workflow/tooling

### Layer 4: Optional skills / agent prompts

Loaded only when relevant.

Rules:
- not always-on
- not part of ion's core prompt

### Layer 5: Task/mode fragments

Examples:
- plan mode
- compaction prompt
- future slash-command helpers

Rules:
- scoped to one operation or mode
- not part of the base prompt

## Implementation order

1. **Prompt builder refactor** (`tk-qc3u`)
2. **Runtime context separation**
3. **Project instruction boundary cleanup**
4. **Prompt evals** (`tk-g060`)
5. **Follow-up prompt tuning**
