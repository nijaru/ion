# Core rewrite plan

Status: R0 accepted; clean production rewrite is active, 2026-09-12.

This document translates `DESIGN.md` into implementation order. The old runtime is not a compatibility target. Git history is the archive. `docs/source-layout.md` owns source/module organization for the fresh implementation. `docs/r0-kernel-gates-2026-09-12.md` records the accepted pre-rewrite evidence.

## Decision

Do **not** perform a prolonged lane/operation-to-task refactor.

The current implementation encodes several concepts the target removes:

```text
AgentId / Family
lane
OperationId / OperationMachine
single open effect
operation-bound provider signals
root-wide session schema
```

Trying to rename/translate these in place would repeatedly preserve old ownership assumptions while the target uses:

```text
Session
  Conversation
    Entry
    Input
    Task
```

with workers as owned conversations, immutable context controls, task-level recovery and one per-session canonical store.

The five pre-rewrite gates passed at `81c344d713f13b73e19232b4ad36dfe40ba663b9` under CI run `34718467220`. Replace `ion-core` directly. Do not maintain production `old` and `new` runtimes side by side.

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

The async stack is process-local, never durable continuation. `TaskContext::commit` replaces the complete typed checkpoint and performs authorized canonical writes through the session writer after revalidating task identity, invocation generation and cancellation authority.

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

Create a small independent `ion-ai` crate containing the provider-neutral generation contract and scripted service:

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

R0 is closed. Remove the old core implementation and reconstruct `ion-core` around the target and `docs/source-layout.md`.

### Delete/rewrite

Treat these as old-runtime code, not refactor anchors:

- `src/agent.rs`
- `src/agent_host.rs`
- `src/operation/`
- `src/runtime/`
- `src/session/lane.rs`
- old session tree/lane abstractions
- old provider runtime contract in `src/provider.rs`
- old generic effect orchestration
- old context machinery that conflicts with immutable context controls
- old store schema/SQL tied to agents/lanes/operations/effects

Do not create a replacement `runtime.rs` catch-all. The fresh implementation is organized around semantic modules (`conversation`, `task`, `session`, `view`, `store`, built-ins) as defined in `docs/source-layout.md`.

### Application cutover

The current `ion` application crate is tightly coupled to the old core. Do not preserve those dependencies merely to keep the old binary running during the rewrite.

At the clean cutover, choose the smallest workspace that remains honest and green:

- delete/replace old application code that only drives removed core APIs;
- temporarily remove `crates/ion` from the workspace if no useful new shell exists yet, **or** replace it with a genuinely minimal new shell once the new command/view API exists;
- keep `ion-terminal` only as an independently compiling low-level terminal crate pending its later first-principles review;
- reintroduce the full application/TUI incrementally from the new core outward.

User-facing temporary unusability is acceptable; a compatibility bridge to the obsolete runtime is not.

### Review and port algorithms, not APIs

Potentially useful implementation material:

- `tool/`: path validation, output bounding, artifact mechanics;
- `process.rs`: process cleanup/sandbox helpers;
- `policy.rs`: policy checks;
- provider adapters in `crates/ion`: wire parsing and auth knowledge;
- existing crash/race tests as scenario inventories.

Copy or rewrite those pieces only after their new subsystem interface is defined. Do not preserve a type because a leaf algorithm uses it today.

### Defer until the new command/observation contract exists

- `extensions.rs`
- `mcp.rs`
- `rpc.rs`
- CLI ACP adapter
- old session manager
- TUI application integration
- import/export compatibility

Low-level terminal editor/rendering utilities may later be reused after P4 review.

## Fresh implementation order

### K0 — establish `ion-ai` contract crate

Promote the accepted R0.5 provider-neutral types and scripted service into a small independent crate. Keep it contract-only: no production HTTP/OAuth/catalog implementation yet.

This is a dependency boundary, not an AI framework. `ion-core` may depend on it; `ion-ai` must not depend on session/task/store types.

### K1 — storage-independent domain types

Create the fresh module skeleton from `docs/source-layout.md` only as real behavior lands. Do not pre-create empty hierarchy for aesthetics.

Add target identifiers backed by the accepted session-local sequence representation:

```text
SessionId
LocalSeq
ConversationId
EntryId
InputId
TaskId
ArtifactId
CommitSeq
```

Add immutable:

- `Conversation { parent?, owner_task? }`;
- entry generic facets;
- typed input/request receipt state;
- task input/checkpoint/outcome records.

No lanes, operations, durable agents or generic effects.

### K2 — session writer + in-memory store

Build the serialized command line first against a deterministic in-memory store.

Required commands:

- create root/session;
- create conversation/fork/owned conversation;
- append entry/context control;
- accept/dedupe/place input;
- create task/dependencies;
- reserve task invocation;
- checkpoint task;
- mark cancellation;
- terminal closure commit.

Build observations from committed batches.

### K3 — task driver

Implement pending claim, execute/recover/abort invocation ownership, generation fencing, local cancellation signalling, waits/dependencies and close/fault behavior.

No real provider yet. Use deterministic task kinds.

### K4 — SQLite session store

Implement the fresh schema as one database for one session. Do not migrate old tables in place during core development.

Follow the SQLite module boundaries in `docs/source-layout.md`: connection/open policy, schema, atomic commit application and focused per-record reads/writes. Do not create another monolithic `sql.rs`/`queries.rs` file.

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

Each pass may reuse leaf code but starts from the target contract, not from the current module layout.

## Rule

A clean rewrite is not permission to throw away evidence. Preserve the **invariants and failure cases** from old tests and previous prototypes; throw away implementation structure that no longer expresses them cleanly.

The temporary R0 prototype files remain only until equivalent production invariants exist, then are deleted rather than becoming a parallel implementation.