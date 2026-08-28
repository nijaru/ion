# Architecture v2 migration note

**Status:** working migration target for the current architecture workstream.  
**Canonicalization:** reconcile [`../DESIGN.md`](../DESIGN.md) with this note before treating the redesign as settled.

This note records the architecture Ion is migrating toward, the evidence behind it, and the order in which the remaining ownership boundaries should land. It is intentionally more specific about invariants than about public Rust names.

## Reference weighting

Not every agent implementation is equally useful architectural evidence.

### Tier A: core architecture

**Pi 2** is the strongest reference for durable harness semantics:

- append-only parent-linked conversation history;
- named lanes as active cursors into shared passive history;
- one session writer with at most one open operation per lane;
- parallel slow effects across lanes;
- forks for isolated history and lanes for shared-history concurrency;
- provision-before-effect identity and durability;
- explicit replay/reconciliation semantics;
- immutable operation acceptance plus latest total operation state;
- explicit total lane state/configuration in the newer state-machine redesign.

Ion follows the logical model, not Pi's TypeScript API or JSONL physical layout. Where Pi's older canonical note and newer explicit-state handoff differ, Ion currently favors explicit lane configuration outside semantic conversation history.

**DeepSeek Harness / Cordis** is the strongest complementary reference for ownership and composition:

- shared infrastructure with agent-scoped registration visibility;
- lifecycle-owned tools, prompt contributions, listeners, tasks, and resources;
- create/resume as a private setup transaction followed by one publication point;
- rollback of private resources if admission/publication fails or loses a race;
- one authoritative mechanism per independent fact instead of mirrored registries and readiness flags.

Pi supplies the durable session substrate; DSH/Cordis supplies the strongest ownership discipline around it.

### Tier B: production/system constraints

**Codex** is the strongest practical constraint on multi-agent control:

- one family/root-scoped agent-control authority;
- explicit execution limits and rollout budgets;
- control parentage separate from fork/history lineage;
- inherited effective configuration with child-specific overrides;
- retained agent identity separate from active execution capacity.

Its real failure modes are useful design tests for Ion: completed agents must release execution permits; cancellation must cascade deliberately; `wait any` and `wait all` must not be ambiguous; spawn admission must not hang invisibly on capacity; recovered children must restore effective configuration; tool schemas must match runtime affordances; and lifecycle state must be visible through headless/event APIs.

**Zed / Agent Client Protocol (ACP)** is the strongest current interoperability signal for the client/agent boundary:

- ACP has real multi-editor and multi-agent adoption rather than being a single-product internal protocol;
- its explicit session lifecycle, prompt/update/cancel flow, capability negotiation, and permission requests keep client UX concerns outside the agent reasoning loop;
- resume can replay session updates before the response completes, which forces clients to treat session state as an ordered protocol rather than a local UI object;
- Zed exercises this boundary with its own agent plus external agents such as Claude Agent and Codex CLI.

**VS Code Agent Host / Agent Host Protocol (AHP)** is complementary evidence, not a primary agent-substrate reference. Its August 2026 host redesign is unusually strong evidence for persistent host/client semantics:

- the dedicated agent host owns sessions independently of editor clients;
- work can continue with no editor attached;
- multiple local or remote clients can observe/control one session;
- reconnect is state-first: snapshot plus ordered actions/deltas;
- the host can sit above more than one agent harness/protocol.

ACP is the better signal for Ion's interoperable frontend boundary; AHP is the better corroborating signal for a durable host that outlives any one client. The shared conclusion is important: **TUI, ACP, and future remote clients are projections of authoritative runtime state, never the execution owner.**

### Tier C: strong mechanism references

**Grok Build** is particularly valuable because it is an inspectable Rust implementation. Its useful mechanisms include independent child sessions, admission/background separation, resume, explicit capability modes, shared-vs-worktree isolation, per-agent `cwd`, transition-driven waiting rather than polling, and host-stamped authoritative subagent/session/worktree/delivery facts.

**Cloudflare Agents** reinforces one important systems invariant: an agent is a durable identity, not an always-running process. Hibernation/residency can change without changing the semantic agent/session identity.

### Tier D: experimental stress tests

**Prime Agent** is useful for stressing the substrate with recursive children, daemon-backed continuity, direct messaging, goals, schedules, heartbeats, and a persistent model-facing control environment. Its benchmark claims are not architectural evidence, and its RLM/IPython environment is optional capability rather than core Ion machinery.

**Headlong** is useful for persistent-agent semantics: external messages as observations, trajectory/history separate from context projection, continuous autonomous work, and fork/merge experimentation. Its shared-mind semantics are not an appropriate default isolation model for Ion.

### Product/UX evidence only

Cursor and Warp/Oz are useful evidence that users benefit from parallel sessions, worktrees, cloud/remote agents, and unified agent lists. Their closed internals are not primary substrate evidence.

OpenCode remains low-weight because repeated rewrites weaken architectural convergence claims. Gemini CLI is not an architectural reference.

## Core corrections to the old Ion design

1. **Linear history is not the target.** Session history is an append-only parent-linked tree.
2. **One operation per session is too restrictive.** The invariant is one open operation per lane under one session writer.
3. **Queued prompts are not queued operations.** A busy lane has lane-owned `pending_next_run`; a new operation is created only when that run is actually accepted.
4. **Execution configuration is not conversation history.** Model selection and future earned execution settings belong to total lane configuration.
5. **Operation recovery should be direct.** Immutable acceptance plus latest total operation state must determine continuation without reconstructing a hidden program counter from partial records.
6. **A child is not a special runtime species.** Shared-history lane agents, forked sessions, fresh sessions, and eventually external/remote agents are topologies over common primitives.
7. **Control lineage and history lineage differ.** Persist and reason about them separately.
8. **Agent identity and execution residency differ.** A durable agent/session may be loaded in a Tokio task, hosted in another process, remote, or inactive without changing identity.
9. **History topology and workspace topology differ.** Shared history does not imply shared checkout; forked history does not require a worktree.
10. **Foreground/background is waiting policy.** It is not an agent type or identity property.
11. **Workspace freshness cannot be proved by a global Ion generation counter.** Verification must bind to observed file/git/command evidence.
12. **Tool recovery semantics belong to resolved invocations.** Session code must not switch on tool names or registration origin.
13. **The UI is not the runtime owner.** Terminal, ACP, future AHP/remote clients, and other frontends consume authoritative session state.

## Durable session target

```text
Session
├── conversation tree
├── lanes
│   ├── State { leaf, current_operation, pending_next_run }
│   └── Config { model, ...only earned fields }
├── immutable operations
├── latest total operation state (+ optional revision history)
├── effects
├── durable inputs
├── usage
└── facts
```

A run acceptance transaction should conceptually:

```text
capture lane.pending_next_run
preserve its already-provisioned semantic EntryId
provision OperationId at acceptance, never while merely queued
append the resulting semantic entry at lane.leaf
create immutable Operation { lane, source_leaf, accepted intent }
write first total OperationState
set lane.current_operation
clear captured pending_next_run
commit all of the above atomically
```

A terminal operation transaction writes terminal total state and clears `lane.current_operation` if it still points to that operation.

`prompt` on a busy lane rejects. `next_run` queues outside an operation. `steer` and `follow_up` belong to the active operation state.

## Agent architecture target

The current `ChildManager` combines several independent facts. The replacement architecture must separate them before public API naming is finalized.

### Durable identity

An agent address refers to durable semantic identity. It must not embed a task handle, process incarnation, terminal attachment, or execution permit.

The execution host provisions authoritative identity before work begins. Child/worker code does not self-report facts the host already owns, including agent/session identity, control parentage, workspace identity, or foreground/background delivery state.

### Residency

Residency describes where an admitted agent is currently executable:

- loaded in-process session task;
- future local worker process;
- future remote host;
- inactive/hibernated but durable.

Residency is runtime state, not conversation semantics and not agent identity. A session task/process is an incarnation of a durable session, never the durable identity itself.

### Family control

One root/family-scoped control authority owns:

- control parentage and stable agent addresses;
- admitted/retained descendants;
- execution permits and rollout/resource budgets;
- spawn, observe, wait, cancel, interrupt, resume, and messaging routing;
- cancellation ownership;
- deterministic recovery/reattachment;
- background completion delivery.

It is not process-global. Separate roots should not accidentally share agent namespace, budgets, or cancellation trees.

Retained identity and active execution capacity are distinct. A completed child can remain observable without consuming a live execution permit.

### Spawn is admission-first

Creation should have one authoritative admission transaction:

1. provision durable identity and requested topology;
2. privately construct/validate scoped capabilities and resources;
3. durably publish/admit the agent exactly once;
4. return its handle/address promptly;
5. start or attach execution residency;
6. observe completion separately.

If setup/admission loses a race or fails, all private resources roll back. Foreground behavior is `spawn + wait`; background behavior is `spawn` without the wait. Completion is never part of identity creation.

### Orthogonal spawn dimensions

History:

- shared-history lane;
- fork from a source entry;
- fresh session;
- future external/remote history source.

Workspace:

- shared workspace;
- worktree;
- future sandbox/remote workspace.

Capabilities/configuration:

- role/persona or agent definition;
- model/reasoning configuration;
- tool scope;
- MCP/extensions;
- policy/sandbox;
- budgets.

Waiting/delivery:

- return after admission;
- wait for one;
- wait for any of a set;
- wait for all of a set;
- deadline/timeout;
- background completion delivered as durable input.

These axes should not be encoded as a combinatorial enum of child types.

### Wait semantics

Waiting must wake from authoritative state transitions rather than polling. It should distinguish one/any/all explicitly, support cancellation/deadline, and ensure dropping a waiter cannot consume completion or change durable agent state.

### Messaging and autonomous input

User prompts, agent-to-agent messages, background completion, schedules, heartbeats, and future external events should converge on durable session input rather than mutating another session's context directly.

Delivery policy can decide whether input steers active work, becomes a follow-up, becomes the lane's next run, or is informational. The durable input identity is separate from that policy.

### Client/host boundary

The execution host/session writer is authoritative. TUI, print/JSON, ACP, and future remote/multi-client protocols are projections over:

- an initial snapshot;
- ordered runtime/session events or actions;
- commands carrying stable semantic IDs.

A client disconnect must not implicitly cancel durable work. This boundary should be settled before the TUI redesign so UI architecture does not leak into session ownership.

## SQLite direction

Use SQLite naturally rather than emulating Pi's log format:

- immutable `entries(id, parent_id, seq, payload, ...)`;
- directly readable lane current-state/config projection;
- immutable operation acceptance with lane + source-entry identity;
- latest total operation state, with revision history only where debugging/evaluation earns it;
- durable agent/control metadata separate from execution residency;
- effects/usage/inbox/evidence in typed supporting tables/codecs;
- transactions and foreign keys enforcing cross-record invariants.

Totality is a **logical invariant**, not a requirement to copy Pi's append-only JSONL physical representation. Recovery must be able to read the current state directly.

Ion is still pre-1.0, so schema iterations may archive older development databases rather than carrying migration machinery whose compatibility value is not yet real.

## Rust/API direction

The architecture should become more Rust-like as it settles, not more framework-like:

- `session/` owns tree/lane topology;
- `operation/` owns the pure reducer/state machine;
- `store/` owns persistence codecs/transactions;
- `runtime/` owns live session residency/task execution;
- `agent/` should appear only when family-scoped control owns production behavior;
- `workspace/` should appear when shared/worktree/remote ownership becomes real;
- modules provide naming context instead of root-level type-name prefixes;
- private implementation types should disappear from `ion_core::*` rather than merely receive prettier names;
- avoid ambient service locators and mirrored state authorities;
- dependencies are fine when they materially improve correctness/clarity and respect Rust 1.98.0.

Do not rename `ChildManager` into an `AgentManager` and call the migration done. The current design must first be decomposed by ownership: durable identity, family control, execution residency, history topology, workspace topology, and scoped capability publication.

## Migration checkpoint

Already landed on `main` before this workstream:

- runtime/store/tool module normalization;
- operation reducer moved into `operation/`;
- typed durable model/tool/compaction effect vocabulary;
- replay preserving durable operation identity;
- malformed durable-effect fencing;
- simplified `ion/default@1` harness identity;
- child live-slot leak fix;
- parent-linked entry schema and durable hidden `main` lane;
- entry identity represented as UUIDv7 rather than storage sequence;
- total operation-state retention/current recovery projection;
- immutable operation-origin capture of accepting lane + exact source leaf;
- lane config as authoritative durable model selection;
- Rust 1.98.0 workspace/toolchain/CI contract.

Completed in the current architecture workstream:

- UUIDv7 entry identity is provisioned by the authoritative session transition before persistence;
- SQLite stores the supplied identity instead of inventing one;
- `LoadedSession` round-trips full entry records;
- live session state retains those same durable entry records after reopen and after successful commit;
- payload-only consumers explicitly project semantic entries.

The busy-lane queueing migration is now the active checkpoint: `pending_next_run` owns a provisioned semantic `EntryId`, while `OperationId` is provisioned only when the lane actually accepts the run. SQLite persists the pending identity without inventing it, and no queued `Accepted` operation exists.

## Next implementation order

1. Finish and validate total lane state (`leaf`, `current_operation`, `pending_next_run`) with the provision-before-persistence identity rule above.
2. Make operation acceptance explicitly lane-addressable over the durable lane/source-leaf origin contract.
3. Generalize the session owner from hidden `main` to multiple lanes while retaining one writer and concurrent slow effects.
4. Introduce family-scoped agent control with admission-first identity, separate retained registry/execution permits, explicit wait semantics, cancellation ownership, and deterministic reattachment.
5. Replace `ChildManager`/child-only topology with lane/fork/fresh agent admission. Add worktree/remote topology only when its owner exists.
6. Add durable agent messaging/background completion through the common session-input path.
7. Add scoped capability publication/teardown around agent creation/resume.
8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery/multi-agent invariants.
9. Reconcile `DESIGN.md`, public API/module vocabulary, and dead compatibility scaffolding.
10. **Only then redesign the TUI** as an ACP-capable client of the authoritative session/agent host contract.

The broad agent-architecture survey is now sufficient to proceed. Further research should answer concrete implementation questions rather than reopen the substrate without new evidence.
