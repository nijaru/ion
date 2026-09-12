# Core rewrite plan

Status: active implementation plan, 2026-09-12.

This document translates `DESIGN.md` revision 5 into implementation order. The old runtime is not a compatibility target. Git history is the archive. `docs/source-layout.md` owns source/module organization for the fresh implementation.

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

Trying to rename/translate these in place would repeatedly preserve old ownership assumptions while the target now uses:

```text
Session
  Conversation
    Entry
    Input
    Task
```

with workers as owned conversations, immutable context controls, task-level recovery and one per-session canonical store.

After the five pre-rewrite gates below pass, replace `ion-core` directly. Do not maintain a production `old` and `new` runtime side by side.

## Pre-rewrite gates

### R0.1 — Task contract

Prototype the final Rust task authoring interface in isolation.

Required shape:

```text
TaskKind
  Input
  Checkpoint
  Completed
  Failure
  Aborted

  execute(...)
  recover(...)
  abort(...)
```

`execute/recover/abort` are async invocations, but the async stack is never durable continuation state. `TaskContext::commit` replaces the complete checkpoint and makes authorized canonical writes through the session writer. Each commit revalidates task identity + invocation generation + cancellation.

The terminal result is a closure/plan applied on the writer so outcome, successor work, ownership changes and scratch retirement are atomic.

Also prototype an optional typed state/phase helper that compiles to the ordinary task trait. Reject any design that creates a second scheduler or public task framework.

Test:

- checkpoint survives abrupt process loss;
- stale invocation writes reject;
- settle-before-cancel and cancel-before-settle;
- abort uses a fresh invocation after old invocation joins;
- cancelled waiter does not cancel durable task;
- panic/failure cannot silently lose running work.

### R0.2 — Entries/context/forks

Prototype immutable entries with generic facets:

```text
kind
semantic data
provider-neutral model projection
optional context head
optional constrained context edits
```

No mutable canonical context vector.

Test:

- append-only transcript;
- reset/handoff by new head;
- summary compaction with retained tail;
- fork inherits only the source prefix and controls visible at its cutoff;
- later source changes invisible;
- source tasks not inherited;
- two tool results settle B then A while provider projection is A then B;
- cold/warm context reads and edit/head indexes are measurable.

Start worker inheritance only at complete safe exchange boundaries. Arbitrary historical incomplete-exchange repair may be added later if worth the extra semantics.

### R0.3 — Remove generic Effect

Prototype provider/tool/job recovery using only task identity + typed checkpoint + invocation generation + attempt/usage records.

Representative checkpoints must distinguish:

```text
prepared but not dispatched
dispatched retry-safe attempt
dispatched reconcile/adopt handle
dispatched no-safe-retry operation
```

Crash after external dispatch and before terminal commit. Reopen and verify retry/adopt/indeterminate behavior.

Only keep a separate `Effect` entity if this prototype exposes a concrete identity/lifetime that cannot be represented cleanly as task + attempt/checkpoint.

### R0.4 — IDs and sequence

Compare two private schema representations:

A. typed object IDs + separate commit sequence;
B. one session-local monotonic mutation sequence that also mints object IDs.

Both must support:

- several objects created and cross-referenced in one batch;
- rejected batch consumes/publishes no visible IDs;
- historical/fork cutoff ordering;
- compact SQLite indexes;
- stable typed Rust public handles;
- `(SessionId, local ID)` cross-session references when required.

Choose the simpler measured representation before the fresh production schema is declared.

### R0.5 — Minimal AI port

Specify only what the fresh generation task needs:

```text
ModelRef
ModelRequest
provider-neutral Message/Content
ToolSpec
ModelStreamEvent
ModelResponse
Usage
ProviderErrorKind
ModelService::stream(...)
```

The model service must not know session/task IDs or database commands.

Use a scripted/faux implementation for the first core. Production provider catalog/auth/wire adapters are a separate component pass.

The leading organization is a small independent `ion-ai` crate containing only this provider-neutral contract plus the scripted service. R0.5 may reject that crate split if it proves artificial, but do not put HTTP/OAuth/provider-catalog logic in `ion-core` merely to avoid one stable dependency boundary.

## Rewrite boundary

Once R0.1–R0.5 are accepted, remove the old core implementation and reconstruct `ion-core` around the target and `docs/source-layout.md`.

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

The current `ion` application crate is tightly coupled to the old core. Do not preserve those dependencies just to keep the old binary running during the rewrite.

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

## Fresh `ion-core` implementation order

### K1 — Storage-independent domain types

Create the fresh module skeleton from `docs/source-layout.md` only as real behavior lands. Do not pre-create empty hierarchy for aesthetics.

Add only target nouns:

```text
SessionId
ConversationId
EntryId
InputId
TaskId
ArtifactId
```

plus selected sequence representation.

Add immutable:

- `Conversation { parent?, owner_task? }`;
- entry generic facets;
- typed input/request receipt state;
- task input/checkpoint/outcome records.

No lanes, operations, agents or generic effects.

### K2 — Session writer + in-memory store

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

### K3 — Task driver

Implement pending claim, execute/recover/abort invocation ownership, invocation fencing, waits/dependencies and close/fault behavior.

No real provider yet. Use deterministic task kinds.

### K4 — SQLite session store

Implement the fresh schema as one database for one session. Do not migrate the old tables in place during core development.

Follow the SQLite module boundaries in `docs/source-layout.md`: connection/open policy, schema, atomic commit application and focused per-record reads/writes. Do not create another monolithic `sql.rs`/`queries.rs` file.

For old development data, preserve/archive/refuse according to the pre-1.0 policy. A migration can be written later only if preserving old sessions is actually valuable.

### K5 — Generation + tool chain

Use the minimal scripted model service and narrow tool executor.

Prove:

```text
input
 -> generation
 -> tool A + tool B
 -> post-tools join
 -> final generation
```

with B settling before A while projected order remains A,B.

### K6 — Workers

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

Compare root-wide legacy SQLite against one-DB-per-session with production-like history/output load. Move to the per-session topology only after the measurement/fault evidence is recorded.

## AI subsystem pass after the kernel

The later AI/model component should follow this logical split:

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

Intentional Ion improvements:

- typed provider failure categories instead of retry policy based on message substrings;
- generation task owns durable retry/backoff/usage accounting;
- hidden SDK retries disabled/controlled;
- no session/task IDs inside provider API;
- credentials never enter session storage.

Do not implement the full provider catalog before the core can run one scripted model turn. If `ion-ai` was accepted by R0.5, extend that crate rather than moving provider/network logic back into the application binary or `ion-core`.

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