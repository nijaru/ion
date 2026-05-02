# Subagent Personas And Routing

## Current State

Ion has a synchronous `subagent` tool implementation, persona loader, and Canto
child-session hook points, but the tool is not registered in the default native
tool surface. The current product baseline remains the eight coding tools in
`ai/specs/tools-and-modes.md`.

Do not expose `subagent` by default until the context contract below is
implemented and tested. A child agent that only receives ad hoc user-provided
context is too easy for the model to misuse: it may assume the child sees the
parent transcript when it does not.

Active prerequisite: `tk-hz8p` - Subagents: implement explicit context modes
before registration.

Built-in personas remain the target shape:

| Persona | Model slot | Tool scope | Purpose |
|---|---|---|---|
| `explorer` | `fast` | read/search/memory recall | cheap isolated context gathering |
| `reviewer` | `primary` | read/search/shell | correctness and regression review |
| `worker` | `primary` | edit/shell plus read/search | scoped implementation |

Custom personas load from global Markdown files with YAML frontmatter:

```markdown
---
name: scout
description: Quick read-only repo scouting.
model: fast
tools: [read, grep, glob, list]
---
Find relevant files and summarize concrete findings with paths.
```

Default directory: `~/.ion/agents`
Config override: `subagents_path`

## Context Contract

The next implementation should make context transfer explicit in the tool schema
instead of relying on prompt convention.

| Mode | Meaning | Default use |
| --- | --- | --- |
| `summary` | Parent sends a compact task brief plus selected project/session summary. | cheap exploration and review |
| `fork` | Child starts from a snapshot of the parent's current provider-visible history. | high-fidelity delegated reasoning |
| `none` | Child receives only the task and persona prompt. | narrowly scoped commands where prior context is harmful |

`summary` should be the default. `fork` is valuable, but it must be explicit
because it carries more tokens, more privacy surface, and more chance of child
agents duplicating parent work.

Fork rules:

- snapshot the parent's effective provider-visible history at spawn time
- never let child events mutate parent history directly
- persist child events under a child session id with parent linkage metadata
- return only a concise child result to the parent unless the user expands
  details
- a child fork does not see parent turns submitted after spawn

This likely belongs partly in Canto. Canto should own reusable child-session and
history-snapshot primitives; Ion should own personas, tool exposure, display,
and user-facing policy.

## Routing Policy

- `primary` uses the active provider/model.
- `fast` uses the existing fast preset resolver (`fast_model`, then provider
  catalog heuristic).
- Unknown or invalid persona files fail startup instead of silently changing
  delegation behavior.
- Tool scope is fail-closed through Canto `Registry.Subset`.

## Product Boundary

This intentionally stays small. Ion should not grow many specialized personas
by default; generic explorer/reviewer/worker cover the useful split without
forcing a complex delegation UI. More advanced swarms, worktrees, and async
operator views stay downstream of the reliable inline solo loop.

`tk-pwsl` closed the alternate-screen swarm/operator view as deferred. The
normal near-term product surface is inline Plane B subagent visibility plus
explicit context modes; a full operator view should not start until subagent
registration and child-session ownership are boring.

Near-term inline behavior should be conservative:

- no background subagent wakeups in the normal chat surface
- no subagent-to-subagent communication in the default inline mode
- no automatic worktree or branch creation
- no registration until tests prove child sessions preserve provider-history
  validity and parent/child ownership boundaries

Future references to keep in mind:

- Pi-style subagent communication can be useful for orchestrated workflows, but
  belongs in a later swarm/operator surface.
- Claude Code-style forked subagents are the better first high-fidelity context
  feature because the boundary is a snapshot, not ongoing shared state.

## Acceptance Gates Before Registration

- parent transcript and provider-visible history remain unchanged by child
  events except for the final returned result
- `summary`, `fork`, and `none` modes are covered by deterministic tests
- child session replay is durable and readable
- parent cancellation cancels in-flight synchronous children
- tool scope remains fail-closed through persona allowlists
- TUI shows compact child lifecycle rows without dumping child transcript by
  default
