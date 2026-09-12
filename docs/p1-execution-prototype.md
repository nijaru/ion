# P1 execution prototype evidence

**Date:** 2026-09-12  
**Status:** prototype validated; production promotion still open  
**Validated code head:** `78148e84d6120d5670a784ec3ecb07684577db1d`

This note records implementation evidence for the first P1 execution/transaction prototype. It is not a second architecture specification. `DESIGN.md` remains the target contract and `ROADMAP.md` remains the work-order authority.

## What was built

The prototype lives only under `crates/ion-core/tests/`:

- `p1_runtime.rs` drives the deterministic scenarios.
- `p1_support/mod.rs` contains the two authoring candidates and the minimal UI reducer.
- `p1_support/store.rs` is an isolated SQLite durability fixture.

It does not alter or migrate existing Ion session storage. The fixture must be removed after its validated semantics are promoted into the production runtime; it is not a second production engine.

The prototype deliberately models only the state needed to exercise the first execution boundary: typed session-local agent/input/task/effect IDs, a commit sequence independent of object identity, input receipts, retained workers, task generations, effect attempts and recovery class, checkpoints, terminal outcomes, and provisional output fencing.

## Evidence established

The current fixture demonstrates the following properties with deterministic tests:

1. **Durable idempotent admission.** An admitted request is reopened from disk and retried with the same request key. Identical target/content/mode returns the original receipt; changed content or delivery mode conflicts without creating new work.
2. **Independent effect settlement.** A turn admits two effects together. B settles before A and its result is durable immediately; model-facing projection remains A then B by original call ordinal.
3. **Durable checkpoint resume.** The typed re-entrant turn checkpoint is persisted, the store is closed, and the behavior resumes from that checkpoint after reopen.
4. **Retained worker lifetime.** Worker identity and initial task are admitted atomically while the spawning task is already terminal; the worker remains addressable and inspection dispatches no effects.
5. **Capacity-safe waiting.** A coordinator waiter does not hold the only model-request semaphore permit, so the worker can acquire it and complete.
6. **Cancellation fencing.** Settlement-before-cancel and cancel-before-settlement are both tested. Cancellation advances the task generation; output, effect settlement, and normal completion from the old generation are rejected.
7. **Early P4 target safety.** Root and worker drafts are independent. A delayed command reply remains bound to its captured target after focus changes.
8. **Abrupt process-loss recovery.** A subprocess commits an effect intent, performs and fsyncs an external witness, then exits through `std::process::exit` before settlement. Reopen creates a new attempt for a retry-safe effect, but converts a no-safe-retry effect to an indeterminate outcome without repeating the witness.

The abrupt-process fixture is intentionally stronger than a cooperative close/reopen test. The witness file is test evidence only; it is not a proposed production transaction mechanism.

## Rust authoring API decision

**Recommendation: use a typed re-entrant task step/checkpoint API as the durable task boundary.** Async Rust remains appropriate inside provider, tool, process, and other effect adapters, but an async stack frame should not be the durable continuation model.

Both styles can express concurrent live effects. The difference appears at recovery:

- The re-entrant candidate has an explicit serializable checkpoint. After process loss the runtime reloads typed state and asks the behavior for its next step. Durable effect intent/results remain runtime-owned rather than hidden in local variables.
- The async candidate can `join!` live effects naturally, but process-local futures, borrows, and locals vanish on restart. Making it recoverable requires recreating a durable state machine beside the async method, duplicating the task lifecycle the runtime already has to own.

This decision is based on recovery and ownership behavior, not on preserving the existing `OperationMachine`. The existing production reducer is useful evidence that the shape is viable, but its current single-open-tool-effect model does not satisfy the new concurrent-settlement requirement and must be changed rather than protected.

The intended production shape should keep behavior payloads typed and keep any registry erasure private. The runtime owns admission, task/effect identity, durable checkpoints, attempt/generation fencing, persistence, scheduling, and cancellation. A behavior returns typed next-step/effect/completion intent; it does not write session history or SQLite directly.

## Representation choices proven only for the prototype

The fixture uses monotonically allocated typed integer IDs scoped to the prototype database and a separate monotonically increasing commit sequence. This successfully demonstrates that object identity does not need to encode commit order.

That is **not yet sufficient to freeze the production physical representation**. Promotion still needs explicit answers for allocation visibility within atomic batches, rollback behavior, import/fork remapping, process ownership, and how production sequence values compose with the existing store. Do not migrate the real schema merely to match this fixture.

Effect identity and retry attempt are separate. The effect ID remains stable across recovery; a retry-safe recovery increments its attempt. That distinction should survive production promotion.

## Validation

GitHub Actions run `34703651147` validated commit `78148e84d6120d5670a784ec3ecb07684577db1d` with the repository's pinned Rust 1.98.0 toolchain:

```text
cargo fmt --check                                                     PASS
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings  PASS
cargo test --locked --workspace                                      PASS
```

No paid or live provider call was used. No PTY or human-terminal acceptance is claimed.

## What this disproved or changed

The existing production operation reducer cannot simply be treated as P1-complete because it durably tracks one open tool effect and advances planned tools serially. P1 requires independent read-only calls to be admitted together and allowed to settle out of order while preserving call-order projection.

The prototype also weakens the case for an async-method task abstraction as the durable authoring surface. Async remains an implementation mechanism for effects, but recovery is substantially clearer when the durable continuation is explicit typed state.

## Still open before P1 can close

This is an evidence-bearing prototype, not completion of P1. The next slice must promote the proven semantics into the production runtime and then rerun equivalent tests against production components. At minimum it still needs to cover or resolve:

- production request-key receipt binding and retry after real `SessionStore` reopen;
- multiple simultaneously admitted tool effects in the real operation/runtime path;
- atomic successor/ownership transfer without a false-idle interval;
- explicit pending-task reopen that performs no external work until driven;
- caller disappearance before versus after admission;
- rejected versus uncertain persistence commit handling and session fencing;
- real writable-session ownership / second-writer exclusion and abnormal release;
- missing task-kind recovery with retained data;
- failure/panic/host-close ownership and joins;
- explicit input terminal/unanswered disposition;
- known dependency/self-wait cycle rejection;
- a runtime observation contract consumed by the real TUI layer rather than only the prototype UI reducer.

P4 also remains open beyond the early target/draft reducer checks: no PTY behavior, reconnect stream, narrow-layout, Unicode editor, hidden-worker approval, output-flood, or human-terminal acceptance has been established here.

## Next bounded slice

Promote the execution semantics into existing production ownership rather than growing the fixture:

1. Add request-key/receipt admission to the real session command/store transaction boundary.
2. Replace the operation reducer's single open tool effect with a typed exchange/checkpoint that can own multiple admitted effects and retain each complete result independently.
3. Preserve source call order for context projection while allowing eligible effects to settle in completion order.
4. Carry task/effect invocation generation and attempt identity through the existing runtime recovery path.
5. Port the prototype tests to production `SessionRuntime`/`SessionStore` fixtures as each invariant becomes real, then delete the corresponding prototype storage implementation.
6. Feed production group/selected-agent observations into `ion-terminal` and keep inspection read-only.

Do not redirect existing persisted sessions into a new schema until migration or explicit refusal/archive behavior is designed and tested.
