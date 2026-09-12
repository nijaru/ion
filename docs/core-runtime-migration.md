# Core runtime migration strategy

Status: active implementation plan, 2026-09-12.

This document turns the target contracts in `DESIGN.md` into an implementation sequence. It is deliberately subordinate to the design and evidence gates: if production tests or measurements invalidate a boundary, change the plan rather than preserving an implementation artifact.

The scope of this plan is the **core agent/session runtime only**. Shared task boards, long-term/project knowledge, memory systems, semantic indexes, and similar higher-level coordination mechanisms are not part of the core target. They may be researched later against a working baseline. Do not pre-shape the core around speculative future services.

## Goal

Replace the current lane/operation-centric execution core with the target conversation/task/session model without creating a permanent second runtime or preserving compatibility with internal abstractions that no longer fit.

The migration should preserve useful adapters, providers, tools, process ownership, authority logic, tests, and TUI work where their contracts remain correct. It should not preserve `lane`, `operation`, single-open-effect, or other old runtime concepts merely because they already exist.

## Reference posture

Current Pico/Pi 2/Pico v3 work is the primary minimal-harness design reference. Its strongest relevant ideas are small generic scheduler concepts, immutable transcript entries, explicit model context, generic durable tasks, one serialized session mutation line, atomic settlement/successor creation, durable dependency-based waits, and explicit recovery after uncertain effects.

Ion intentionally differs where its own requirements or evidence justify it:

- Ion P1 selected a typed re-entrant step/checkpoint boundary rather than making an async task frame the durable continuation.
- Ion keeps object identity separate from commit ordering unless P1/P2 measurements show a concrete reason not to.
- Ion is testing one SQLite database per session/group instead of assuming multiple sessions should share a physical database.
- Ion treats authority, retained agent identity, recovery classes, and client observations as explicit runtime contracts.

Codex remains a production-engineering reference when public source answers a concrete question. Goose, OpenHands, and other harnesses are not default baselines.

## Target core

```text
frontends / adapters
        |
        v
+--------------------------+
| Host                     |
| residency / locks / I/O  |
+-------------+------------+
              |
              v
+--------------------------+
| Session owner            |
| one serialized writer    |
| commands + observations  |
+---+------------------+---+
    |                  |
    | atomic commits   | effect dispatch
    v                  v
+-----------+      +--------------------+
| Session   |      | Effect adapters    |
| store     |      | model/tool/job/... |
+-----------+      +--------------------+
    |
    +-- immutable entries + context
    +-- retained agents/conversations
    +-- durable tasks/dependencies
    +-- inputs/messages/receipts
    +-- effect intent/attempt/settlement
    +-- authority/approval/usage
```

The session owner is the only semantic writer. Storage is an atomic persistence mechanism, not another scheduler. Effect adapters are asynchronous and concurrent but cannot directly mutate canonical session state.

This distinction is fundamental to the storage decision: provider calls, tool execution, subprocess I/O, waiting, and other slow work never run while a database write transaction is held. The serialized writer should perform short validation/commit transitions and immediately release the store.

## Core nouns to converge on

- **Session**: one durable coordination and transaction boundary containing one root agent and any cooperating retained agents created within that group.
- **Agent**: retained identity with configuration, authority and a current conversation.
- **Conversation**: immutable transcript plus explicit model-context state and fork provenance.
- **Input**: admitted user/agent input with delivery mode, request identity and disposition.
- **Task**: generic durable executable unit with typed kind state, dependencies, invocation generation and terminal outcome.
- **Effect**: durable external-effect intent plus attempt/recovery classification and settlement.
- **Job**: environment-backed long-lived work represented through durable tasks/effects.
- **Artifact**: retained output/evidence referenced by durable session state when it should not live inline.

Do not introduce another permanent synonym for these concepts during migration.

## Session is the consistency boundary

A session is deliberately larger than a single conversation or agent. Root agent and retained workers in one cooperating group share one session transaction boundary because operations across them can require atomic invariants:

- spawning an agent together with its initial conversation/input/task;
- parent/child supervision and cancellation barriers;
- task dependencies and successor creation;
- messages and target inbox admission;
- authority/grant narrowing;
- usage/reservation accounting;
- shared workspace ownership or conflict metadata;
- durable observations derived from one committed transition.

Do **not** use one database per agent. That would turn ordinary same-group transitions into cross-database protocols and make crash recovery harder for no semantic benefit.

Independent top-level sessions do not require those transactions with one another. They should be separate ownership/failure domains and may be active concurrently.

## Migration decision

Do **not** deepen the existing `lane -> operation -> one open effect` execution model and then translate it later. The mismatch is structural:

- target agents own conversations, not lanes;
- target execution is generic durable tasks, not model-specific operations;
- multiple tool tasks/effects may be live concurrently;
- durable waits are task dependencies/continuations, not a caller holding an async wait chain;
- input admission and idempotent receipts are first-class records rather than an argument to `submit_if_idle_on_lane`;
- retained agent lifetime is independent from one turn/operation;
- observation targets stable agent/conversation/task identities rather than UI focus or lane names.

Use an **incremental replacement** instead. New production core pieces are crate-private until they replace the old path. A slice is promoted only when equivalent or stronger tests exist, after which the displaced old path is deleted. There must never be two public task frameworks or two long-term transcript authorities.

## Implementation sequence

### K0 — Freeze the target boundary

Before adding more old-runtime features:

1. Keep `DESIGN.md` and the P1/P2 evidence as the contract.
2. Add target identity types needed by the core (`ConversationId`, `TaskId`, `InputId`, and any receipt/message IDs that survive as first-class objects).
3. Keep `CommitSeq` separate from semantic identity. The physical integer/UUID representation remains an implementation decision until P1/P2 measure it.
4. Define crate-private read/write interfaces around one session store so the later root-wide -> per-session physical migration does not leak through runtime APIs.
5. Keep higher-level coordination/memory ideas outside this boundary until the core provides a stable baseline to evaluate them against.

### K1 — Session command kernel

Introduce one crate-private session kernel that owns serialized mutation. It should accept typed commands and produce atomic commit plans plus post-commit dispatch/observation work.

Required first commands:

- admit input with request key and exact equivalence check;
- create/settle/cancel task;
- open/settle/recover effect attempt;
- append immutable entry and update explicit context;
- create retained agent/conversation;
- add/remove task dependency under cycle checks.

A command cannot execute provider/tool/process work. It can only record intent and return dispatch work after commit.

### K2 — One real turn as generic tasks

Implement the smallest real agent turn using generic tasks:

```text
input admission
      |
      v
 generation task
      |
      | assistant calls A, B
      | atomic settlement creates all children + join
      v
  +--------+     +--------+
  | tool A |     | tool B |
  +---+----+     +---+----+
      |              |
      +------v-------+
          join task
              |
              v
        next generation
```

Tool A and B settle independently and durably in completion order. The provider projection normalizes their results to source call order. Mutating tools remain serialized by environment policy where required; the task scheduler itself does not fake concurrency through one `open_effect` slot.

This slice must exercise real `SessionStore` transactions and scripted providers/tools, not the isolated P1 store.

### K3 — Recovery and cancellation

Promote the P1 fault semantics:

- effect intent is durable before dispatch;
- each attempt has stable effect identity plus attempt identity;
- reopen distinguishes retry-safe, reconcile, and no-safe-retry;
- opening/inspection starts no work;
- explicit drive/resume starts eligible work;
- cancellation durably revokes the current invocation generation before signalling local execution;
- settlement-before-cancel wins; otherwise normal completion is fenced and cleanup owns settlement;
- caller wait cancellation never cancels accepted work;
- persistence uncertainty fences the session until reopen/reconciliation.

### K4 — Retained agents and group observations

Move retained agent behavior onto the same kernel rather than a separate orchestration runtime.

- spawn commits identity, supervision, conversation seed, authority ceiling, workspace request and initial input/task atomically where required;
- a spawn command/tool may settle while the retained worker remains addressable;
- waits park as durable dependencies/continuations and do not retain execution capacity needed by the child;
- inspection is read-only and never wakes work;
- frontend drafts and delayed command replies remain keyed by captured target identity.

Once this path covers the old family/lane behavior, delete the displaced lane-based orchestration path.

### K5 — Remove legacy execution concepts

Delete old operation/lane machinery as soon as target coverage is equivalent. Preserve only concepts that still have independent meaning.

Likely deletion/replacement targets include:

- `OperationMachine` as the main agent-turn scheduler;
- `OperationId` where the semantic object is actually a `TaskId`/turn root;
- `lane` as agent/conversation identity;
- singular `open_effect` checkpoint state;
- `submit_if_idle_on_lane` as the main input-admission contract.

Do not rename these in place while retaining their old semantics. Convert callers to the target API, then remove them.

## Persistence topology

The implementation interface should assume one authoritative store **per session/group**, even while the current backend is still physically root-wide during transition.

Leading candidate:

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/

  # optional derived discovery cache once needed
  catalog.sqlite
```

`catalog.sqlite` is not required for correctness and should not be part of initial session semantics. A filesystem scan is a valid simple baseline. If listing/opening many sessions justifies a catalog, it remains rebuildable derived metadata rather than another source of truth.

### What belongs together in `session.sqlite`

Anything that can be part of one semantic session command transaction:

- session/agent/conversation identity and configuration revisions;
- transcript entries and context controls;
- tasks, dependencies, invocation generations and outcomes;
- effect intents, attempts, recovery metadata and settlements;
- input/message admission, request-key receipts and disposition;
- grants, approvals, reservations, usage and budgets;
- workspace/resource ownership metadata needed for session correctness;
- artifact metadata/reference publication.

Splitting these by category into multiple WAL databases would weaken crash atomicity and complicate recovery for no useful semantic gain.

### Large artifacts and provisional output

Large opaque output may live outside SQLite when this avoids pathological row/WAL growth. The session database stores the durable reference and integrity metadata. Publication ordering must ensure a crash can leave an orphan file to reclaim, but cannot commit a required reference to missing content.

Live display deltas are not canonical session truth. They may be coalesced or lost and reconstructed from a fresh snapshot/final durable result. P2 selects checkpoint/spill policy from measurements.

### Independent sessions

Each top-level session may have its own session owner, storage worker/connection and WAL file. That allows unrelated sessions to make progress independently while keeping one deterministic mutation line *inside* each session. Concurrency therefore comes from partitioning at the semantic ownership boundary, not from allowing competing writers to one session's canonical state.

## Storage engine posture

### SQLite: default candidate

Plain SQLite remains the default engine for the core prototype and initial production path because Ion's intended workload matches its strengths:

- local embedded storage;
- short atomic transactions;
- many reads plus one intentional semantic writer per session;
- relational constraints and indexed point/range queries;
- mature crash behavior, tooling and backup support;
- no daemon or network dependency.

WAL's one-writer-per-database rule is not currently a design limitation because Ion intentionally has one semantic writer per session. The writer must never hold a transaction while waiting on model/tool/process work.

### Turso Database: evaluate, do not adopt yet

Turso Database is relevant and should remain on the P2 comparison list. Its Rust implementation, SQLite compatibility, async I/O, MVCC/`BEGIN CONCURRENT`, CDC and future sync capabilities are potentially useful.

However, concurrent writers do not justify changing Ion's semantic ownership model. If canonical state transitions race through multiple write transactions, the database's conflict machinery would replace a deterministic command line with optimistic conflict/retry logic while still requiring Ion to define ordering, cancellation, authority and recovery semantics above it.

Use Turso only if measurements show an engine-level benefit under Ion's actual design, for example:

- SQLite commit/lock/checkpoint overhead becomes material despite per-session partitioning and short transactions;
- asynchronous storage materially improves runtime behavior versus a dedicated blocking worker;
- a future local/remote sync requirement is accepted as a product requirement;
- another Turso capability solves a measured core problem without forcing weaker semantics.

P2 may build an isolated equivalent-schema benchmark against Turso after the core session workload is representative. Do **not** add Turso/libSQL as a production dependency merely to preserve optionality.

### libSQL

libSQL is not a preferred core candidate. It retains SQLite's fundamental single-writer model while adding replication/remote machinery Ion does not currently need. If remote embedded replication becomes a concrete requirement later, reassess the then-current Turso/libSQL options rather than designing around them now.

### Client/server databases

Postgres or another server database would make sense only if Ion adopts a multi-host or multi-user writable-session architecture. That is outside the local-first core. Do not introduce a server just to obtain write concurrency that the session ownership model intentionally does not use.

### No generic storage framework yet

Keep the store implementation private and its semantic interface narrow, but do not build a general multi-backend abstraction before a second engine earns inclusion. Avoid SQLite-specific assumptions in public runtime APIs; inside the store implementation, use SQLite directly and well.

## Why not one global database?

One root-wide SQLite database is operationally simple, but all independent sessions share one writer-lock/WAL/checkpoint/failure domain. That coupling is unnecessary because separate top-level sessions do not require atomic transactions with each other.

The current root-wide database remains valid migration source/evidence. P2 must measure it against per-session stores rather than assuming per-session is faster. The architectural argument for per-session partitioning is primarily **ownership and isolation**; performance is a hypothesis to verify.

## Why not one database per agent?

Agents in one group are not independent persistence domains. Spawning, messaging, supervision, cancellation, task dependencies, budgets and workspace ownership can cross agent boundaries. A database per agent would force distributed transactions/outboxes into the most common coordination path.

The session/group is therefore the smallest useful core database boundary unless P1/P2 evidence disproves that model.

## Experimental systems are deferred

Do not add assignment boards, durable project knowledge, embeddings, vector stores, memory consolidation, or cross-session synchronization to the core migration. First establish a clean single-agent and retained-worker baseline.

Later experiments can ask whether explicit coordination or cross-session information improves verified task outcomes. They must integrate through stable session APIs rather than require a redesign of core execution truth. A feature that harms prompt quality, adds stale state, or duplicates what the root agent already coordinates should be removable without affecting session correctness.

## Storage questions intentionally still open

Do not freeze these until P2 evidence exists:

- UUIDv7 versus compact session-local integer representation for non-session IDs;
- root-wide SQLite versus one SQLite database per session under measured workloads;
- SQLite versus Turso under the *same* representative session workload, if an engine comparison becomes worthwhile;
- whether a rebuildable catalog is needed at all for initial scale;
- output checkpoint frequency and exact provisional-loss bound;
- spill threshold between SQLite and artifact files;
- WAL checkpoint policy;
- page/cache settings;
- transcript/context index shapes;
- artifact retention and GC;
- backup/archive/clone mechanics.

The architectural constraint is stronger than any one physical choice: one session has one semantic mutation owner and one crash-atomic canonical store boundary; independent sessions can be physically independent.

## Promotion rule

A new kernel slice replaces old code only when:

1. its public semantics match the target design;
2. deterministic race/crash tests cover the relevant P1/P2 cases;
3. CI passes formatting, strict workspace clippy and locked workspace tests;
4. no second public API/runtime has been introduced;
5. displaced implementation is deleted or has a concrete next-slice deletion dependency;
6. evidence and known limitations are recorded.

A failed slice is evidence to revise the design. It is not a reason to add a hidden compatibility layer.
