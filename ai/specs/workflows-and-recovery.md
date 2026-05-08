# Workflows and Recovery

## Position

Ion workflows are named coding procedures built from Canto graph primitives.
They are not a second chat mode, a generic automation builder, or an LLM-authored
runtime graph.

Default interaction stays the simple inline agent loop. Workflows are opt-in when
the task needs explicit gates, repeatable recovery, or parallel review.

Goals and missions are durable objective metadata layered over sessions and
workflows. They are not prompt prefixes. Do not add `/goal` until Ion can store,
pause, resume, budget, and report objective state without relying on the model
to remember it.

## Ownership

| Layer | Owns |
|---|---|
| Canto | `x/graph` DAG execution, loop nodes, parallel fan-out, checkpoints, wait-state pause/resume |
| Ion | workflow names, DAG topology, prompts, recovery policy, TUI commands, human gate copy |

Ion should not fork graph/checkpoint mechanics. If Canto primitives are missing,
file the gap upstream and keep Ion's code to topology assembly and UI.

## First Workflows

### 1. Code Review

Purpose: get a structured review before merge or before large refactors.

Graph:

| Node | Type | Gate |
|---|---|---|
| `scope` | single agent turn | require changed-file summary |
| `static_checks` | tool node | run configured tests/lints when cheap |
| `reviewer` | loop node, max 3 turns | produce severity-ranked findings |
| `fix_plan` | single agent turn | only if findings exist |
| `human_gate` | wait state | user chooses fix now / defer / stop |

Recovery:
- checkpoint after every node
- completed `static_checks` must not rerun on resume unless inputs changed
- if the user gates at `human_gate`, resume returns to the same gate with the
  last review summary visible

TUI surface:
- `/workflow review`
- progress line shows `workflow review: reviewer`
- transcript gets compact workflow boundary rows, not every graph checkpoint

### 2. Bug Fix

Purpose: enforce reproduce-before-fix for reported bugs.

Graph:

| Node | Type | Gate |
|---|---|---|
| `intake` | single agent turn | extract claim, expected behavior, files |
| `reproduce` | tool/agent loop, max 3 turns | must produce observed failing command/test |
| `edit` | loop node | blocked until reproduction fails for the claimed reason |
| `verify` | command/check node | rerun reproduction plus focused regression through shell/project commands |
| `summary` | single agent turn | explain root cause and verification |

Recovery:
- checkpoint after each node
- reproduction artifact is projected to a human-readable checklist in the
  workspace or issue when available
- if verification fails, resume enters `edit` with the failing command attached

TUI surface:
- `/workflow bug`
- if no reproduction can be produced, workflow stops with a normal transcript
  explanation instead of drifting into speculative edits

## Human Gates

These operations always park, regardless of mode or approval history:

- push to remote
- open or merge PR
- force push
- releases or publishing
- destructive git operations
- destructive filesystem operations outside scratch space
- CLA/signature/legal attestations

Parking means:
- save graph checkpoint
- show the gate in Plane B
- write a durable session event through Canto if available
- optionally update an external checklist projection later

## Durable Goals And Missions

### Boundary

Ion should use these terms precisely:

| Surface | Meaning | Durability |
|---|---|---|
| background job | live process started by `bash background=true` | job handle is live session runtime state; transcript records only starts/output/kill results |
| workflow | host-authored procedure with typed nodes, gates, and checkpoints | durable graph/checkpoint state once Canto exposes the needed primitive |
| goal | durable objective metadata attached to a session or workflow | survives resume/import/export and can be paused, resumed, completed, or failed |
| mission | goal plus one or more workflow runs or child-agent/job activities | experimental/x until workflow checkpoints, budgets, and supervision are boring |
| swarm | operator view over many agents/jobs/missions | alternate-screen future work, not a default chat mode |

The first accepted goal slice should be metadata and status, not autonomy. A
goal can tell the user and agent what the current objective is, what budget or
gate applies, and what recovery state exists. It should not schedule hidden
turns, spawn children, or keep working after the user leaves until those
behaviors have a separate automation design.

### Goal Record

Candidate product record:

| Field | Purpose |
|---|---|
| `id` | stable goal id within the session lineage |
| `session_id` | owning Ion session |
| `title` | short user-visible label |
| `objective` | durable user-approved goal text |
| `state` | `draft`, `active`, `paused`, `blocked`, `done`, `failed`, or `cancelled` |
| `created_at` / `updated_at` | UTC timestamps for audit and resume display |
| `owner` | `user`, `assistant`, or `workflow` |
| `budget` | optional max turns, tokens, wall time, or cost |
| `progress` | concise status summary and current step |
| `checklist` | explicit user-visible work items when useful |
| `last_event_id` | latest durable event considered by the progress summary |
| `jobs` | related background job ids and last known transcript references |
| `blockers` | user-visible blockers or gates |
| `recovery_hint` | where resume should restart or what the user must decide |

Store goal state outside provider-visible history. A resumed provider request
may include a compact active-goal summary only when a goal is active and the
prompt-budget impact is measured.

### Pause And Resume

Pause is a host/workflow state, not a normal assistant message.

Rules:

- pausing a goal prevents further autonomous goal/workflow steps
- pausing does not silently kill background jobs; `/jobs` and `/stop <job-id>`
  remain the process-control surface
- resuming rehydrates goal metadata, latest progress, blockers, budgets, and
  recovery hint before the next provider request
- if an active model turn must be interrupted to pause, use the existing cancel
  path and settle the terminal state before recording the pause
- completed or failed goals remain visible through explicit status/history
  surfaces, not default footer chrome

### Budget And Progress

Budgets are hard gates for autonomous work and soft context for ordinary chat.

Budget dimensions:

- turn count
- provider tokens or cost when available
- wall-clock elapsed time
- background job runtime
- child-agent count or runtime when subagents are involved

Progress should be derived from durable events, workflow checkpoints, explicit
checklists, and verified command results. Model-written status text is allowed
as a summary, but it is not the source of truth for whether a command passed,
a gate was approved, or a background job is still live.

### TUI And CLI Surface

Do not add visible command chrome until goal metadata exists.

Future command shape, in order:

1. `/status` shows an active goal summary when one exists.
2. `/goal` shows or edits the current session goal; no hidden autonomy.
3. `/goal pause|resume|clear` manipulates durable goal state.
4. `ion goal status --session <id>` or equivalent scriptable output exposes
   goal state for automation.
5. Mission/schedule commands remain experimental/x until they can supervise
   workflow checkpoints, child agents, background jobs, budgets, and gates.

Default footer should stay quiet. If a goal is active, one compact status token
is enough; detailed progress belongs in `/status` or the future goal command.

### Acceptance Before `/goal`

Before `/goal` becomes a visible command:

- deterministic tests persist, pause, resume, complete, fail, and export/import
  a goal record
- resumed provider history includes the active goal summary only when intended
  and excludes completed/cleared goals by default
- budget exhaustion parks the goal with a visible blocker instead of continuing
  silently
- `/status` displays active goal state without adding default footer clutter
- tmux smoke covers status, pause/resume display, and resume after restart
- live-provider smoke runs only if goal state is included in provider-visible
  context
- no background process survival is implied; live jobs stay governed by
  `/jobs` and `/stop`

## Recovery Policy

| Failure | Behavior |
|---|---|
| process crash | restart from last graph checkpoint |
| context overflow | rely on Canto runtime overflow recovery before retrying node |
| transient provider error | rely on provider retry/recovery wrapper |
| tool/test failure | node-specific edge, not generic retry |
| human gate | park until explicit user action |
| changed workspace input | invalidate downstream checkpoints |

Checkpoint invalidation should start simple: hash the relevant file list or command
inputs per node. Do not add a broad workspace snapshot system under this task.

## Deferred

- visual workflow editor
- LLM-generated graph topologies
- Temporal-compatible activity workers
- cross-host workflow queue
- automatic PR creation
- Slack/email workflow approvals
- hidden autonomous missions
- scheduled/remote mission runners
- alternate-screen swarm supervision

Those are separate products. The first implementation should make review and bug-fix
workflows recoverable and visible without making the default Ion loop heavier.
