# Ion K0-K3 next-session handoff — 2026-09-12

## Purpose

Continue the clean `nijaru/ion` rewrite from the new production kernel. Do **not** restore or refactor the removed lane/agent/operation/effect runtime. The project is now on the new Session/Conversation/Entry/Input/Task architecture.

This handoff is intentionally implementation-specific enough that a new session should not need to reconstruct the last several hours of decisions from Git history.

## Revisions and validation

Validated **code checkpoint**:

```text
a33e0fb22073857cc724244b23f99e0c12249147
```

GitHub Actions CI run:

```text
34725190754
```

At that checkpoint all normal gates passed:

```text
cargo fmt --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

The final code change in that checkpoint fixes cancellation-vs-settlement ordering in the K3 task driver and adds the explicit “settlement commits first, therefore cancellation becomes a no-op” test.

Authority docs were updated after the code checkpoint. Therefore the final repository head after this handoff is documentation-only relative to `a33e0fb2`; when starting a new session, verify current `main` and its latest CI instead of assuming this exact documentation commit is still head.

Important earlier cutover commit:

```text
2d273f78
```

That physically removed the legacy production `ion-core` runtime/store/operation/tool tree and the historical R0/P1/P2 prototype harnesses. Git history is the archive. Do not bring those source paths back merely for compatibility.

## Read these first

In this order:

1. `DESIGN.md` — semantic authority.
2. `ROADMAP.md` — current K0-K6/P1/P2 status and exits.
3. `docs/core-runtime-migration.md` — implementation order and the K4 storage refactor direction.
4. `docs/source-layout.md` — module/crate ownership and K4 SQLite layout.
5. this handoff.

Useful historical evidence, not current production architecture:

- `docs/r0-kernel-gates-2026-09-12.md`
- `docs/p2-storage-topology-prototype.md`
- `docs/research/agent-topology-context-2026-09-12.md`
- old commits/prototypes referenced in `ROADMAP.md`

## Current workspace

The active workspace is intentionally small:

```text
crates/ion-ai
crates/ion-core
crates/ion-terminal
```

There is currently no active `crates/ion` application binary. That was intentional: the old application was coupled to the deleted runtime and was removed instead of bridged. Rebuild an application shell later from the new command/view API.

## Current architecture

The durable semantic model is:

```text
Session
  Conversation
    Entry
    Input
    Task
```

There is no durable Agent row, lane, Operation, or generic Effect.

Workers later become ordinary owned `Conversation`s. History inheritance and task ownership are independent edges.

Canonical history is append-only. Model context is derived from immutable entries plus constrained context controls and stable fork cutoffs.

A task is the durable recovery boundary. Async Rust futures are process-local execution only; durable continuation is the task input + complete checkpoint/output + generation/lifecycle state.

## K0 — `ion-ai`: complete

`ion-ai` is an independent provider-neutral model contract crate with a deterministic scripted service.

It deliberately contains no session/task/store IDs and no production auth/catalog/HTTP implementation yet.

Keep this dependency direction:

```text
ion-ai -> no ion-core dependency
ion-core -> may depend on ion-ai
```

Do not move durable retry/backoff/usage policy into provider SDKs. That belongs to future generation task logic.

## K1 — fresh domain: complete

Key identifiers share one session-local ordered physical namespace while remaining distinct Rust types:

```text
SessionId       # global UUIDv7
LocalSeq        # positive i64
ConversationId
EntryId
InputId
TaskId
ArtifactId
CommitSeq
```

Rejected transactions publish/consume no durable sequence values. Object IDs and the later batch `CommitSeq` are separate values even though both come from the same allocator.

Current durable nouns include:

- `Conversation { parent?, owner_task? }`
- immutable `Entry`
- admitted `Input` with explicit disposition
- `TaskRecord`
- `TaskOutput`
- `Artifact`

No legacy identity aliases are retained.

## K2 — session writer / transaction kernel: complete

### Main shape

Current K2 implementation lives primarily in:

```text
crates/ion-core/src/session/owner.rs
crates/ion-core/src/session/command.rs
crates/ion-core/src/session/transaction.rs
crates/ion-core/src/store/mod.rs
crates/ion-core/src/store/memory.rs
crates/ion-core/src/view/
```

`Session` currently owns the deterministic `MemoryStore` and bounded committed observations.

A transaction:

1. clones/builds against a private draft for read-your-writes;
2. allocates object IDs only in the draft;
3. validates/stages typed mutations against that draft;
4. allocates the commit sequence last;
5. commits the whole `MutationBatch` atomically;
6. publishes the committed observation only after commit succeeds.

Failed transactions do not publish IDs, commits, observations, or partial successor state.

### Implemented K2 semantics

- root/session creation;
- independent/forked/owned conversation creation;
- stable fork cutoff validation;
- reciprocal task -> owned-conversation link in the same transaction;
- immutable entry append and visible context-reference validation;
- exact request-key input admission replay/conflict;
- explicit input disposition transitions;
- task/dependency creation;
- execute/recover/abort reservation;
- monotonically increasing invocation generation;
- generation-fenced checkpoint/output replacement;
- durable cancellation mark;
- cancellation fencing of normal invocation writes;
- atomic terminal settlement plus successor/ownership writes;
- bounded observations with overflow -> resnapshot behavior.

### Important internal lifecycle rule

K2 lifecycle methods are crate-private session-writer operations. They were intentionally **not** made public merely to satisfy Clippy. K3 is now the real production caller.

That includes reservation, checkpoint, cancellation, input placement and terminal-plan paths.

## K3 — task driver: validated core slice, not complete

Current production K3 code is centered on:

```text
crates/ion-core/src/task/kind.rs
crates/ion-core/src/task/context.rs
crates/ion-core/src/task/registry.rs
crates/ion-core/src/session/scheduler.rs
crates/ion-core/tests/k3_driver.rs
```

### Task contract

`TaskKind` has exactly one lifecycle framework:

```rust
execute(...)
recover(...)
abort(...)
```

Each returns `TaskFuture<'a>` and receives a durable `RunningTask` snapshot plus invocation-scoped context.

`TaskContext` exposes checkpoint commits and process-local cancellation observation:

```text
checkpoint(...)
is_cancelled()
cancelled().await
```

`AbortContext` is deliberately narrower and provides checkpoint commits without the normal invocation cancellation token.

### Registry

`TaskRegistry` is keyed by:

```text
(TaskKindName, schema_version)
```

Duplicate registration is rejected. Missing implementation is preserved as durable task data and currently settles `Unsupported` when explicitly driven.

### Driver behavior

`TaskDriver` currently provides:

```text
new(session, registry)
snapshot()
drive_task(task_id)
cancel_task(task_id)
assign_input(input_id, task_id)
consume_input(input_id, entry_id)
```

`drive_task` is explicit. Merely constructing the driver or calling `snapshot()` starts no work.

Invocation selection:

```text
pending + not cancelled -> Execute
running + not cancelled -> Recover
nonterminal + cancelled -> Abort
terminal -> AlreadyTerminal
```

The active process keeps a local map of `TaskId -> CancellationToken` and rejects a second simultaneous local drive of the same task.

Task futures execute outside the session mutation lock. A checkpoint briefly reacquires the session writer through the internal `TaskRuntime` adapter.

Task futures are run in Tokio tasks so panics can be joined and converted into durable `Failed` outcomes instead of unwinding the driver.

Current outcome policy:

```text
TaskKind returns completion -> use it
TaskRunError              -> Failed { error }
panic/join panic          -> Failed { "task panicked" }
missing kind              -> Unsupported
```

### Cancellation semantics

`cancel_task` first commits the durable cancellation mark under the session writer, then signals the current local `CancellationToken` if one exists.

The old execute/recover invocation must join before a fresh higher-generation Abort invocation is reserved and run.

The cancellation-vs-normal-settlement decision is now serialized under the session lock. This was fixed immediately before the handoff because the earlier code had a TOCTOU window between checking `cancel_requested` and reacquiring the lock to settle.

Required ordering now is exact:

```text
normal settlement commits first
  -> task is terminal
  -> later cancellation is no-op

cancellation mark commits first
  -> normal settlement is not allowed to win
  -> fresh Abort invocation owns cleanup/terminalization
```

Do not regress this into a separate check + later settle.

## Fresh-core tests already present

Important current tests include:

### `tests/k1_domain.rs`

Domain/identity/context/fork invariants.

### `tests/k2_session.rs`

- unified sequence allocation;
- rejected fork consumes no IDs/commit;
- request-key exact replay/conflict;
- atomic owned conversation <-> task link;
- observation overflow/resnapshot.

### `session/tests.rs`

- execute/checkpoint/recover generation fencing;
- cancellation fencing + fresh abort generation;
- dependency readiness;
- explicit input disposition;
- failed terminal plan rollback.

### `tests/k3_driver.rs`

- execute + checkpoint outside writer lock;
- cancellation signal -> fresh abort invocation;
- settlement-first cancellation ordering;
- missing task kind -> Unsupported without data loss;
- task error -> Failed;
- task panic -> Failed;
- snapshot does not implicitly drive pending work;
- duplicate simultaneous local drive rejected.

Current validated code checkpoint `a33e0fb2` passes the whole workspace gate.

## K3 work still open

Do **not** call K3 complete yet.

### 1. First-class waits / dependency wakeups

Right now dependency readiness is enforced at reservation. A blocked drive gets a readiness error; there is not yet an addressable wait/subscription API that wakes when dependencies/tasks settle.

Need a design where waiting does **not** hold execution/model/tool capacity.

Likely shape should use committed session observations or narrow per-task notifications derived from committed state. Do not add a second scheduler or mutable task truth outside the session.

### 2. Capacity/resource scheduling

There is no explicit execution-capacity model yet. Add capacity only where it represents real scarce work (model/tool/process/etc.), and ensure waiting tasks release/not acquire those permits.

Avoid one global semaphore if later task classes clearly need separate resource domains.

### 3. Host close / fault / join behavior

There is no final policy/API yet for closing a driver/session with live local invocations.

Need deterministic behavior for:

- graceful close;
- forced/fault close;
- task future panic already handled locally;
- session/store fault while task is live;
- what gets signalled, joined, left durably running for recovery, or terminalized.

Do not silently turn host disappearance into successful cancellation.

### 4. Persistence-backed recovery

The driver already selects `Recover` for a durable running task, but honest reopen/process-death coverage requires K4 SQLite persistence.

Do not claim crash recovery in the fresh core until actual process-death tests reopen a persisted session and explicitly drive recovery.

## Recommended next-session order

### Step 0 — verify before editing

Read the authority docs listed above, fetch current `main`, and verify latest CI. Do not assume the handoff's documentation head is still current.

### Step 1 — close the storage-independent K3 gaps that are cheap now

Prefer a focused pass for:

1. task/dependency wait subscription semantics;
2. capacity-safe waits / basic resource admission;
3. close/fault/live-invocation policy.

Keep these small. If a clean design clearly depends on persistent reopen/ownership state, document the dependency and defer that portion until K4 rather than inventing in-memory-only semantics.

### Step 2 — begin K4 with a resident-state/persistence split

This is the most important K4 prerequisite.

Current K2 `MemoryStore` owns both `SessionState` and commit application. That was intentionally simple for the deterministic proof, but SQLite should not become the semantic state owner queried on every command.

Preferred direction:

```text
Session owner
  resident SessionState
  transaction builder
  observations
  persistence store

commit path
  validate/build MutationBatch against resident state
  -> persistence_store.commit(batch) DURABLY
  -> apply batch to resident state/indexes
  -> publish committed observation
```

The private Rust shape can be a narrow trait, enum, or simpler concrete abstraction. Do **not** create a public generic database framework just to support Memory + SQLite.

First refactor the current memory path to this shape while keeping all existing K1-K3 tests green. That gives SQLite a stable contract.

### Step 3 — fresh per-session SQLite store

Target physical topology remains:

```text
Ion data root/
  sessions/<SessionId>/
    session.sqlite
    artifacts/

  catalog.sqlite   # optional/rebuildable only
```

Implement a fresh schema. Do not port the old agent/lane/operation/effect tables.

Suggested K4 module split from `docs/source-layout.md`:

```text
store/sqlite/
  mod.rs
  connection.rs
  schema.rs
  commit.rs
  conversation.rs
  entry.rs
  input.rs
  task.rs
  artifact.rs
```

SQLite connection/open policy should establish the selected durability pragmas and one-writable-owner behavior. Historical prototype evidence favored WAL + `synchronous=FULL` for accepted-intent durability, but revalidate the exact fresh implementation rather than copy old SQL blindly.

### Step 4 — open/reopen semantics

Opening/inspection must do no work.

On reopen:

- reconstruct the resident state/indexes needed by the kernel;
- leave running tasks durable and inert;
- explicit `drive_task` chooses Recover;
- cancelled nonterminal tasks choose Abort;
- missing task implementation preserves data and follows the accepted unsupported/orphan policy.

### Step 5 — real crash/recovery tests

Use actual child-process death, not cooperative drop/close.

Fresh-core tests eventually need:

- process dies after invocation reservation/checkpoint;
- reopen shows running task without auto-driving;
- explicit drive invokes Recover;
- retry-safe external phase can retry;
- reconcile/adopt phase can recover from durable external identity;
- no-safe-retry phase becomes Indeterminate;
- second writable owner rejected, including abnormal predecessor exit;
- committed cancellation vs settlement ordering remains exact across process loss.

Task-kind-specific external recovery policies should live in typed checkpoints/task logic, not a reintroduced generic Effect table.

## K4 schema guidance

Do not prematurely normalize every JSON payload into dozens of tables. Preserve semantic query needs:

- point lookup by typed ID;
- conversation visible-entry range/fork traversal;
- input request-key lookup;
- task status/dependency/readiness/recovery lookup;
- owned-conversation lookup;
- commit/sequence metadata;
- bounded/lazy history rather than decoding every old task on open.

Use integer local IDs/commit cursors. Keep `SessionId` at the database/session boundary rather than redundantly stuffing it into every row unless a concrete query requires it.

Keep schema/version metadata explicit and refuse unsupported newer schemas. Pre-1.0 old databases may be archived/refused instead of migrated.

## P1 status at this checkpoint

Fresh-core coverage already exists for parts of P1, but P1 is not closed.

Covered now:

- exact request-key replay/conflict;
- generation fencing;
- explicit input disposition;
- atomic terminal successor writes/rollback;
- dependency reservation readiness;
- cancellation + late normal-write fencing;
- both high-level cancel/settle orderings;
- missing kind without data loss;
- panic/error settlement;
- no automatic work on inspection;
- duplicate local drive rejection.

Still requires future fresh-core evidence:

- out-of-order tool completion + call-order projection (K5);
- retained worker lifetime/races (K6);
- capacity-safe waits;
- real process-loss recovery classes;
- persisted reopen with no auto-drive;
- caller disappearance before/after admission;
- cross-process writable owner exclusion;
- self-wait/dependency cycle rejection beyond duplicate dependency checks;
- host-close/fault join policy;
- worker creation/message/cancellation races.

Historical P1/R0 prototypes are scenario/evidence inventories only. Do not count them as current production closure.

## Things not to do next

- Do not reintroduce `AgentId`, lane, Operation, generic Effect or an old-runtime compatibility module.
- Do not build a second task state machine beside `TaskKind` execute/recover/abort.
- Do not make async stack frames durable continuation.
- Do not make SQLite callbacks or provider callbacks mutate canonical state directly.
- Do not start tasks merely because a session was opened or inspected.
- Do not make request-key replays allocate a new receipt/commit.
- Do not allow cancellation to race outside the writer with terminal settlement.
- Do not make the store API public/pluggable just because Memory and SQLite both exist.
- Do not port the full provider/auth/TUI/extension stack before K4/K5 establish the new kernel end to end.
- Do not claim SOTA/optimality from these kernel tests; they validate invariants, not comparative product performance.

## Useful commands / validation posture

Normal required gates:

```bash
cargo fmt --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

If dependencies change, update `Cargo.lock` intentionally with the pinned Rust toolchain, then return to the normal `--locked` gates. Do not loosen CI.

Keep commits small enough that CI failures identify one conceptual change. Strict Clippy has been useful for exposing unreachable/test-only architectural paths, so prefer giving code its real production caller over suppressing dead-code warnings.

## Good stopping point reached

The session intentionally stops here because:

- the legacy runtime is gone;
- K0 and K1 are real production code;
- K2 has the complete in-memory transaction/lifecycle boundary needed by persistence;
- K3 has a real async driver and production callers for K2 lifecycle paths;
- the cancel/settle race discovered during the final audit is fixed and covered;
- all normal gates are green at the recorded code checkpoint;
- authority docs are aligned;
- K4 can begin from a clean, bounded persistence contract instead of another runtime redesign.
