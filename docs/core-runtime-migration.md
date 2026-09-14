# Core rewrite plan

Status: K0–K5 foundations and part of K6 implemented; review repair gate active, 2026-09-13.

`ROADMAP.md` §1 is the current execution order: R1–R5 correctness repairs, bounded storage and a real single-agent coding baseline precede further joined-worker expansion. The K/P sections below describe subsystem scope and historical implementation order, not a competing next-task list. Revision-7 contract changes in `DESIGN.md` are accepted targets, not completed repairs.

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

The five pre-rewrite gates passed at `81c344d713f13b73e19232b4ad36dfe40ba663b9` under CI run `34718467220`. The K3 core-driver checkpoint `a33e0fb22073857cc724244b23f99e0c12249147` (CI run `34725190754`) is historical validation evidence, not a statement about the current head.

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

**Core driver, turn control and exclusive ownership implemented.**

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
- initial abort dispatch through the shared invocation path with separate bounded cleanup admission;
- restricted finalization: `TaskCompletion::with_plan` commits transcript entries and successor tasks atomically with the outcome; plan-local handles resolve to session IDs only at commit, and any error rolls back the settlement and every planned write;
- foreground-turn membership: `Session::create_turn` opens one authoritative slot per conversation, plan successors inherit the turn unless marked background, `TaskDriver::cancel_turn` durably cancels every non-terminal task in the turn and then signals local invocations, and the last terminal member releases the slot; the root's settlement alone does not.

Still open:

- reclamation beyond archival retirement (later K6); planned owned-conversation creation is implemented.

The typed authoring adapter is implemented in `task/typed.rs` and erases into the ordinary registry. Decode failures are terminal only for never-dispatched work; for an already-dispatched task they interrupt and stay recoverable, and an un-encodable result interrupts rather than discarding its reconciliation opportunity.

Storage-independent waits, capacity and close are implemented: client waits recheck committed state after notifications; dependency waits precede independent resource permits; admitted drives outlive callers; graceful close signals and joins, while fault close aborts and joins async futures. Both fence canonical writes without durably cancelling unfinished work. Dependencies are immutable backward references; dynamic invocation waits are not exposed. Persistence-dependent work remains:

- nothing on this boundary: cross-process dispatch-window crash evidence is witnessed at `8c91cb28`, and fail-closed checkpoint reading landed at `9c19a331`.

Do not add a second scheduler to solve these. Extend the same session/task driver boundary.

### K4 — SQLite session store

**Per-session SQLite store, exclusive ownership and process-death evidence implemented.**

`Session::create(path)` and `Session::open(path)` are live. One database holds one session at schema version 5 (version 4 added the turn cancellation barrier and version 5 changed the meaning of a serialized input disposition; older development databases are refused rather than migrated): session metadata and cursors, conversations, entries, inputs with their durable request-key and admission-commit mappings, and tasks with dependency and ownership child tables. A commit is applied in one SQLite transaction that also advances the commit cursor with a compare-and-set on the cursor the batch was built against, so a stale live authority is fenced instead of interleaved. The durability floor is WAL with `synchronous = FULL`.

Evidence is in-process plus one real process-death test (`tests/k4_sqlite.rs`). In-process: a representative write set — history-parented conversation, entries, input admission and disposition, a pending task, a dependent task, a task-owned conversation, two turns with different foreground-slot outcomes, a checkpoint, a finalization plan and a durable cancellation mark — reconstructs an identical snapshot after close and reopen; duplicate-input replay survives reopen; a stale second authority is fenced and left closed; and an interrupted task reopens as running with nothing implicitly started. Process death: a child process commits three entries and then dies on `SIGABRT` without unwinding, closing or checkpointing; the parent finds all three commits durable and recovery idempotent.

What that does **not** establish: durability under machine power loss or kernel failure, which depends on the filesystem honouring `fsync` and cannot be tested from inside one machine. The claim is scoped to process death, which is what the test actually observes.

`TaskDriver::{create, open, create_with_capacity, open_with_capacity}` are the client entry points, so a caller never constructs a `Session` itself; opening through the driver still reads only.

Ownership is exclusive and distinguishable: `Session::open`/`create` take a kernel-held lock before touching SQLite and release it last during close, a second live process fails with `SessionError::SessionInUse` before it reconstructs anything, and reconstruction reads one transaction. A read-only inspection mode would need a shared lock and does not exist.

Still open for this slice: cold-history reads, since the resident store still holds every record after open (the per-commit map clone was removed at `54367ada`); a `Session::create` path that refuses an occupied file is covered, but backup/repair and orphan-artifact handling are not; and index tuning.

The pre-SQL work this section used to gate on is complete: the resident/persistence split, restricted task finalization with same-batch references, typed authoring adaptation, foreground-turn membership, safe context validation, and the bounded/fallible read and observation contract (invariants 7-9 below; invariant 6 lands with the SQLite read path).

The pre-SQL resident/persistence split is implemented. `Session` owns `SessionState`, and the crate-private `Persistence` interface accepts validated batches. The transaction prepares changes in place under exclusive mutation authority through a rollback journal. Failed persistence restores resident records before fencing canonical writes and signalling local fault shutdown; observations publish only after success. `MemoryStore` retains only volatile commit ordering, not another semantic state copy. The resulting boundary is:

```text
Session owner
  resident SessionState
  transaction builder
  observations
  persistence store

commit path
  journal resident/index changes and validate batch (unobservable)
  -> store.commit(batch) durably (failure: rollback and fence)
  -> publish observation
```

The exact private Rust shape may be a narrow store trait, enum or another simpler representation; do not generalize it into a public pluggable database framework. The invariant is persistence-before-observability with exactly one semantic writer; prepared resident changes remain hidden and reversible until commit.

#### Residency decision

Semantic ownership is not the same as keeping every record in memory. `Session` owns the semantics, but it must not require full-history residency, and no commit may cost O(history).

Chosen shape: **typed indexed reads plus a small transaction overlay**, extended by a copy-on-write resident representation. Decision records and session state hold records behind `Arc`, and the in-place journal retains previous values for touched records without cloning map structures. A mutation copies only the records it touches. This is preferred over a bounded resident working set with ad-hoc loaders because it avoids hand-built cache invalidation and keeps one read path.

Required K4 invariants:

1. checkpointing an active task must not copy or hydrate unrelated historical task payloads (satisfied by copy-on-write resident records, covered by `commit_copies_only_touched_task_records`);
2. session summaries and observation recovery use bounded views, not full-state snapshots (`SessionSummary`, `Session::conversation_entries`);
3. context construction at a frozen cutoff can run without holding the mutation line;
4. a durable commit followed by an unexpected resident-apply failure is a fail-stop/reopen condition, never silent divergence;
5. per-commit map-structure cloning is absent (implemented at `54367ada`); bounded residency and cold indexed reads remain open independently.

Index tuning, measured thresholds and cold-history paging remain P2 work; these are structural requirements.

Also required before the SQLite interface is frozen (flagged by the 2026-09-13 design review):

6. reads must be fallible — a missing record and a storage failure are different results, so `Option<Record>` alone is insufficient once a real backend exists. **Still open:** the resident read path cannot fail, so this lands with the SQLite read path rather than as speculative `Result` plumbing;
7. ~~the driver must expose bounded reads (summary, record lookup, transcript page) and observation polling~~ **done:** `TaskDriver::{summary, task, conversation_entries, observations_after, wait_observations}`; covered by `tests/k4_reads.rs`;
8. ~~observation recovery needs a coverage floor~~ **done:** `observations_after` returns `reset_required` for a cursor older than retained coverage or ahead of the last commit, and `None` means "everything retained" without asking for a reset;
9. ~~every projection whose state changes must be invalidated~~ **done:** opening and releasing the foreground slot both emit `ForegroundTurnChanged`, asserted in `tests/k4_reads.rs`.

Expensive frozen-cutoff context construction must move outside mutation authority; indexed storage alone does not fix the current call structure.

On open, SQLite reconstructs or lazily supplies the resident state needed by the kernel. Opening/inspection must not drive tasks. Running tasks remain durable records and are entered through explicit recovery drive.

For old development data, preserve/archive/refuse according to the pre-1.0 policy. A migration can be written later only if preserving old sessions is actually valuable.

### K5 — generation + tool chain

**Implemented over `ion-ai`; provider catalogue and mid-turn steering remain.**

The production built-ins live in `crates/ion-core/src/builtin/`. `Builtins` registers `generation`, `tool` and `post_tools` under their canonical names over one `ModelService` and one `ToolCatalog`; the scheduler branches on no built-in name. `tests/k5_chain.rs` keeps the shape proof with scripted in-test kinds, and `tests/k5_generation.rs` runs the same chain through the real kinds:

```text
input
 -> generation
 -> tool A + tool B
 -> post-tools join
 -> final generation
```

A generation invocation reads its own transcript in bounded pages, projects it with `conversation::context::project`, and records the complete `ModelRequest`, its transcript cutoff and the ids of the placed inputs that request included in its checkpoint *before* dispatch, together with an attempt count. Accepted input is already in the transcript: the session writer placed it when it bound the input to the turn that answers it, so the invocation adds no input messages and the settlement consumes nothing. Recovery replays that recorded request instead of rebuilding one from changed history, inputs or configuration, and only a first attempt derives a request from current state. A checkpoint that exists but cannot be read is a third case: it is not a first attempt, so the invocation settles `Indeterminate` without rebuilding or dispatching. The settlement appends the assistant entry and creates the tool children plus the join: the accepted input is already placed by the writer, so a settlement places and consumes nothing. The recorded input ids come from the projection that built the request, not from a cutoff comparison, so an edit that removes an input's content is not reported as included. A stream that ends without an explicit `Completed` settles `Failed` with its evidence and appends nothing, so a partial answer cannot become history. In-flight model calls observe the durable cancellation signal.

The tool kind runs exactly one call against the catalogue and owns its own result entry. It records a durable dispatch before handing the call over, so recovery distinguishes "never dispatched" (safe to run) from "outcome unknown": a non-retry-safe call whose dispatch was recorded but never settled by a tool error, cancellation or lost invocation settles `Indeterminate` with a recorded result that states the uncertainty, rather than repeating the external action or claiming it was stopped. `Tool::retry_safe` is the explicit opt-in for a repeat-safe call, and the dispatch record freezes the policy that was in force at hand-over: a repeat needs the recorded policy and the current one to allow it, so a tool that is later removed, replaced or made retry-safe cannot retroactively resolve an older uncertain call. A checkpoint that exists but cannot be read as this call's dispatch record is treated as uncertain, not as "never dispatched". The post-tools join appends nothing: it makes the continuation generation runnable only once every call has a recorded result, so a split exchange cannot become model context.

`TaskDriver::cancel_turn` marks the turn's non-terminal members, signals the live invocations, and drives abort cleanup for members that never dispatched, since those cannot observe a local signal. Together with per-tool result ownership this closes a cancelled exchange and releases the foreground slot without a manual drive.

Submission places an input and binds it to a generation task in one commit: `Session::admit_input` applies the mode/state policy from `DESIGN.md` §11 in one commit, starting the turn that answers the input when the mode and conversation state call for one and otherwise queueing it; a refused admission admits nothing. `TaskDriver::admit_input` uses the driver's configured `TurnTemplate` for the turn it may start, `TaskDriver::submit_input` takes an explicit turn request, and `Session::queue_input` is the queue-only primitive. The generation places and consumes no input. `TaskContext::placed_inputs` reads only the accepted inputs whose placement names the invocation's own turn, and is provenance for the frozen request rather than a queue. Exact request-key replay is unchanged and never opens a second turn; a replay of an admission that queued reports the input as queued rather than inventing a turn.

Queued input is drained by the settlement that releases a conversation's turn slot, scoped to the conversations whose slot that settlement actually released, one input per successor turn and in admission order, driven by the driver rather than a coordinator task. `TaskDriver::schedule_next_turn` exposes the same step explicitly, which is how a conversation reopened with durable queued input resumes; without a configured `TurnTemplate` nothing is scheduled automatically, and a turn whose kind the driver cannot run is never bound at all.

Still open for this slice:

- a production provider catalogue (auth, wire adapters, model listing);
- mid-turn steering: a steer on a busy conversation is queued and delivered at the next turn boundary, but injecting it into a running generation's context is unbuilt;
- a paused/idle-policy control per conversation, which the mode table references but does not define;
- context/output bounds, and the readiness scan's per-settlement dependency-list comparison, which is index work under P2;
- per-tool reconciliation/adoption. A tool can today only declare retry safety; a tool that needs to query an external identity to adopt a prior attempt has no seam for it yet, so the execution-environment pass owns that;
- cleanup created by cleanup. A successor born cancelled during abort cleanup still needs an explicit drive to settle; auto-driving it would let a pathological kind generate cleanup recursively, so it stays bounded until a depth or budget rule exists.

### K6 — workers

**Retained spawn, archive, own-turn runs and closure receipts implemented; further expansion deferred behind the repair gate and coding baseline.**

`TaskPlan` can create owned conversations: `create_conversation` returns a plan-local handle, planned entries and successors may target it, and the conversation is created first, owned by the settling task with the reciprocal edge, in the same commit as the outcome. Context seed is explicit (`PlannedConversation::fresh` or `inherited` at a stable cutoff validated like any fork), and the plan bounds cover conversations as well as entries, inputs and tasks. `tests/k6_workers.rs` covers fresh and inherited workers, ownership reciprocity, seeding a worker with a brief and a retained task, reopen, a foreign plan handle, an invisible cutoff and the conversation bound.

The production entry point is `builtin::worker`: a `worker` task's immutable input is a `WorkerSpec` (a brief and an optional inherited seed), and its settlement plans the owned conversation, the brief as a transcript entry with a user projection, and the worker's initial generation in its own turn. The brief is an entry rather than an admitted input, so there is no admission receipt to replay and no input to consume; `DESIGN.md` §11 already allows a task with no assigned input to read its transcript. Reads are bounded: `TaskDriver::{conversation, owned_conversations}` look up one conversation record and a task's owned list without materializing the session.

Retirement is implemented as a read-only archive: `Conversation.retired` (schema version 2), `Session::retire_conversation` / `reactivate_conversation` and the same pair on the driver. Retirement requires an owned conversation with no foreground turn and no non-terminal task; input that was queued but never started is cancelled in the same commit. Every writer path rejects a retired conversation — transcript entries, task creation, input admission, opening a turn, and settlement plans — while reads, history, ownership, terminal outcomes and checkpoints are preserved, and inheriting a cutoff from a retired ancestor stays allowed. `tests/k6_retirement.rs` covers quiescence, the rejection paths including a rolled-back plan, cancellation of unstarted input, ownership, reactivation, reopen and the schema refusal.

A planned successor now names its turn: `PlannedTurn::{Inherit, Own, Background}` replaced the old `background` flag. `Own` opens the target conversation's foreground slot in the same commit as the successor that roots it, so the one-turn-per-conversation rule is validated on the plan path too; a plan that opens a slot already held is rejected and rolls back. The spawned worker's initial task uses `Own`, so a worker's run occupies its own conversation's turn: a follow-up admitted to a busy worker queues and drains into its own successor turn once the first chain finishes, `cancel_turn` on the worker's root stops exactly that run, and the creator's conversation is idle again as soon as the spawn settles. Successor cancellation inheritance reads the turn's durable barrier on the conversation holding the slot, so it also holds when the root has already settled. `Background` stays outside cancellation scope.

A turn's completion is durable: the turn root records the member whose settlement closed the turn (`turn_closed_by`), written with that settlement and the slot release, and `TaskDriver::{turn_closed_by, wait_turn}` expose it. `wait_turn` observes whole-chain closure, not necessarily an answer or success. It is deliberately distinct from waiting for a worker's initial generation, because a generation settles as soon as it has planned its children. `tests/k6_turn_completion.rs` covers the receipt naming the closing member, a later turn not reopening an earlier receipt, one wait not blocking another worker's chain, cancellation still recording a closure, the non-root refusal, and reopen.

Deferred until the repair gate and single-agent baseline in `ROADMAP.md` §1:

- **a dependency edge on a turn, plus the collector.** The receipt makes closure observable, not answer selection or success; a collector needs an explicit result contract and a *task* cannot yet depend on a turn. The edge must reject the aggregate cycle a backward-only reference check cannot see: a task depending on turn R while a member of R depends on that task.
- the command surface for send/follow-up, inspect and wait as first-class operations rather than composed primitives;
- interruption scoped to one worker run as a first-class command; the worker's own `cancel_turn` primitive already exists, subject to R2;
- reuse of a retained worker and nested ownership limits.

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