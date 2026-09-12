# Core runtime migration strategy

Status: active implementation plan, 2026-09-12.

This document translates `DESIGN.md` into implementation slices. The plan is subordinate to design and evidence: change it when tests or measurements disprove a boundary.

Scope is the **core agent/session runtime only**. Knowledge/memory systems, shared task boards, semantic/vector indexes and other higher-level coordination mechanisms are out of scope.

## Goal

Replace the current lane/operation/agent-family execution core with the target **session + conversation + task** model without creating a permanent second runtime.

Preserve providers, tools, process ownership, authority code, tests and frontend work only where their contracts remain correct. Existing internal abstractions have no compatibility claim.

## Target nouns

- **Session** — one durable coordination/transaction domain with a primary conversation and related worker/branch conversations.
- **Conversation** — one durable agent thread: transcript/context, configuration/authority/workspace state, optional history parent and optional owner task.
- **Entry** — immutable semantic transcript fact.
- **Input** — admitted user/agent input with target/mode/request identity/disposition.
- **Task** — generic durable executable unit with typed state, dependencies and terminal outcome.
- **Effect** — durable external-work intent/attempt/settlement.
- **Job** — environment-backed long-lived work represented through tasks/effects.
- **Artifact** — retained output/evidence stored inline or by durable reference.

There is **no separate target durable Agent row**. `Agent`/`Worker` may remain API/UI terminology wrapping a `ConversationId`.

## Target topology

```text
Session
  primaryConversationId
  |
  +-- Conversation root
  |     +-- entries
  |     +-- inputs
  |     +-- tasks/effects
  |
  +-- Conversation worker A
  +-- Conversation worker B
  +-- Conversation historical branch
```

Relationships remain independent:

```text
history:      Conversation --parent/cutoff--> Conversation
ownership:    Task --owns--> Conversation
readiness:    Task --depends-on--> Task
workspace:    Conversation/Task --binds--> Workspace
communication Input(sender,target)
```

A task-created child may have both an owner and a history parent. A host-created historical fork may have a history parent and no owner.

## Why remove the separate Agent entity

No established core use case currently requires one participant identity to own several simultaneous conversations:

- related follow-ups continue the same conversation;
- context reset/handoff can give that thread a clean model context;
- true alternatives/branches should be independently addressable conversations;
- root and workers should use identical scheduler/storage types.

Current Pico's normative implementation specification likewise models a subagent as an owned conversation rather than a second durable object; current Codex spawned-agent identity is a thread ID. Ion should add a separate persistent agent identity later only if a concrete invariant requires it.

During migration, existing `AgentId`, lane identity and hosted-family structures are legacy production concepts, not target API commitments.

## Session remains the consistency boundary

Root and cooperating workers share one session/store because creation, messages, task dependencies, cancellation barriers, authority, usage and workspace metadata may require one atomic transition.

A clean worker context does **not** require a new session. Create a fresh conversation inside the same session.

New top-level sessions are reserved for genuinely independent lifecycle/security/ownership domains.

## Worker creation model

Worker context and worker lifetime are independent decisions.

### Context

- **fresh**: no history parent; explicit task prompt + selected initialization;
- **inherit**: history parent/cutoff at a safe complete exchange boundary;
- **reuse**: send a follow-up to an existing worker conversation when its accumulated context is useful.

The first baseline should favor fresh context for independent delegation and require explicit inheritance when prior conversational context matters. Measure this before stabilizing model-facing defaults.

### Lifetime

- **joined/foreground**: parent work depends on child result;
- **retained/background**: creator may settle immediately while child remains addressable;
- later waits are durable dependencies/continuations, not held execution permits.

Same conversation/task schema for all cases.

## Migration decision

Do not deepen:

```text
lane -> operation -> one open effect
AgentId -> lane/session indirection
```

Target:

```text
ConversationId -> Inputs / Entries / Tasks
TaskId         -> dependencies / owned Conversations / Effects
SessionId      -> one mutation owner / canonical store
```

Use incremental replacement. New core pieces remain crate-private until they replace old behavior. When target coverage has equivalent or stronger tests, delete the displaced path.

## K0 — Freeze semantic identities and store boundary

1. Keep `SessionId`, `ConversationId`, `EntryId`, `InputId`, `TaskId`, `EffectId` as target semantic identities.
2. Treat `AgentId` and `OperationId` as legacy migration identities.
3. Keep `CommitSeq` independent from object identity.
4. Keep physical UUID/integer representation reopenable until P1/P2 evidence.
5. Define one narrow crate-private session-store interface that does not expose the current root-wide physical DB layout.
6. Do not add memory/task-board abstractions.

## K1 — Session command kernel

Introduce one crate-private session kernel that owns serialized semantic mutation.

Required initial commands:

- admit input with request key + exact equivalence check;
- create/settle/cancel task;
- add/remove task dependency with cycle checks;
- open/settle/recover effect attempt;
- append immutable entry/context control;
- create conversation, optionally with history parent and/or owner task;
- copy explicitly selected initialization state for a child.

A command never performs provider/tool/process work. It commits intent/state and returns post-commit dispatch work.

### First worker-creation transaction

The kernel must support one atomic batch equivalent to:

```text
create child Conversation
  + optional history parent/cutoff
  + owner Task
  + selected configuration/authority/workspace initialization
  + initial Input
  + first generation Task
```

No partially created worker becomes externally visible.

## K2 — One real turn as generic tasks

Implement:

```text
Input
  -> generation G
       +-> tool A --+
       +-> tool B --+-> join P -> generation G2/final
```

Generation settlement atomically creates all tool tasks + join. Tools settle independently in completion order; provider projection restores call order.

Use real `SessionStore` transactions plus scripted providers/tools. The isolated P1 store is deleted as equivalent production tests land.

## K3 — Recovery and cancellation

Promote P1 semantics:

- effect intent before dispatch;
- stable effect ID + attempt identity;
- retry-safe/reconcile/no-safe-retry recovery;
- open/inspect starts no work;
- explicit drive/resume;
- durable cancellation mark revokes invocation generation before signalling;
- settlement-before-cancel wins, otherwise normal completion fences;
- caller wait cancellation never cancels accepted work;
- persistence uncertainty fences session until reopen/reconciliation.

## K4 — Owned conversations / workers

Implement workers on the same kernel:

- fresh child;
- inherited child at complete boundary;
- joined foreground child;
- retained background child;
- send/follow-up;
- read-only inspect/status;
- durable wait without permit starvation;
- interrupt/cancel and explicit retire;
- nested owned conversations under limits.

The root and worker use identical conversation/task schemas. Role/model/tool differences are configuration.

Once covered, delete the existing agent-family/lane orchestration path.

## K5 — Remove legacy execution concepts

Likely deletion/replacement targets:

- `OperationMachine` as main turn scheduler;
- `OperationId` where object is a `TaskId`;
- `AgentId` where object is a `ConversationId`;
- lane as conversation/agent identity;
- singular `open_effect` checkpoint;
- `submit_if_idle_on_lane` as input admission;
- duplicate retained-agent registry/state that can be derived from conversations/tasks.

Convert callers to target semantics, then delete old code; do not rename old semantics in place.

## Persistence topology

Semantic interface assumes one authoritative store **per session**, even while migration still reads/writes the current root-wide database.

Leading physical candidate:

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/

  catalog.sqlite   # optional rebuildable discovery cache only if needed
```

One `session.sqlite` contains all records that may participate in one session command: conversations/history, inputs, tasks/dependencies, effects, authority/approvals, usage/budgets, workspace correctness metadata and artifact refs.

Do not split a session transaction by record category across WAL databases.

SQLite remains the default engine. Turso is a P2 benchmark candidate only if measurement or an accepted sync requirement makes it relevant. The semantic one-writer rule remains regardless of engine.

## Workspace policy

Context inheritance and workspace isolation are independent.

- read-only workers may share the source workspace;
- parallel mutating workers normally receive isolated worktrees/snapshots;
- integration/apply is a separately admitted effect with current-base/dirty-state checks and re-verification.

Do not infer workspace sharing from history parentage.

## Observation/TUI trace

P4 should consume the same runtime objects:

- session overview of conversation/worker status;
- focused conversation transcript/tasks/tools;
- stable per-conversation drafts;
- replies/approvals routed by captured IDs;
- inspection read-only;
- no lane object required in durable state.

A lane-shaped presentation, if useful, is a projection of one conversation's live tasks/output.

## Effectiveness evaluation before freezing delegation defaults

Core correctness is not evidence that workers improve coding success.

Compare under equal model/token/time budgets:

1. root works directly;
2. one fresh worker;
3. one inherited worker;
4. reuse of an existing worker;
5. bounded parallel workers on decomposable tasks.

Task families should include repo exploration, debugging continuation, independent review, parallel implementation, sequential refactor and integration-heavy changes.

Measure verified task success, elapsed time, tokens/cost, duplicated work, integration failures, incorrect/stale inherited assumptions and human intervention.

The initial orchestration policy should remain centralized: the primary/root conversation delegates and validates bounded work. Peer swarms are not the default.

## Promotion rule

A kernel slice replaces old code only when:

1. semantics match `DESIGN.md`;
2. deterministic race/crash tests cover relevant P1/P2 cases;
3. formatting, strict workspace clippy and locked workspace tests pass;
4. no second public runtime/API remains;
5. displaced implementation is deleted or has an explicit next-slice deletion dependency;
6. evidence and known limitations are recorded.

A failed slice changes the design. It is not a reason to add a compatibility layer.