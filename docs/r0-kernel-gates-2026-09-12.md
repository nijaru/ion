# R0 kernel gates

Status: accepted pre-rewrite evidence, 2026-09-12.

This document records the decisions from the five clean-rewrite gates in `docs/core-runtime-migration.md`. The prototypes are disposable evidence, not production APIs.

Validated head: `81c344d713f13b73e19232b4ad36dfe40ba663b9`

GitHub Actions CI run: `34718467220`

Validated with Rust 1.98.0:

```text
cargo fmt --check                                                   PASS
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings  PASS
cargo test --locked --workspace                                     PASS
```

The full workspace test run includes actual child-process termination for task-checkpoint and external-action recovery. No live provider, human TUI, or production-core validation is implied by these gates.

## R0.1 — accepted task contract

Production target:

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

The async Rust stack is process-local and never durable continuation state. Durable continuation is the complete typed checkpoint plus canonical task/session state.

A running invocation receives `TaskContext`. Its commit path builds a bounded mutation and revalidates task identity, invocation generation and cancellation authority before canonical writes become durable.

Cancellation uses two different mechanisms deliberately:

1. a durable cancellation mark revokes the old normal invocation's canonical write/settlement authority;
2. a process-local cancellation signal asks the old async invocation to return promptly;
3. after the old invocation joins, a fresh higher-generation abort invocation owns cleanup and abort settlement.

Caller/waiter cancellation is not durable task cancellation.

An optional typed checkpoint/phase helper is allowed for exhaustive task authoring. It compiles to the same task trait and creates no second scheduler or lifecycle.

Evidence: `crates/ion-core/tests/r0_task_contract.rs` and `tests/r0_support/{task.rs,store.rs}` cover typed registry erasure, durable checkpointing, stale-generation fencing, both cancellation/settlement orderings, fresh abort ownership, waiter cancellation, panic recovery and abrupt subprocess death.

## R0.2 — accepted immutable transcript/context model

Canonical conversation history is append-only. There is no separately mutable canonical context vector.

An immutable entry may contain:

```text
semantic kind/data
provider-neutral model projection
optional context head
constrained omit/replace edits
```

Effective model context is derived from fork-visible entries at a cutoff, the newest visible head, constrained edits and provider-safe projection normalization.

Accepted rules:

- summary compaction appends a new head plus summary projection;
- handoff/reset are new heads, not history mutation;
- newest visible edit wins without rewriting its target;
- a fork records source conversation + immutable cutoff and never sees later source appends;
- initial inherited worker/fork cutoffs and context heads must land on complete tool-exchange boundaries;
- transcript chronology may record tool B completing before A while model projection restores source call order A then B;
- source task state is not history inheritance.

Evidence: `crates/ion-core/tests/r0_context.rs` and `tests/r0_support/context.rs`.

Cold/warm/index scaling remains P2 work; R0 establishes semantics, not a complexity claim.

## R0.3 — generic Effect rejected

The prototype did not expose an independent generic effect identity/lifetime that justified preserving `EffectId`.

Fresh core therefore uses:

```text
Task
  + complete typed checkpoint
  + invocation generation
  + task-scoped attempt/usage evidence
  + external reconciliation/idempotency identity where one exists
```

A repeat-sensitive task checkpoint can distinguish prepared/not-dispatched, dispatched retry-safe, dispatched reconcile/adopt and dispatched no-safe-retry states.

Recovery classes:

- retry-safe: record the uncertain prior attempt and execute another attempt;
- reconcile/adopt: use durable external identity/evidence and adopt without repeating;
- no-safe-retry: settle/park indeterminate rather than guessing or duplicating the action.

Evidence: `crates/ion-core/tests/r0_external_recovery.rs` uses actual subprocess death after durable dispatch checkpoint plus external witness, then verifies retry, adopt and indeterminate outcomes without a generic Effect table/entity.

If a future concrete operation proves a truly independent identity/lifetime, this decision can be reopened. Do not add generic effects preemptively.

## R0.4 — accepted session-local sequence representation

Use one private monotonic local sequence per session store.

Every durable local object and every committed mutation batch obtains a value from that allocator. Rust still exposes distinct semantic wrappers such as:

```text
ConversationId(LocalSeq)
EntryId(LocalSeq)
InputId(LocalSeq)
TaskId(LocalSeq)
ArtifactId(LocalSeq)
CommitSeq(LocalSeq)
```

The wrappers are not interchangeable even though their stored values share one ordered namespace.

Rules:

- `SessionId` remains globally unique;
- local IDs are meaningful only with their session;
- a batch may reserve successive local sequence values for several cross-referencing objects and then a later value for its commit cursor;
- mutation-only commits still advance the local sequence for their `CommitSeq`;
- a rejected transaction publishes and consumes no durable sequence values;
- IDs never escape before the creating transaction is durable;
- historical/fork cutoff order can use ordered entry IDs without a second object-order clock;
- cross-session references, when required, are `(SessionId, typed local ID)`.

Evidence: `crates/ion-core/tests/r0_ids.rs` compares this representation with separate object/commit counters, verifies same-batch cross references, rollback/no visible ID consumption and ordered entry cutoffs, then runs the same SQLite workload. The unified representation passed the prototype's compact-footprint bound (no more than one SQLite page above the separate-counter form). This is structural evidence, not a performance benchmark.

## R0.5 — accepted independent model-service boundary

Create a small provider-neutral `ion-ai` crate rather than putting provider/model contracts in `ion-core` or the application binary.

Initial contract contains only the surface required by generation:

```text
ModelRef
Message / Content
ToolSpec
ModelRequest
ModelStreamEvent
ModelResponse
Usage
ProviderError / ProviderErrorKind
ModelService::stream
scripted model service
```

The service receives no `SessionId`, `ConversationId`, `TaskId`, SQLite/store command, credential-store handle or retry policy.

Provider-specific opaque replay metadata may survive on otherwise provider-neutral assistant content. Typed provider failures cross the boundary as facts; generation-task logic owns durable retry/backoff/compaction/usage policy. Hidden provider/SDK retries must be disabled or controlled when production adapters are added.

Evidence: `crates/ion-core/tests/r0_model_service.rs` proves an object-safe async scripted stream with text/tool/usage/final events, opaque replay metadata and typed pre-stream rate-limit failure with no hidden retry.

HTTP APIs, OAuth, credential persistence, provider catalogs and dynamic model discovery remain the later AI/provider component pass.

## Rewrite decision

R0.1–R0.5 have no unresolved dependency on the old lane/Agent/Operation/Effect runtime. The clean rewrite boundary is therefore open.

Next production work is K1 onward in `docs/core-runtime-migration.md` and `docs/source-layout.md`:

1. establish the independent `ion-ai` contract crate;
2. replace `ion-core` directly around the target domain and accepted task/context/sequence contracts;
3. build the single session writer against an in-memory store before SQLite;
4. add the task driver, fresh per-session SQLite store, scripted generation/tool chain and owned-conversation workers;
5. remove old production runtime structures instead of retaining a compatibility path.

The R0 prototypes should remain only until the equivalent production invariants are covered by the fresh core, then be removed with the rest of the temporary evidence harness.