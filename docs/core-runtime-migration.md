# Core rewrite plan

Status: K0-K2 implemented; K3 core task driver validated and still in progress, 2026-09-12.

This document translates `DESIGN.md` into implementation order. The old runtime is not a compatibility target. Git history is the archive. `docs/source-layout.md` owns source/module organization for the fresh implementation. `docs/r0-kernel-gates-2026-09-12.md` records the accepted pre-rewrite evidence.

The clean cutover has already happened. The legacy `ion-core` lane/agent/operation/effect runtime and prototype harnesses were physically removed at cleanup commit `2d273f78`; the active workspace now contains only `ion-ai`, `ion-core` and `ion-terminal`.

## Decision

Do **not** reintroduce a lane/operation compatibility layer.

The removed implementation encoded concepts the target intentionally dropped:

```text
AgentId / Family
lane
OperationId / OperationMachine
single open effect
operation-bound provider signals
root-wide session schema
```

The fresh implementation instead uses:

```text
Session
  Conversation
    Entry
    Input
    Task
```

with workers as owned conversations, immutable context controls, task-level recovery and one per-session canonical store.

The five pre-rewrite gates passed at `81c344d713f13b73e19232b4ad36dfe40ba663b9` under CI run `34718467220`. Current clean-core code checkpoint `a33e0fb22073857cc724244b23f99e0c12249147` passes format, strict Clippy and full workspace tests under CI run `34725190754`.

## Accepted R0 contracts

### R0.1 — task contract: accepted

Use one typed task framework:

```text
TaskKind
  Input
  Checkpoint
  Completed
  Failure
  Aborted

  async execute(...)
  async recover(...)
  async abort(...)
```

The async stack is process-local, never durable continuation. `TaskContext` checkpoint commits replace the complete durable checkpoint/output through the session writer after revalidating task identity, invocation generation and cancellation authority.

Cancellation is durable mark + process-local signal. The durable mark revokes the old invocation's normal authority; after that invocation joins, a fresh higher-generation abort invocation owns cleanup and terminal abort settlement.

An optional typed checkpoint/phase helper may make exhaustive task authoring easier but creates no second scheduler/lifecycle.

### R0.2 — immutable entries/context/forks: accepted

Canonical history is append-only. Effective model context is derived from immutable entries containing provider-neutral projection plus optional heads and constrained edits.

Summary, handoff and reset append heads. Forks reference a stable source cutoff and never observe later source appends. Initial inherited cutoffs/heads must be complete tool-exchange boundaries. Transcript chronology may record completion order while projection normalizes tool results to source call order.

There is no separately mutable canonical context vector.

### R0.3 — generic Effect: rejected

Do not create a generic `EffectId`/effect lifecycle in the fresh core.

Use task identity + typed checkpoint + invocation generation + task-scoped attempt/usage evidence + an external reconciliation/idempotency identity where one exists.

Recovery remains typed per task phase:

```text
retry-safe
reconcile/adopt
no-safe-retry -> indeterminate
```

Reopen this only if a concrete future operation exposes a truly independent identity/lifetime that task state cannot represent cleanly.

### R0.4 — IDs/sequence: accepted

Use one private monotonic local sequence per session store. Durable local objects and committed mutation batches receive successive values from that allocator, while Rust exposes distinct semantic wrappers:

```text
ConversationId(LocalSeq)
EntryId(LocalSeq)
InputId(LocalSeq)
TaskId(LocalSeq)
ArtifactId(LocalSeq)
CommitSeq(LocalSeq)
```

A rejected transaction consumes/publishes no durable sequence values. A batch may mint several object IDs and then a later commit cursor. `SessionId` remains globally unique; cross-session references pair it with a typed local ID.

### R0.5 — minimal AI boundary: accepted

The independent `ion-ai` crate contains the provider-neutral generation contract and scripted service:

```text
ModelRef
ModelRequest
Message / Content
ToolSpec
ModelStreamEvent
ModelResponse
Usage
ProviderError / ProviderErrorKind
ModelService::stream(...)
```

The model service receives no session/task IDs or database commands. Production HTTP/auth/catalog adapters come later. Durable retry/backoff/usage policy belongs to generation tasks, not hidden provider/SDK retries.

## Rewrite boundary

The old core has already been removed. Do not rebuild it beside the fresh core or add compatibility aliases that reproduce its ownership model.

### Deleted/replaced

The following old-runtime areas are now Git-history-only reference material:

- old `agent.rs` / `agent_host.rs` family runtime;
- `operation/` and `runtime/` orchestration;
- lanes and old session tree abstractions;
- old provider runtime contract;
- generic effect orchestration;
- mutable context machinery that conflicted with immutable controls;
- old store schema/SQL tied to agents/lanes/operations/effects;
- old tool/runtime unit/integration suites and R0/P1/P2 prototype support trees.

Do not create a replacement `runtime.rs` catch-all. The fresh implementation is organized around semantic modules (`conversation`, `task`, `session`, `view`, `store`, built-ins) as defined in `docs/source-layout.md`.

### Application cutover

The old `crates/ion` application crate has been removed from the active workspace rather than bridged to deleted core APIs. The workspace currently contains:

```text
crates/ion-ai
crates/ion-core
crates/ion-terminal
```

Reintroduce a minimal application shell only after the new command/view/store boundaries are useful enough to drive honestly. Keep `ion-terminal` as an independently compiling low-level terminal crate pending its later first-principles review.

### Review and port algorithms, not APIs

Potentially useful historical implementation material remains in Git history:

- tool path validation, output bounding and artifact mechanics;
- process cleanup/sandbox helpers;
- policy checks;
- provider wire parsing/auth knowledge;
- crash/race tests as scenario inventories.

Copy or rewrite those pieces only after their new subsystem interface is defined. Do not preserve a type because a leaf algorithm used it previously.

### Still deferred

- extensions;
- MCP;
- RPC/ACP/JSON adapters;
- full session manager/application shell;
- TUI application integration;
- import/export compatibility.

Low-level terminal editor/rendering utilities may later be reused after P4 review.

## Fresh implementation order

### K0 — establish `ion-ai` contract crate

**Implemented and validated.**

The accepted R0.5 provider-neutral types and scripted service live in a small independent crate. It remains contract-only: no production HTTP/OAuth/catalog implementation yet.

`ion-core` depends on it; `ion-ai` does not depend on session/task/store types.

### K1 — storage-independent domain types

**Implemented and validated.**

The fresh domain contains the accepted identifiers and durable target nouns only:

```text
SessionId
LocalSeq
ConversationId
EntryId
InputId
TaskId
ArtifactId
CommitSeq

Session
Conversation
Entry
Input
Task
TaskOutput
Artifact
```

Conversations have independent history-parent/cutoff and owner-task edges. Entries are immutable and carry provider-neutral projections/context controls. Tasks contain kind/schema, immutable input, checkpoint/output, dependency, ownership, cancellation, generation/invocation and terminal state.

No lanes, operations, durable agents or generic effects remain.

### K2 — session writer + in-memory store

**Implemented and validated.**

The deterministic in-memory kernel now provides:

- root/session creation;
- independent/forked/owned conversation creation;
- immutable entry/context commits;
- exact request-key input admission replay/conflict;
- explicit input disposition;
- task/dependency creation;
- execute/recover/abort reservation;
- generation-fenced checkpoint/output replacement;
- durable cancellation mark;
- atomic terminal closure with successor/ownership writes;
- bounded committed observations and resnapshot-on-overflow;
- rejected-transaction rollback with no published sequence consumption.

K2 lifecycle primitives are exercised by K3 production code instead of being test-only helpers.

### K3 — task driver

**In progress; core driver slice validated.**

Implemented:

- explicit `TaskDriver::drive_task` with no automatic work on snapshot/inspection;
- registry by task kind + schema revision;
- pending -> execute, running -> recover, cancelled -> abort invocation selection;
- async task futures outside the session mutation lock;
- invocation-scoped `TaskContext` checkpoint commits through the writer;
- durable cancellation mark plus process-local `CancellationToken` signal;
- fresh abort generation after the old invocation joins;
- local duplicate-drive rejection;
- dependency readiness check at reservation;
- missing implementation for never-dispatched work -> durable `Unsupported`;
- unavailable implementation for already-running work -> recovery blocked without consuming a generation or writing state;
- known application failure -> durable `Failed`; unresolved external uncertainty -> durable `Indeterminate`;
- handler interruption (`TaskRunError`, panic, dropped future) -> no settlement, task stays running and recoverable;
- serialized cancel/settle decision: settlement wins if it commits first, otherwise abort owns cleanup;
- initial abort dispatch through the shared invocation path with separate bounded cleanup admission.

Still open:

- restricted typed finalization (non-noop terminal plan/closure) and typed task authoring adapter;
- writable ownership release after local joins (K4 SQLite).

Storage-independent waits, capacity and close are implemented: client waits recheck committed state after notifications; dependency waits precede independent resource permits; admitted drives outlive callers; graceful close signals and joins, while fault close aborts and joins async futures. Both fence canonical writes without durably cancelling unfinished work. Dependencies are immutable backward references; dynamic invocation waits are not exposed. Persistence-dependent work remains:

- persistence-backed reopen/recovery tests once K4 exists.

Do not add a second scheduler to solve these. Extend the same session/task driver boundary.

### K4 — SQLite session store

**Paused before SQL for bounded-state and task-contract work.**

The resident/persistence split below establishes commit ordering only. Full-history clones remain in transaction drafts and snapshots. Before SQL, define bounded typed reads plus transaction overlays (or a justified bounded working set), restricted task finalization with same-batch references, typed authoring adaptation, and foreground-turn membership. Also strengthen safe context validation before K5. These are structural contracts, not P2 index tuning.

Implement a fresh schema as one database for one session. Do not migrate old tables in place during core development.

Follow the SQLite module boundaries in `docs/source-layout.md`: connection/open policy, schema, atomic commit application and focused per-record reads/writes. Do not create another monolithic `sql.rs`/`queries.rs` file.

The pre-SQL resident/persistence split is implemented. `Session` owns `SessionState`, and the crate-private `Persistence` interface accepts validated batches. The transaction's prepared state is installed only after a successful commit; failed persistence leaves resident records and observations unchanged, fences canonical writes, and signals local fault shutdown. `MemoryStore` retains only volatile commit ordering, not another semantic state copy. The resulting boundary is:

```text
Session owner
  resident SessionState
  transaction builder
  observations
  persistence store

commit path
  validate/build batch against resident state
  -> store.commit(batch) durably
  -> apply batch to resident state/indexes
  -> publish observation
```

The exact private Rust shape may be a narrow store trait, enum or another simpler representation; do not generalize it into a public pluggable database framework. The important invariant is persistence-before-resident-apply/publish with exactly one semantic writer.

On open, SQLite reconstructs or lazily supplies the resident state needed by the kernel. Opening/inspection must not drive tasks. Running tasks remain durable records and are entered through explicit recovery drive.

For old development data, preserve/archive/refuse according to the pre-1.0 policy. A migration can be written later only if preserving old sessions is actually valuable.

### K5 — generation + tool chain

Use `ion-ai`'s scripted model service and a narrow tool executor.

Prove:

```text
input
 -> generation
 -> tool A + tool B
 -> post-tools join
 -> final generation
```

with B settling before A while projected order remains A,B.

### K6 — workers

Create owned conversations through the same writer:

- fresh context;
- inherited context at safe cutoff;
- joined run;
- retained spawn;
- send/follow-up;
- wait without monopolizing model/tool capacity;
- cancel/retire;
- nested ownership limits.

No separate agent registry.

### K7 — P2 physical/session-store pass

Compare root-wide legacy SQLite evidence against one-DB-per-session with production-like history/output load. Move the production physical topology only on recorded measurement/fault evidence; the semantic session boundary is already fixed.

## AI subsystem pass after the kernel

The later AI/model component extends `ion-ai` using this logical split:

```text
ModelService / registry
  Provider
    auth + model catalog + endpoint policy
      API adapter
        HTTP/SSE/WebSocket protocol
```

Strong ideas from `pi-ai` to evaluate:

- provider-neutral messages/tools/stream results;
- provider runtime separated from reusable wire API implementations;
- providers own auth resolution and model listing;
- app owns credential persistence;
- dynamic model catalogs refresh explicitly and cache outside agent sessions;
- provider-specific opaque replay metadata survives on otherwise provider-neutral assistant content;
- faux provider for deterministic tests.

Ion requirements:

- typed provider failure categories instead of retry policy based on message substrings;
- generation task owns durable retry/backoff/usage accounting;
- hidden SDK retries disabled/controlled;
- no session/task IDs inside provider API;
- credentials never enter session storage.

Do not implement the full provider catalog before the core can run one scripted model turn.

## Subsequent subsystem passes

After the core and AI boundary are stable, audit each subsystem from first principles in this order:

1. execution environment + built-in tools + jobs/workspaces/sandbox;
2. model/provider/auth/catalog implementation;
3. TUI observation/rendering/input architecture;
4. extensions/hooks/MCP and dynamic authority;
5. ACP/JSON/RPC/external client adapters;
6. settings/export/import/update packaging.

Each pass may reuse leaf code but starts from the target contract, not from the old module layout.

## Rule

A clean rewrite is not permission to throw away evidence. Preserve the **invariants and failure cases** from old tests and previous prototypes; throw away implementation structure that no longer expresses them cleanly.

The historical prototypes are evidence inventories only. Do not restore them as parallel production implementations.