# Architecture v2 migration note

**Status:** rationale and migration guide for the architecture now merged to `main`.  
**Normative design:** [`../DESIGN.md`](../DESIGN.md).

This note records why the architecture changed direction and how the remaining implementation should move toward the canonical design. It is not a second contract.

## Reference weighting

Pi 2 is the primary practical and architectural reference for Ion's harness. The most important transferable ideas are:

- append-only parent-linked conversation history;
- named lanes as active cursors into shared passive history;
- one session writer with at most one open operation per lane;
- parallel slow effects across lanes;
- forks for isolation and lanes for shared-history concurrency;
- provision-before-effect identity/durability;
- prompt-cache-friendly tail growth;
- explicit replay/reconciliation semantics;
- the newer explicit-state redesign: immutable operation acceptance, total operation-state revisions, total lane state, and total lane configuration.

Ion follows that logical model rather than Pi's TypeScript API or JSONL physical representation.

DeepSeek Harness/Cordis independently supports lifecycle-owned, agent-scoped contributions over shared infrastructure. Codex independently supports family-scoped control, explicit control/history lineage, budgets, messaging, and Rust ownership boundaries. Grok Build is useful for concrete background/resume/worktree mechanics. Prime Agent is a pressure test for recursive/long-running composition.

## Key corrections to the old Ion design

The earlier design made several assumptions that no longer survive the evidence:

1. **Linear history is not the target.** Session history is an append-only parent-linked tree.
2. **One operation per session is too restrictive.** The invariant is one open operation per lane under one session writer.
3. **Queued prompts are not queued operations.** A busy lane has lane-owned `next_run` input; a new operation is created only when that run is actually accepted.
4. **Execution configuration is not conversation history.** Model selection and future reasoning/tool selections belong to total lane configuration. `SessionEntry::ModelChanged` is transitional scaffolding.
5. **Operation recovery should be direct.** Immutable acceptance plus one latest total operation-state revision must determine continuation; do not reconstruct a hidden program counter from partial records.
6. **Child topology is not one special manager.** Lane/fork/fresh/external agents share common durable primitives, with family-scoped control above them.
7. **Control lineage and history lineage differ.** Persist them separately.
8. **Workspace freshness cannot be proved by a global Ion generation counter.** Bind verification to observed file/git/command evidence.
9. **Tool recovery semantics belong to resolved invocations.** Session code must not switch on tool names or whether a tool was dynamically registered.
10. **The current `Runtime` name must not dictate architecture.** It currently owns one loaded session. Add a process/session registry only if concrete host lifecycle requires one.

## Durable target

```text
Session
├── conversation tree
├── lanes
│   ├── State { leaf, current_operation, pending_next_run }
│   └── Config { model, ...only earned fields }
├── immutable operations
├── append-only total operation states
├── effects
├── durable inputs
├── usage
└── facts
```

A run acceptance transaction should conceptually:

```text
capture lane.pending_next_run
append resulting semantic entries at lane.leaf
create immutable Operation { lane, source_leaf, accepted intent }
append first total OperationState
set lane.current_operation
clear captured pending_next_run
commit all of the above atomically
```

A terminal operation transaction appends the terminal total state and clears `lane.current_operation` if it still points to that operation.

`prompt` on a busy lane rejects. `next_run` queues outside an operation. `steer` and `follow_up` belong to the active operation state.

## SQLite direction

Use SQLite naturally rather than emulating Pi's log format:

- immutable `entries(id, parent_id, seq, payload, ...)`;
- one directly readable lane current-state/config projection;
- immutable operation acceptance with lane + source-entry identity;
- append-only total operation-state revisions;
- effects/usage/inbox/evidence in typed supporting tables/codecs;
- transactions and foreign keys enforcing cross-record invariants.

A directly readable current-state projection is compatible with append-only revision history. The current schema retains every total operation-state revision in an immutable ledger while keeping the existing latest-state row as a cheap recovery projection.

Operation topology is now captured separately in immutable `operation_origins`. While the runtime still addresses only hidden `main`, accepting an operation atomically records the exact lane and pre-acceptance source leaf. The database rejects operation acceptance if that lane cannot be resolved. When lane-targeted runtime commands become real, the current main-lane capture path should become an explicit lane argument without changing the origin contract.

Ion is still pre-1.0, so schema iterations may archive older development databases rather than carrying migration machinery whose compatibility value is not yet real.

## Rust/API direction

The architecture should become more Rust-like as it settles, not more framework-like:

- `session/` owns tree/lane topology;
- `operation/` owns the pure reducer/state machine;
- `store/` owns persistence codecs/transactions;
- `runtime/` remains the live owner/task while migration proceeds;
- `agent/` appears only when family-scoped control owns production behavior;
- modules provide naming context instead of root-level type-name prefixes;
- private implementation types should disappear from `ion_core::*` rather than merely receive prettier names;
- dependencies are fine when they materially improve correctness/clarity and respect the Rust 1.98.0 toolchain contract.

## Migration checkpoint

Already landed on `main`:

- runtime/store/tool module normalization;
- operation reducer moved from the misleading old session module into `operation/`;
- typed durable model/tool/compaction effect vocabulary;
- replay preserving durable operation identity;
- malformed durable-effect fencing;
- simplified `ion/default@1` harness identity;
- child live-slot leak fix;
- parent-linked entry schema and a durable `main` lane;
- entry identity decoupled from storage sequence and represented as UUIDv7;
- append-only total operation-state revision retention plus a current-state recovery projection;
- immutable operation-origin capture of accepting lane + exact source leaf;
- Rust 1.98.0 as the pinned workspace/toolchain/CI contract.

Three migration boundaries remain especially important:

- entry IDs are currently generated inside store insertion; the target ownership is the authoritative session transition provisioning the UUIDv7 before persistence so later lane/fork references can name intended entries before the write starts;
- model changes still use `SessionEntry::ModelChanged` to bridge old runtime behavior into lane config; the target is direct lane-config mutation with no semantic model-change entry;
- busy-lane prompts still become complete queued `Accepted` operations in live/durable state; the target is lane-owned `pending_next_run` input with an operation created only at actual run acceptance.

## Next implementation slices

1. Move UUIDv7 entry provisioning from store insertion to the authoritative session transition.
2. Make lane config authoritative and remove `ModelChanged`.
3. Add total lane state and move busy-lane queueing to `pending_next_run`.
4. Thread the existing immutable operation origin through load/runtime types and make operation acceptance explicitly lane-addressable.
5. Remove `queued_operations` from the live runtime and map current `enqueue` compatibility behavior onto next-run input.
6. Generalize the session owner from only `main` to multiple lanes and concurrent slow effects.
7. Finish typed effect codecs/admission boundaries.
8. Replace special-purpose child/delegate paths with family-scoped agent control.
9. Reconcile the public API/module vocabulary after the durable ownership model is stable.
10. Establish/expand `ion-eval` before adding higher-level orchestration policy.

Broad architecture surveying is complete enough to proceed. Further research should answer concrete implementation questions rather than reopen the substrate without new evidence.
