# Core runtime migration strategy

Status: active implementation plan, 2026-09-12.

This document turns the target contracts in `DESIGN.md` into an implementation sequence. It is deliberately subordinate to the design and evidence gates: if production tests or measurements invalidate a boundary, change the plan rather than preserving an implementation artifact.

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

## Core nouns to converge on

- **Session**: one durable coordination and transaction boundary.
- **Agent**: retained identity with configuration, authority and a current conversation.
- **Conversation**: immutable transcript plus explicit model-context state and fork provenance.
- **Input**: admitted user/agent input with delivery mode, request identity and disposition.
- **Task**: generic durable executable unit with typed kind state, dependencies, invocation generation and terminal outcome.
- **Effect**: durable external-effect intent plus attempt/recovery classification and settlement.
- **Job**: environment-backed long-lived work represented through durable tasks/effects.
- **Assignment**: optional coordination state, not scheduler state.
- **Knowledge item**: optional project/workspace information, not transcript or task state.

Do not introduce another permanent synonym for these concepts during migration.

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

The implementation interface should assume one authoritative store **per session**, even while the current backend is still physically root-wide during transition.

Target candidate:

```text
Ion data root/
  catalog.sqlite
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/
  projects/
    <KnowledgeSpace>/
      knowledge.sqlite       # optional, later
  indexes/                   # optional/rebuildable
```

### What belongs together in `session.sqlite`

Anything that can be part of one semantic session command transaction:

- session/agent/conversation identity and configuration revisions;
- transcript entries and context controls;
- tasks, dependencies, invocation generations and outcomes;
- effect intents, attempts, recovery metadata and settlements;
- input/message admission, request-key receipts and disposition;
- grants, approvals, reservations, usage and budgets;
- same-session coordination/assignment state if E1 is enabled;
- artifact metadata/reference publication.

Splitting these by category into multiple WAL databases would weaken crash atomicity and complicate recovery for no useful semantic gain.

### What can live separately

A separate database/store is justified when its lifecycle and failure domain are genuinely independent:

- `catalog.sqlite`: rebuildable session discovery metadata only;
- `knowledge.sqlite`: optional project/workspace-scoped cross-session knowledge, if E2 earns inclusion;
- search/vector indexes: rebuildable derived data;
- logs/telemetry caches: optional independent data, if introduced;
- credentials: host credential storage, not these databases.

Cross-store authoritative publication uses explicit idempotent/outbox protocols. Ion must not pretend an attached-WAL multi-file transaction gives one crash-atomic commit.

## Optional coordination and knowledge

These systems should be designed into the boundaries now without being implemented as mandatory core behavior.

### Assignment/task coordination (E1)

A future assignment board may expose owner/claim state, revisions, dependencies, evidence/result refs, and optional advisory path scopes. It is **not** the durable task scheduler. If an assignment belongs to one session/group and needs atomic coordination with messages/ownership, it lives in that session database. Model-facing assignment tools are independently toggleable.

The baseline to beat is simply `spawn/send/wait/result`. E1 graduates only if controlled tests show less duplicated/missed work for acceptable token/tool/latency cost.

### Project knowledge (E2)

Cross-session knowledge has a different lifetime and should not be placed in every `session.sqlite`. Candidate records require provenance, revision/freshness, supersession/invalidation and explicit trust status. Retrieval is bounded and inspectable; no hidden automatic prompt injection.

Start with exact/lexical retrieval. Embeddings/vector indexes are derived optional machinery only if measured task/retrieval quality justifies them. Publication from a session uses an idempotent durable outbox; knowledge-store failure must not make the source session unrecoverable.

## Storage questions intentionally still open

Do not freeze these until P2 evidence exists:

- UUIDv7 versus compact session-local integer representation for non-session IDs;
- output checkpoint frequency and exact provisional-loss bound;
- spill threshold between SQLite and artifact files;
- WAL checkpoint policy;
- page/cache settings;
- transcript/context index shapes;
- artifact retention and GC;
- backup/archive/clone mechanics;
- derived search/vector engine choice, if any.

The architectural constraint is stronger than any one physical choice: atomic session truth has one owner/store boundary; independently-lived data can be separated.

## Promotion rule

A new kernel slice replaces old code only when:

1. its public semantics match the target design;
2. deterministic race/crash tests cover the relevant P1/P2 cases;
3. CI passes formatting, strict workspace clippy and locked workspace tests;
4. no second public API/runtime has been introduced;
5. displaced implementation is deleted or has a concrete next-slice deletion dependency;
6. evidence and known limitations are recorded.

A failed slice is evidence to revise the design. It is not a reason to add a hidden compatibility layer.
