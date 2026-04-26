# Workflows and Recovery

## Position

Ion workflows are named coding procedures built from Canto graph primitives.
They are not a second chat mode, a generic automation builder, or an LLM-authored
runtime graph.

Default interaction stays the simple inline agent loop. Workflows are opt-in when
the task needs explicit gates, repeatable recovery, or parallel review.

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
| `verify` | tool node | rerun reproduction plus focused regression |
| `summary` | single agent turn | explain root cause and verification |

Recovery:
- checkpoint after each node
- reproduction artifact is projected to a human-readable checklist in the
  workspace or issue when available
- if `verify` fails, resume enters `edit` with the failing command attached

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

Those are separate products. The first implementation should make review and bug-fix
workflows recoverable and visible without making the default Ion loop heavier.
