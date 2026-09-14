# Rust and crate-surface review — 2026-09-14

**Keep the kernel architecture and dynamic boxed-future boundary; do not freeze the current public authoring/provider surface.** The most immediate defects are uncancellable provider opening and cloneable plan identities that can silently bind the wrong dependency. The in-place journal is worth retaining, but duplicate insertion is not failure-atomic and the derived-index rollback tests are insufficient.

This is a review, not an accepted redesign or a repair-completion record. Recommendations replace wrong v0 surfaces directly; they do not call for compatibility wrappers, another runtime, or another storage backend.

## Scope and evidence

Scope: all source and tests in `crates/ion-ai` and `crates/ion-core`, their manifests, workspace/toolchain declarations, and the core storage-measurement example. The legacy application and `ion-terminal` are excluded. Architecture/status references are `AGENTS.md`, `DESIGN.md`, `docs/source-layout.md`, `docs/decisions.md` and `ROADMAP.md`. The concurrently added [provider-request design](../design/provider-request-assembly.md) is a **proposal**, not evidence of implemented behavior.

Source baseline: `87225783`; rechecked through `01cff5cd`. Scoped Rust, tests, manifests and lockfile did not change between these commits. The intervening instruction/decision changes were reconciled, including D13's resolved-instruction requirement and the rule to delete unused/wrong surfaces. Paths and line numbers below refer to this unchanged Rust baseline.

Evidence labels:

- **Reproduced:** an external path-dependent probe, an isolated source-copy experiment, or the checked-in measurement harness was executed. No production source/test changes were made for this review.
- **Source-derived:** the cited implementation establishes the finding; a proposed regression has not been added or run.
- **Measured:** timings below describe this host/workload, not allocation profiles, live model performance, or a general complexity proof.

`cargo test --locked -p ion-ai -p ion-core` passed, including the existing subprocess tests. Pinned compiler: `rustc 1.98.0 (88d9e12ae 2026-08-18)`, edition 2024. Full-workspace fmt/clippy/test gates, terminal checks, live-provider checks, allocator profiling and sanitizer/Miri checks were not run. No Rust repair is claimed validated.

## Findings, in severity order

### 1. High — cancellation does not own the provider-opening future

**Evidence:** `crates/ion-core/src/builtin/generation.rs:125-139` awaits `ModelService::stream` before entering the cancellation select. Only collection is cancellable. The open future can perform DNS, connection setup, headers or credential resolution under `crates/ion-ai/src/service.rs:12-17`. The durable cancellation/fresh-abort machinery cannot finish until this invocation yields ownership.

**Reproduced:** a service signalled entry into `stream`, installed a drop witness and then awaited forever. After `cancel_task`, a 100 ms timeout expired: the task remained `Running`, generation 1, with durable cancellation true, and the opening future had not dropped. Fault close dropped it. This is not evidence that ordinary streamed cancellation or fault close is broken; it isolates the pre-stream gap. Graceful close is cooperative and has the same handler-cooperation limitation.

**Recommendation / owner:** `builtin/generation.rs` must select cancellation against the entire owned open-and-collect operation, check cancellation before dispatch, and preserve dispatch uncertainty when dropping it. The provider contract must forbid detached request producers and state what dropping both futures and streams ends locally. A timeout does not prove the provider did not receive the request. Add deadline ownership at the same boundary when R7 introduces limits; do not make the session scheduler understand HTTP.

**Acceptance:** barrier-controlled open future → committed cancellation → observed drop → old invocation joined → fresh abort generation → no assistant entry/tool child. Repeat for stream collection and cancellation immediately before opening; check both cancellation/settlement writer orders. `crates/ion-core/tests/k5_generation.rs:576` exercises tool-chain cancellation, not this open phase.

**Decision implications:** preserve D4 and D5; enforce `DESIGN.md` §13 and §15. This is a local adapter repair, not grounds to reopen R0. The proposed provider design §6 recognizes this gap but does not close it.

### 2. High — `TaskPlan: Clone` defeats plan-local handle identity

**Evidence:** `crates/ion-core/src/task/plan.rs:27-33` derives `Clone`, copying the plan ID along with independently mutable vectors. IDs are minted only by `new` (`:43-49`); new handles combine that copied ID and a vector index (`:61-88`). Writer validation checks plan ID and index (`crates/ion-core/src/session/transaction.rs:347-369,377-412`), not the originating branch of a cloned builder.

**Reproduced, using only public APIs:** create empty plan A; clone it into B; add task “intended” to A, retaining its index-0 handle; add task “different” to B; add a dependent to B using A's handle. Settle B. Settlement succeeds and the dependent points at “different”. Neither malicious IDs nor storage damage is required. Conversation handles have the same structural weakness. The unrelated-plan test at `crates/ion-core/tests/k3_plan.rs:318` does not cover diverged clones.

**Recommendation / owner:** make the mutable plan builder non-`Clone`, and remove transitive convenience `Clone` derives such as `TaskCompletion` where necessary (`crates/ion-core/src/task/kind.rs:46-52`). Move a plan into completion. Do not implement a fresh-ID clone without also remapping every internal handle; there is no demonstrated need for that complexity. Immutable diagnostic snapshots can be a different read-only representation if a real consumer needs one.

**Acceptance:** an external compile-fail test that a plan cannot be cloned, plus existing foreign-plan rejection and atomic-settlement tests. If cloning is deliberately retained instead, both task and conversation versions of the diverged-clone witness must reject before any write.

**Decision implications:** D4 and `DESIGN.md` §8 promise one atomic plan with scoped handles. Preserve that primitive, remove the derive that weakens it. D16 should block freezing this builder as-is.

### 3. Medium — duplicate insertion can mutate resident state on `Err`; reconstruction permits the triggering sequence inconsistency

**Evidence:** `SessionState::insert_input`, `insert_task` and `insert_entry` use `map.insert(...).is_some()` to detect duplicates **after replacing the existing value** (`crates/ion-core/src/session/state.rs:131-159,185-199`). Their journal wrappers append undo records only after those fallible calls return successfully (`crates/ion-core/src/session/journal.rs:114-131,152-164`). The duplicate error therefore has no inverse. `put_conversation` already avoids this (`:92-102`). Reconstruction accepts independently parsed metadata and records without checking sequence order or their complete cross-record consistency (`crates/ion-core/src/store/sqlite/mod.rs:133-174`).

**Reproduced with an explicit corruption precondition:** create entry ID 3 and close; execute `UPDATE session_meta SET last_seq=2 WHERE id=1`; reopen. Open succeeds. A public append allocates ID 3 and returns a duplicate error, but the resident original entry has been replaced. The commit cursor and observation tail do not advance. Closing/reopening restores the original durable entry. This is not a claim that healthy SQLite commits generate such metadata or that a normal user can supply an append ID.

**Recommendation / owner:** use `BTreeMap::entry` with an occupied rejection before replacement, keeping record/index insertion failure-atomic. During SQLite reconstruction, reject inconsistent sequence/commit metadata, missing root/reference records and impossible lifecycle combinations before returning a writable owner. Do not repair by guessing a new sequence or rewriting evidence. Keep validation in the existing record/reconstruction owners, not a second runtime.

**Acceptance:** direct duplicate insert tests assert exact records **and indexes** unchanged for entries, tasks and inputs. Add the lowered-sequence database fixture and require open to fail without writes. Add representative malformed running/invocation and ownership-reference fixtures; serde successfully parsing individual fields is not whole-session validation.

**Decision implications:** this meets D9's reliability-review trigger under malformed state; fix the journal's failure atomicity rather than reverting its performance work. D1/D6 and `DESIGN.md` §5–6, §8 and §17 require fail-closed reconstruction. No stronger corruption-resilience guarantee should be claimed before these tests exist.

### 4. Medium — all derived-index rollback handlers can be disabled without failing the scoped suites

**Evidence:** `crates/ion-core/src/session/journal.rs:237-262` owns inverses for entry membership, queued inputs, turn membership and reverse dependencies. The injected persistence-failure scenario creates a conversation, not these index mutations (`crates/ion-core/src/session/persistence_tests.rs:84-115`). Successful replay coverage at `:215` and entry-index reopen coverage at `crates/ion-core/tests/k4_sqlite.rs:708` do not establish rollback correctness. Public `SessionSnapshot` equality omits private indexes (`crates/ion-core/src/session/owner.rs:485-518`).

**Reproduced:** in an isolated `git archive` copy, replace the four inverse branches with:

```rust
Undo::EntryIndex(_, _) | Undo::Queued(_, _) |
Undo::TaskTurn(_, _) | Undo::Dependent(_, _) => {}
```

All `ion-ai`/`ion-core` tests still passed. Removing the excluded terminal member required regenerating the copied workspace lockfile offline; the initial `--locked` invocation refused the changed workspace, and the successful run used `cargo test --offline -p ion-ai -p ion-core`. Production files/lockfile were untouched. This demonstrates missing sensitivity to these inverses, **not** that every current inverse is incorrect.

**Recommendation / owner:** add a journal/session regression matrix that stages each indexed mutation and then rejects a later mutation; repeat with injected persistence failure after successful preparation. Compare `SessionState` including indexes, sequence and receipt maps. Exercise public behavior after non-faulting rejection: page history, select queued input, cancel a turn and discover ready dependents. Assert no observations/IDs escaped. For persistence failure, retain the fence assertion and compare the resident rollback state before reopen.

**Acceptance:** deleting any one inverse must fail a named test. Rebuild indexes from records in test assertions and compare; do not add a second production index authority.

**Decision implications:** D9/D10 remain good implementation choices, but their rejection guarantee is not adequately protected. `DESIGN.md` §5, §6 and §8 and the D8 repair order call for this before further indexed scheduling expansion.

### 5. Medium — typed authoring cannot use the ordinary invocation-read or late-registration capabilities

**Evidence:** raw `TaskContext` offers transcript pages, dependency outcomes and placed inputs (`crates/ion-core/src/task/context.rs:88-120`). `TypedContext` has a private inner context and forwards only checkpoint/cancellation (`crates/ion-core/src/task/typed.rs:134-168`). An external typed continuation cannot read what it joined; a typed generation cannot assemble its transcript. Late registration on `TaskDriver` accepts only `Arc<dyn TaskKind>` (`crates/ion-core/src/session/scheduler.rs:135-147`), while typed erasure is crate-private (`crates/ion-core/src/task/typed.rs:333-337`). Pre-opening `TaskRegistry::register_typed` is not the same live recovery surface.

**Recommendation / owner:** forward the same scoped reads on `TypedContext`; keep checkpoint typing and restricted abort authority. Add typed registration on the driver through the existing erasure function, not a public mutable registry escape hatch. Prefer one typed authoring facade over requiring authors to abandon it for raw JSON. Do not expose `Session`, SQL, dynamic waits or unrelated-conversation reads to solve this.

**Acceptance:** an external integration-test crate implements a typed continuation that reads ordered dependency results, its transcript and its placed inputs, and cannot access unrelated conversation state. Reopen a running typed task with its implementation missing, register that implementation through the public driver, explicitly recover and verify the original checkpoint. Existing typed tests cover decoding/outcome/plan behavior, not capability parity (`crates/ion-core/tests/k3_typed.rs:125-198`).

**Decision implications:** close the mismatch with D4 and `DESIGN.md` §8's “one authoring contract”; D16 should not freeze two practically unequal authoring paths. This does not require another task framework.

### 6. Medium — typed provider errors become strings before policy can consume them

**Evidence:** `ProviderError` carries a typed kind (`crates/ion-ai/src/error.rs:4-27`), but both open and stream errors are formatted into `TaskRunError::Interrupted(String)` (`crates/ion-core/src/builtin/generation.rs:129-137`; `crates/ion-core/src/task/kind.rs:111-123`). The frozen checkpoint contains a request, cutoff, input IDs and attempt counter, not failures/usage (`generation.rs:232-245`). The driver surfaces a string interruption and correctly leaves the task recoverable (`crates/ion-core/src/session/scheduler.rs:599-605,664-672`). It cannot recover the original enum by pattern matching.

This is narrower than “errors are untyped everywhere”: cancellation/stale/closed remain distinct `TaskContextError` variants, terminal outcome classes are explicit, and ownership conflict is preserved as `SessionInUse`. But `DESIGN.md` §13's statement that provider failures cross as facts “on the invocation” is stronger than the actual public drive result. The missing durable attempt ledger is already openly recorded; loss of the in-memory enum is an additional fact.

**Recommendation / owner:** preserve bounded serializable provider failure facts in generation attempt evidence and policy decisions. Keep process-local source chains separate from durable facts; do not force a transport error with a boxed source to implement `Clone`, `Eq` or serde. Extend facts only as the wire boundary requires—retry hints, dispatch knowledge, request IDs and usage—not through message-string parsing. Known authentication/invalid-request failures and unknown remote completion must not acquire the same retry policy merely because both interrupted a future.

For core diagnostics, preserve private SQLite/I/O sources through `StoreError` instead of flattening them immediately (`crates/ion-core/src/store/sqlite/mod.rs:40-44`; `crates/ion-core/src/store/mod.rs:25-54`). Public operation-level error categories can remain storage-neutral. All uncertain commit errors must still fence; distinguishing Busy from corruption is diagnostic information, not permission to keep writing. Do not introduce `anyhow` into public library contracts or serialize source chains.

**Acceptance:** scripted Authentication, RateLimited-with-hint and midstream Transport cases retain their distinct facts through the actual driver and reopen. Include usage-before-error and usage-before-cancel. Assert retry policy from enum facts, redaction of secrets, and uncertainty preservation. An error Display/serde test alone misses the lossy adapter.

**Decision implications:** D5/D6/D13 and `DESIGN.md` §8–9/§13. Preserve interruption versus terminal failure; replace the lossy provider bridge and revise the §13 implementation claim when authorized. No change is made to that document here.

### 7. Medium — the provider DTOs and transcript-read signature do not yet encode the promised request boundary

**Evidence:** `ModelRequest` contains only model/messages/tools (`crates/ion-ai/src/request.rs:5-10`); instructions, generation controls, resolved project projection and implementation identity have no explicit frozen contract. Replay metadata is one message-level JSON value, considered compatible by provider-name equality alone (`crates/ion-ai/src/message.rs:14-32,46-48`). `collect_response` accepts the first final response without validating role, call identity or agreement with earlier events; intermediate usage replaces the previous snapshot (`crates/ion-core/src/builtin/generation.rs:269-299`). Typed enums do not by themselves validate legal combinations of their public fields.

The task read API has an exclusive `after`, but no upper cutoff (`crates/ion-core/src/task/context.rs:93-101`). Generation reads pages and only then chooses the final entry as its cutoff (`crates/ion-core/src/builtin/generation.rs:59-61,251-262`). That cannot expose `DESIGN.md` §7's “choose cutoff, then project” operation as an atomic request basis. In an append-only history it need not fabricate a mixed snapshot, but concurrent writers can enlarge or prolong the first read, and the advertised optional-cutoff capability is absent. Recovery currently reuses the stored complete request; this finding does not say recovery rebuilds intact checkpoints.

**Recommendation / owner:** finish the R7 request-assembly design before freezing traits/serde shapes. Capture a typed request basis atomically before paging, with an explicit empty cutoff distinct from “unbounded”; resolve/freeze instructions, selected tools, controls and revisions. Validate role/content/tool-call/termination/replay invariants at provider preparation and final-response acceptance. Define stream ordering, terminal behavior and cumulative usage semantics. Retain ordered neutral content and explicit incomplete results; do not add session IDs or a provider-options JSON bag to `ion-ai`.

Use versioned serde records and golden fixtures for the new checkpoint/request shapes. Constructors validate semantic constraints that serde derives cannot: finite controls, complete calls, replay ownership and revision. Replace the old v0 shape rather than support dual formats without a preservation requirement. Exact wire-byte preservation cannot be inferred from storing a parsed `Value`; determine which bytes/fields must survive via fixtures.

**Acceptance:** capture basis → barrier → append/head/edit → finish paging; later contributions must be excluded and an empty cut must stay empty. Recover after configuration/catalog changes and require identical frozen semantics or fail before dispatch. Feed wrong-role finals, duplicate/partial calls, conflicting final blocks and partial usage snapshots through the real generation path; none may silently authorize tools or erase known usage.

**Decision implications:** D5/D13/D16 and `DESIGN.md` §7–10/§13. The new provider design proposes many of these changes; it is not implemented acceptance. The proposal at `01cff5cd` now follows D15: chat completions plus **Anthropic messages**. Its earlier Responses selection was superseded during this review; no adapter implementation or neutrality validation follows from that documentation correction.

### 8. Medium — the advertised observation companion has a lost-wakeup gap

**Evidence:** `TaskDriver::changed` subscribes only when its async body is polled (`crates/ion-core/src/session/scheduler.rs:258-267`). Reading `observations_after(cursor)` and then awaiting `changed()` leaves a gap. Constructing an unpolled future first does not subscribe. Existing tests intentionally require waiting only for a later commit, and use yield/sleep scheduling (`crates/ion-core/tests/k4_reads.rs:232-269`); they do not establish a safe read/wait loop.

**Reproduced:** read an empty delta at cursor C; commit a turn; await `changed` under a timeout. It waits despite `observations_after(C)` now containing events. This matches the method's narrow next-commit semantics, but contradicts its usefulness as the advertised polling-free companion. `wait_task`/`wait_turn` already subscribe before checking state and are not implicated (`crates/ion-core/src/session/wait.rs:25-47,58-77`).

**Recommendation / owner:** replace the companion with a cursor-aware wait that subscribes before checking whether committed state/coverage differs from the supplied cursor, or expose a subscription object created before the initial read. Keep notifications coalescible and observations authoritative. Return a typed closed result and retain reset-required semantics; do not add a durable event bus.

**Acceptance:** deterministic barriers place commits before subscription, between read/wait and while asleep; all yield a delta or resnapshot without another commit. Also close with no new commit. Avoid using sleeps/yields as proof of subscription order.

**Decision implications:** D1 and `DESIGN.md` §15–16; repair this client seam before the R8 streaming attachment boundary. No terminal redesign is needed to establish it.

### 9. Medium — “bounded” currently means counts, not safe byte/allocation limits

**Evidence:** `visible_page` uses `limit + 1` (`crates/ion-core/src/session/state.rs:283,287,353`), reached through public `Session::conversation_entries` (`crates/ion-core/src/session/owner.rs:456-482`). **Reproduced:** `usize::MAX` panics in debug. Normal unchecked release arithmetic would wrap these additions; that release behavior was source-derived, not separately probed. A zero-page special case is not an upper-bound policy.

Plan limits count entries/tasks/conversations (`crates/ion-core/src/task/plan.rs:9-14`; `crates/ion-core/src/session/transaction.rs:262-273`), but the builder allocates without those checks and one accepted `Value` can be huge. Checkpoint/output values and stream text/call vectors are similarly unbounded (`crates/ion-core/src/task/output.rs:6-10`; `crates/ion-core/src/builtin/generation.rs:270-280`). A page of one giant entry is not byte-bounded; a full `SessionSnapshot` deliberately clones all payloads.

**Recommendation / owner:** reject or cap page limits before lookahead arithmetic, with an explicit maximum and typed invalid-limit behavior; `saturating_add` alone removes overflow but not unbounded allocation. Define byte limits at stream decoding, request assembly, task checkpoint/output and transaction admission. Count limits remain useful as separate constraints. Bound producer growth and work per poll so a perpetually ready stream cannot starve cancellation; state clearly that a caller-supplied already-allocated `Value` cannot retroactively be memory-bounded by the receiver. Delete the unimplemented artifact promise until its publication/read boundary exists (see crate-surface notes), rather than claim it supplies spilling today.

**Acceptance:** limits 0/1/max/`usize::MAX` on root and inherited pages; oversized single entry/checkpoint/tool result; stream floods; oversized finalization plan after earlier staged writes. Assert explicit errors, unchanged state/sequence and no dispatch, plus representative peak-memory measurements for accepted maximum sizes.

**Decision implications:** D8/D13 and `DESIGN.md` §5/§8/§16–17. These are R6/R7/R8 boundary gaps, not evidence that the existing count limits or indexes should be deleted.

### 10. Medium — bounded commit work is improved, but full residency and payload cloning remain on important paths

**Evidence:** the journal copies touched records, not entire maps (`crates/ion-core/src/session/journal.rs:144-172`); the pointer regression protects unrelated task allocations (`crates/ion-core/src/session/persistence_tests.rs:45-81`). However:

- SQLite open materializes every record (`crates/ion-core/src/store/sqlite/mod.rs:133-174`). `Session::summary` still scans task records for counts (`crates/ion-core/src/session/owner.rs:424-448`); a small returned DTO is not bounded query work or residency.
- Inherited paging materializes ancestor-visible IDs before applying the fork cutoff (`crates/ion-core/src/session/state.rs:307-342`), so source entries appended beyond a fixed fork can still increase query work. The measured root-page scaling below does not establish page-plus-depth cost for forks.
- Task runtime paging clones an entire task merely to obtain its conversation ID (`crates/ion-core/src/session/scheduler.rs:697-711`; `crates/ion-core/src/session/owner.rs:613-615`). A checkpoint/output can be large even when the requested field is an ID.
- Typed decoding clones a `Value` before deserializing (`crates/ion-core/src/task/typed.rs:244-261,288-296`); generation clones its frozen request for dispatch and final message for projection (`crates/ion-core/src/builtin/generation.rs:129-132,181`). Its full-request checkpoint retains historical payloads in successive generation tasks (`:232-248`). Quadratic aggregate retained-request bytes over an uncompacted growing conversation are an inference from repeated history copies, not a measured result of the tiny-task harness.
- `Editor::task_mut` clones the full touched task even when replacing just a checkpoint/status. Global queued-input membership still filters by conversation (`crates/ion-core/src/session/state.rs:626-633`), so it is bounded by queued inputs across the session, not by that conversation's queue.
- Synchronous `rusqlite` commit runs while the async session mutex is held (`crates/ion-core/src/session/owner.rs:783`; `crates/ion-core/src/session/scheduler.rs:688-694`), with a 5-second SQLite busy timeout (`crates/ion-core/src/store/sqlite/connection.rs:24-26`). No handler/network await occurs under that mutex, which is good; the async facade nevertheless does not make SQLite/serialization nonblocking. Executor starvation under a forced busy writer was not measured.

**Recommendation / owner:** preserve D9/D10. First replace payload-cloning metadata reads with borrowed/private metadata access; move owned final values and decode from owned or borrowed JSON where the ownership path permits. Avoid speculative `Arc`/arena/interning across every DTO. Resolve the frozen representation through R7 before storing another context copy/cache. Finish typed indexed reads/retention in the existing session/store owners. Measure storage/serialization tails on the real headless loop; if they stall unrelated async work, move the same single writer/connection to a dedicated blocking execution boundary, not one unordered `spawn_blocking` per SQL statement.

**Acceptance:** small active set over growing terminal history, large active checkpoint, repeated model/tool turns, deep forks and concurrent unrelated conversation queues. Record allocator bytes/counts, peak resident memory, DB/WAL separately, task cancellation and executor heartbeat latency under a controlled SQLite lock. Current measurements below establish neither zero-copy nor bounded residency.

**Decision implications:** keep D9/D10; honor D13's provider-before-remaining-residency order. `DESIGN.md` §5/§13/§17 remain the target; a faster small-record journal is not evidence they are fully implemented.

## Async traits, lifetimes and error shape: what to retain

The explicit erased signatures are idiomatic for the actual ownership model:

- `TaskKind`, `ModelService` and `Tool` are heterogeneous registry objects; boxed `Send` futures with lifetimes tied to `&self` are appropriate (`crates/ion-core/src/task/kind.rs:10-31`; `crates/ion-ai/src/service.rs:8-17`; `crates/ion-core/src/builtin/tool.rs:36-52`). Compiler probes on the pinned toolchain compiled native static async traits and explicit `impl Future + Send`; attempting a native-async `&dyn Trait` failed with E0038. Replacing the public erased method with `async fn` is not a drop-in improvement.
- `Send` futures support `tokio::spawn`; `Sync` on the service supports sharing. Futures/streams need not be `Sync` or `Unpin`. Keep `Pin<Box<dyn Stream + Send + 'static>>`: the returned stream owns transport/parser state, while only the opening future may borrow the service. Do not widen borrowed invocation futures to `'static` by reflexively cloning everything.
- `TypedHandler` is statically adapted into `TaskKind`; it need not itself be dyn-compatible. A static RPITIT `-> impl Future<Output = ...> + Send` could remove the inner typed-future box while retaining one erased box. That is optional simplification, not a measured performance need. Do not add `async-trait`, nightly trait features or a second scheduler to achieve it. The adapter also clones an `Arc<H>` per invocation (`crates/ion-core/src/task/typed.rs:207-240`), though its returned lifetime permits borrowing; remove only as a coherent ownership cleanup.
- `DeserializeOwned` is reasonable for owned durable input/checkpoint decoding across awaits. Keep necessary `Send` constraints; consider moving blanket `Debug` constraints from associated-type requirements to diagnostic impls when they needlessly restrict authors (`crates/ion-core/src/task/typed.rs:27-32`). Do not invent borrowing lifetimes for durable records.
- Keep `TaskRunError` as interruption, `TaskOutcomeKind` as durable terminal classification, and `TaskContextError::{Cancelled, Stale, Closed}` as authority/lifecycle failures. The typed adapter correctly refuses to terminalize unreadable **dispatched** work or unencodable handler results (`crates/ion-core/src/task/typed.rs:218-240,302-321`). Its introductory “decode failure settles Failed” comment at `:22-24` and `DecodeFailure` comment at `:264-265` need narrowing; behavior, not that stale prose, is the design to retain.

For the typed adapter specifically, add a serde serializer that deliberately fails after a witnessed handler action, and an unreadable **abort** checkpoint case. Require interruption with durable evidence intact, not a fabricated Failed/Aborted result. The existing recovery decode test (`crates/ion-core/src/task/typed.rs:395`) does not establish every branch. These are precise D4/D6 guarantee tests, not reasons to add generic error plumbing everywhere.

## Visibility, invariant encoding and semantic modules

These are lower-priority surface repairs, not reasons to reorganize the kernel wholesale. D14/D16 and `DESIGN.md` §6/§8/§18 apply.

| Surface / evidence | Recommendation and check |
|---|---|
| `LocalSeq` is public/re-exported, has `next`, and typed IDs publicly convert to it (`crates/ion-core/src/id.rs:44-84,108-129`; `crates/ion-core/src/lib.rs:21-23`). §6 calls the backing namespace private. | Make the allocator representation/accessors crate-private; keep typed ID parsing, display, serde and numeric transport access as required by clients. Do not claim this prevents deliberate cross-kind reconstruction from integers; it makes the intended surface honest. External compile tests should preserve typed-ID usability without exposing allocator progression. |
| Public `TaskRecord` has independently settable status/generation/invocation fields and constructors (`crates/ion-core/src/task/record.rs:10-35,38-70`). | Treat it as a read DTO, not a validated live capability. Remove unused construction/mutation helpers from the external surface; keep writer transitions authoritative and validate loaded combinations. Do not build a typestate matrix for every session relationship: cross-record invariants remain writer work. |
| `Artifact` exports URI/digest/count fields without a publisher, lookup or durable artifact table (`crates/ion-core/src/artifact.rs:5-13`; `crates/ion-core/src/task/output.rs:6-10`). Scoped searches find no artifact consumer beyond declarations/re-exports. | Delete this unused surface, including unbacked output-reference plumbing, until a designed artifact owner can enforce publish-before-reference and digest/length integrity. Do not imply that `Some(ArtifactId)` is recoverable large-output support. A future accepted boundary needs crash-before-reference and missing/corrupt-content tests. |
| `Session`, `TaskDriver`, requests, typed/raw task traits and views are re-exported; state/journal/store remain private (`crates/ion-core/src/lib.rs:8-41`; `crates/ion-core/src/session/mod.rs`). | Preserve semantic modules and convenient root imports. Multiple import paths are not multiple runtime authorities. Fix missing typed capabilities rather than exposing `SessionState`, `Editor`, SQL or the private persistence trait. Narrow public constructors by actual consumers, not blanket getter boilerplate. |
| `session/{owner,scheduler,state,transaction}.rs` are 806/998/901/1090 lines. Responsibility splits are already visible: observation reading in `owner.rs:424-556`, task-context bridge in `scheduler.rs:677-751`, transcript visibility in `state.rs:205-394`, finalization planning in `transaction.rs:256-416`. | Extract focused observation, invocation-read, transcript-visibility and plan-application owners when repairing those seams. Keep one writer, one journal and one scheduling path. Do not merely move functions into `helpers.rs`, create extra crates, or split cohesive SQL record leaves for size alone. Review `k5_generation.rs` by scenario as its broad suite grows. |
| In-place preparation now precedes durability, but stale prose still describes map-clone drafts (`crates/ion-core/src/session/state.rs:16-25`) and the layout diagram says post-commit index installation (`docs/source-layout.md` §3, store). | Align comments/layout when implementing the relevant repair: prepare unobservable journaled writes → commit → publish; on failure roll back and fence as appropriate. `DESIGN.md` §5's “installs the prepared resident state” needs the same precision. Do not reintroduce the removed clone to match old wording. |

The journal also depends on explicit `rollback` calls rather than rollback-on-drop (`crates/ion-core/src/session/journal.rs:49-52,209-266`; `crates/ion-core/src/session/owner.rs:758-794`). No safe-command panic witness was established here. Consider a rollback-on-drop guard with an explicit successful commit/disarm operation when editing this owner, and fault the session on uncertain commit unwinding. The test replay helper currently drops temporary editors to retain writes (`crates/ion-core/src/session/persistence_tests.rs:194-211`); it must explicitly accept already-durable writes under such a change. Merely adding `Drop` would break that helper's semantics. This is defensive invariant encoding, not an additional reproduced defect.

## Dependencies and toolchain

`Cargo.toml:5-8` and `rust-toolchain.toml:1-3` agree on Rust 1.98.0/edition 2024. No compiler upgrade or version bump is needed for the recommendations. `cargo tree --locked` confirms:

- `ion-ai`: futures-core/util 0.3.34, serde 1.0.229, serde_json 1.0.151, thiserror 2.0.20. Tokio is dev-only. Keep the independent contract/scripted build free of session/store and mandatory HTTP/runtime dependencies.
- `ion-core`: the same serialization/future/error leaves, `ion-ai`, bundled rusqlite 0.40.2, Tokio 1.53.1, tokio-util 0.7.19 and resolved UUID 1.25.0 (manifest requirement 1.24.1). These have concrete current roles: no dependency/framework replacement is justified by this review.
- Workspace Tokio already supplies macros, multithread runtime, sync and time (`Cargo.toml:21`); the repeated dev-feature declaration in `ion-ai` is harmless but redundant. Feature-minimization is secondary to a real dependency boundary, not a reason to migrate runtimes.

For R7, choose one optional HTTP stack and bounded framing implementation behind the provider boundary, with fixture-tested cancellation and no hidden retries. Do not add a vendor SDK that silently owns retry policy, a public storage-backend framework, `async-trait`, a generic plugin engine, or another error abstraction merely for stylistic uniformity. Preserve source chains privately with existing `thiserror`.

These observations are about installed/compiler and resolved dependency evidence, not an upstream release/security audit. No claim is made that the pin is the newest available patch.

## Measurements: journal/index gains versus remaining costs

Host: Apple M3 Max, arm64, macOS 26.6.2 (25G83). Three sequential release runs at each scale, using the existing harness and local SQLite WAL/FULL policy:

```sh
ION_MEASURE_TASKS=N ION_MEASURE_OUTPUT_BYTES=1048576 \
ION_MEASURE_FORK_DEPTH=32 ION_MEASURE_SEED_ENTRIES=256 \
cargo run --locked --release -p ion-core --example storage_measure
# N = 4000, then 16000; three runs each
```

The build phase admits/drives tiny tasks, not the built-in provider loop (`crates/ion-core/examples/storage_measure.rs:250-307`). The 1 MiB output and depth-32 fork phases run separately. RSS uses `ps`; total storage includes DB, WAL **and SHM** (`:153-180`). Page timing is a full sequential read of 64-entry pages (`:315-337`). Reopen follows 64 additional tail turns (`:345-371`).

| Observed metric | 4,000 turns, three-run range | 16,000 turns, three-run range |
|---|---:|---:|
| Build duration | 0.841–0.854 s | 3.314–3.784 s |
| Throughput | 4,682–4,755 turns/s | 4,228–4,829 turns/s |
| Per-run admission p50 | 80–81 µs | 80–83 µs |
| Per-run settlement p50 | 105 µs | 105–108 µs |
| Whole transcript, pages of 64 | 1.031–1.055 ms; 63 pages | 4.155–4.324 ms; 250 pages |
| RSS after build | 15,056–15,152 kB | 39,072–39,136 kB |
| DB bytes after build | 1,527,808 | 6,152,192 |
| DB + WAL + SHM after build | 5,725,928 | 10,370,912 |
| Reopen | 9.645–9.777 ms | 37.186–39.620 ms |

Interpretation: the typical small-record write path no longer shows history-proportional per-command medians in this range; full paged traversal scales roughly with entry count. This supports retaining D9/D10, not proving asymptotic bounds. Residency and reconstruction still grow. Tail noise matters: the third 16k run reached 43.4 ms admission and 126.5 ms settlement maxima despite similar medians. Single 1 MiB output settlements in the 4k runs ranged 7.23–43.36 ms; that is not a stable large-output latency estimate. Cancellation p50 over 16 pending-turn samples ranged 275–432 µs at 4k and 306–405 µs at 16k, not a provider-connect cancellation benchmark.

No pre-D9 baseline was independently rerun. `54367ada` reports historical improvements, but those commit-reported numbers are not an A/B result of this review. No allocation counter or representative model-context workload was run, so deep-clone costs and aggregate request duplication above are explicitly source-derived. The next useful comparison is actual repeated generation/tool turns with bounded requests and large checkpoints, not a replacement runtime benchmark.

## Good designs not to churn

- **Independent neutral crate (D5):** `ion-ai` receives no task/session/store identity. Keep provider transport from acquiring mutation or durable retry authority (`crates/ion-ai/src/service.rs:12-17`).
- **One writer/private persistence (D1/D9):** batches separate durable writes from observations; rejection rolls back, uncertain persistence fences, and publication follows commit (`crates/ion-core/src/session/owner.rs:758-805`; `crates/ion-core/src/store/sqlite/commit.rs:17-31`). Strengthen the failure cases, not the number of authorities.
- **Typed local IDs and constrained context (D2/D3):** positive checked IDs, immutable history, pure context projection and complete-exchange forks are coherent primitives (`crates/ion-core/src/id.rs:44-63`; `crates/ion-core/src/conversation/context/{projection,fork}.rs`). Privatize the allocator seam without replacing session-local ordering with unrelated UUIDs.
- **Owned invocation lifecycle (D4):** dropping a drive waiter does not discard accepted work; normal and abort admission are separate; client waits consume no execution permits (`crates/ion-core/src/session/scheduler.rs:269-280`; `crates/ion-core/src/session/{capacity,lifecycle,wait}.rs`). Keep fresh abort generations and stale-write fencing instead of trying to “cancel” durable work by dropping every future.
- **Honest recovery uncertainty (D6/D11):** absent/readable/unreadable checkpoints are distinct, tool dispatch is recorded first, and unsafe repeats become indeterminate (`crates/ion-core/src/builtin/checkpoint.rs:19-40`; `crates/ion-core/tests/k5_recovery.rs:299-435,621`). D12's same-name replacement gap remains open: add recorded implementation identity plus reopen-after-replacement coverage, not a generic Effect lifecycle.
- **Unknown usage and incomplete response states:** preserve `Option<u64>` rather than inventing zero, and explicit termination rather than treating EOF as success (`crates/ion-ai/src/usage.rs:4-25`; `crates/ion-ai/src/response.rs:6-35`; `crates/ion-core/tests/k5_generation.rs:523`). Extend their validation/accounting, not their basic distinctions.
- **Real crash/ownership evidence:** OS-held writable ownership and actual process-death tests are more meaningful than mocked “crashes” (`crates/ion-core/src/store/sqlite/ownership.rs`; `crates/ion-core/tests/k4_ownership.rs`; `crates/ion-core/tests/k4_sqlite.rs:642-705`). Acknowledged-entry survival does not establish every dispatch/settlement crash window or machine power-loss durability; retain those scope limits.

## Delivery implication

Treat findings 1–4 as focused correctness/test slices before freezing this surface. Complete typed capability parity and the provider request/error/limit design through the existing R7 order; repair observation waiting before building its streaming client. Keep the accepted core nouns, journal and indexes. Re-run the required repository gates for each implemented Rust slice, and close findings in the roadmap only with their boundary-specific regressions and commit evidence. This review changes neither accepted decisions nor repair status.
