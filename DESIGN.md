# Ion — Durable Harness Architecture

**Status:** normative architecture contract for the Rust implementation  
**Date:** 2026-08-28  
**Scope:** durable harness, session/runtime ownership, effects, tools, agents, and persistence  
**Primary implementation target:** Rust 1.98.0, Edition 2024, Tokio, macOS + Linux first

## 1. Product definition

Ion is a user-owned, provider-neutral coding harness for the terminal and compatible frontends. The ordinary model-facing loop stays deliberately small:

```text
project context
→ request model
→ admit and execute tool/effect work
→ append validated results
→ continue or finish
```

The sophistication belongs in durable state, ownership, recovery, policy, context projection, and lifecycle—not in a planner framework around the model.

TUI, print mode, JSON/events, ACP, MCP, extensions, child agents, and a future daemon use the same durable semantics. None gets a second transcript or agent loop.

## 2. Evidence and authority

Ion owns its contracts. External systems are evidence, not compatibility targets.

Reference weighting:

1. **Pi 2** is the primary durable-session reference: parent-linked history, lanes as active cursors, one writer with one open operation per lane, parallel slow effects, forks, provision-before-effect identity, replay/reconciliation, and explicit total lane/operation state. Ion follows the logical model rather than Pi's TypeScript API or JSONL persistence.
2. **DeepSeek Harness / Cordis** is the strongest ownership/composition cross-check: agent-scoped visibility, lifecycle-owned registrations/resources, private setup followed by one publication point, rollback on failed admission, and one authority per independent fact.
3. **Codex** is the strongest production constraint on multi-agent control: family-scoped authority, execution/rollout budgets, control lineage separate from history lineage, retained identity separate from live capacity, cancellation, recovery, and headless lifecycle visibility.
4. **Zed / Agent Client Protocol (ACP)** is the primary interoperability signal for the client/agent boundary: explicit session lifecycle, prompt/update/cancel flow, capability negotiation, permissions, and ordered resume updates.
5. **VS Code Agent Host / Agent Host Protocol (AHP)** is complementary evidence for a persistent host that owns sessions independently of editor clients, supports reconnect from snapshot plus ordered actions, and can serve multiple local or remote clients.
6. **Grok Build** is strong inspectable Rust evidence for background/resumable agents, worktree isolation, transition-driven waiting, per-agent workspace identity, and host-stamped lifecycle facts.
7. **Cloudflare Agents** reinforces the identity/residency distinction: durable agent identity must not depend on an always-running process.
8. **Prime Agent** and **Headlong** are experimental stress tests for recursive, persistent, message-driven, and long-running compositions. Their benchmark or shared-mind choices are not core architecture authority.

Cursor and Warp/Oz are product/UX evidence for parallel sessions, worktrees, remote agents, and unified agent views. Factory Droid and Amp are useful closed-source references only where public docs or blogs expose concrete observable semantics; do not infer their internals. OpenCode remains low-weight because repeated rewrites weaken convergence claims. Gemini CLI is not an architectural reference.

Do not transliterate another system's TypeScript API, physical persistence format, compatibility baggage, or framework vocabulary into Rust.

## 3. Core invariants

### 3.1 One writer per loaded session

One loaded durable session has exactly one mutation authority. Provider, tool, process, and agent work may run concurrently outside that mutation line, but results become authoritative only when the session writer commits them.

Do not distribute session truth through an ambient `Arc<Mutex<SessionState>>` graph.

### 3.2 Durable acceptance before acknowledgement

When Ion reports that input, an approval decision, cancellation, or repeat-sensitive effect has been accepted, enough state is already durable to recover that fact after process loss.

### 3.3 External work is not generally exactly once

Repeat-sensitive effects use one of three explicit recovery semantics:

- safe replay;
- reconciliation / idempotent reattachment;
- visible indeterminate outcome when neither is safe.

Ion never silently retries a possibly mutating effect merely because a process restarted.

### 3.4 Semantic state and presentation state are separate

Durable semantic state includes conversation entries, lane state/configuration, operation acceptance/state, exact effect intents/settlements, durable inputs, usage, approvals, lineage, and artifact metadata.

Bounded stream/progress checkpoints may be durable recovery aids, but they do not prove semantic completion.

Streaming deltas, UI layout, subscribers, task handles, provider clients, sockets, and caches are live state only.

### 3.5 Model context is a projection

Canonical local state is not a provider request payload. A projector derives each model step from the selected conversation branch plus current model-facing contributions. Provider-hosted IDs and caches may accelerate execution but do not own meaning.

## 4. Durable session topology

```text
Session
├── append-only conversation tree
├── lanes
│   ├── total lane state
│   │   ├── leaf
│   │   ├── current operation
│   │   └── pending next-run input
│   └── total lane configuration
├── immutable operation records
├── append-only total operation-state revisions
├── effects
├── durable inputs
├── usage ledger
└── facts / labels / metadata
```

Every session starts with a `main` lane. Initial terminal behavior may expose only `main`; multiple lanes are an underlying durable primitive, not a requirement for immediate UI complexity.

## 5. Conversation tree

Conversation entries are passive semantic history.

Each entry has:

- an opaque `EntryId`, provisioned by the authoritative session writer before persistence;
- an immutable parent `EntryId` or virtual root;
- semantic payload;
- storage ordering/timestamp metadata.

`EntryId` should be an independent UUIDv7 domain identity. It must not encode or be derived from storage sequence. Sequence is useful ordering metadata and may support indexed queries, but it is not semantic identity.

The tree only grows. A new append uses the target lane's current leaf as its parent, then moves that lane's leaf to the new entry in the same durable transaction. Multiple entries in one transition form a parent-linked chain in transition order.

Branches share prefixes by reference. Branching within a session does not copy history.

The conversation tree must not contain lane pointers, operation state, effect bookkeeping, queues, usage, agent control, or runtime configuration merely because those values are durable.

## 6. Lanes

A lane is a durable named cursor into the shared conversation tree plus work serialized at that cursor.

Different lanes may point at the same leaf, execute concurrently, and diverge on later appends. They still share the session's one mutation authority.

### 6.1 Total lane state

Lane state is a latest-value durable record. At minimum:

```text
LaneState
  leaf: Option<EntryId>
  current_operation: Option<OperationId>
  pending_next_run: Option<NextRun>
```

It must be readable directly; recovery must not reconstruct it by folding queue/history events.

There is at most **one open operation per lane**.

### 6.2 Input semantics

Input has explicit delivery semantics instead of being modeled as several hidden operations:

- **prompt / submit-if-idle**: starts a run only if the lane has no current operation; otherwise rejects as busy;
- **next-run**: durable lane-owned input that waits outside any operation and is captured when the next run is accepted;
- **steer**: operation-owned input applied at the next reasoning boundary of the current run;
- **follow-up**: operation-owned input that belongs to the current run but waits until its current continuation settles;
- **notify**: durable delivery that need not implicitly start model work when such a caller exists.

`next_run` queues at most one lane-owned input outside any operation. It provisions the semantic `EntryId` immediately, but no `OperationId` exists until the lane actually accepts that run.

When a run is accepted, the transition atomically captures the lane's pending next-run input, preserves its provisioned semantic entry identity, provisions the `OperationId`, appends the resulting semantic entries, creates the immutable operation record and first total state, installs `current_operation`, and clears the captured pending input.

### 6.3 Total lane configuration

Lane configuration is separate latest-value state for future model work. Initially it only needs values Ion actually owns, starting with model selection. Reasoning/thinking mode and active capability/tool selection belong here only when real runtime behavior needs them.

Conversation navigation changes the lane leaf; it does not implicitly time-travel execution configuration.

Each model generation freezes the exact effective configuration it uses. Later lane-config changes do not alter an in-flight or recovered generation whose input was already persisted.

Model selection exists only as authoritative lane configuration; it is not semantic conversation history.

## 7. Operations

An operation is one accepted run on one lane.

Immutable acceptance data includes at least:

```text
Operation
  id
  session
  lane
  source_leaf
  accepted input / intent
  accepted ordering/time metadata
```

The mutable side is a **total** latest operation state. One latest state plus immutable acceptance must be sufficient to determine what may happen next. Historical revisions may be retained when audit, debugging, or evaluation earns them, but recovery never depends on folding partial history.

```text
OperationState
  phase
  cancellation/control flags
  operation-owned steer/follow-up input
  frozen or pending effect information needed for continuation
  complete continuation state
```

No prior revision may be required to fill missing fields in the current state.

Cancellation is orthogonal control over active workflow state rather than an artificial workflow phase.

When an operation reaches a terminal durable state, the same transaction clears the lane's `current_operation` when it still points to that operation.

## 8. Atomic transition rule

Every authoritative change follows one rule:

> Compute the next total state, then atomically commit every durable mutation required to make that state true.

One transaction may contain:

- new tree entries and lane-leaf movement;
- lane state/configuration changes;
- immutable operation acceptance;
- a new total operation-state revision;
- durable input capture/consumption;
- effect intent or settlement;
- usage;
- facts/labels;
- reconciliation evidence.

Either all logical mutations commit or none do. Failed persistence never installs live success.

Slow provider/tool/process/agent work does not run on the session mutation line.

## 9. SQLite representation

Logical architecture does not inherit Pi's JSONL representation.

Ion uses SQLite and should use it naturally:

- conversation entries are immutable rows with IDs and parent links;
- lanes have a directly readable current-state projection plus configuration;
- operation acceptance is immutable;
- the latest total operation state is directly readable, with revision history retained only where it provides real audit/debug/evaluation value;
- effects and usage remain separately queryable durable records;
- foreign keys and transaction constraints enforce topology where practical.

A current-state projection is not a violation of append-only operation history. The logical contract is direct recovery and atomic state; physical duplication/indexing may be used when it improves queries or correctness.

Ion is pre-1.0 and may archive incompatible development schemas rather than carry speculative migrations.

## 10. Effects and recovery

No repeat-sensitive effect starts before its exact invocation intent is durable.

Typed runtime code operates on typed effect values. A storage codec may encode a discriminant plus JSON, but runtime/store logic should not repeatedly spelunk arbitrary JSON fields to rediscover domain state.

### 10.1 Model steps

A model step freezes the exact effective input needed for recovery and evaluation, including:

- model/provider identity and relevant capabilities;
- projected conversation context;
- effective tool/capability identities;
- trusted/model-facing context contributions;
- harness/profile identity when behavior differs;
- relevant cache/retry expectations.

The current `ion/default@1` profile ID is enough until structured profile behavior actually exists. Do not invent precision by hashing a hard-coded label.

### 10.2 Child/agent creation

The intended child identity is preallocated or deterministically derived before creation executes and is stored in the parent invocation. Recovery reattaches/reconciles instead of spawning a duplicate.

## 11. Tool boundary

The session runtime must not infer semantics from public tool names such as `write`, `edit`, `bash`, or `spawn_child`. Tool-owned typed admission metadata now carries canonicalization, recovery, reconciliation, and policy-route semantics into runtime admission.

The tool boundary resolves a model call into an admitted invocation:

```text
model call
→ resolve tool
→ validate / prepare exact invocation
→ canonical policy target
→ recovery / reconciliation semantics
→ persist typed effect intent
→ execute
→ persist settlement / evidence
```

Recovery policy follows invocation semantics, not whether a tool is native, MCP, extension-provided, or dynamically scoped.

## 12. Context, compaction, and caching

Canonical conversation history is never destructively compacted. Compaction creates a readable semantic baseline used by projection while older entries remain queryable.

Prompt-cache stability is a significant performance property. Stable system sections, tool definitions/order, project instructions, and serialization should stay stable within an operation unless an explicit safe boundary changes them.

Provider opaque state is an optimization only where a portable semantic representation remains available.

## 13. Agent topology

A subagent is an addressable agent using common session primitives, not a second runtime architecture.

History topology is explicit:

- **lane**: another lane in the same session, anchored to shared history;
- **fork**: another durable session seeded from a source entry/branch boundary;
- **fresh**: another durable session with no inherited conversation history;
- **external**: an ACP/other backend with its own history semantics.

Workspace placement is independent:

- shared working directory;
- isolated git worktree;
- future sandbox/remote workspace.

Foreground/background is a waiting/observation choice, not an agent type.

### 13.1 Durable identity and residency

An agent address is durable semantic identity. It must not embed a Tokio task, process incarnation, terminal/client attachment, execution permit, or foreground/background choice. The host provisions authoritative identity before work begins; worker code does not self-report host-owned facts such as identity, control parentage, workspace identity, or delivery mode.

Residency describes where an admitted agent is currently executable: an in-process session task, a future local worker process, a future remote host, or inactive/hibernated durable state. Residency is runtime state, not identity or conversation semantics.

### 13.2 Lineage

Control/spawn lineage and history lineage are distinct durable relationships.

Target session metadata should distinguish:

```text
control_parent_session_id: Option<SessionId>
fork_source_session_id: Option<SessionId>
fork_source_entry_id: Option<EntryId>
```

A fresh child may have control lineage without history lineage. A user fork may have history lineage without a control parent. A fork-context child may have both.

### 13.3 Family-scoped control

A root/session family owns one control authority above individual lanes/sessions for:

- stable agent identity/path and control parentage;
- retained/admitted descendants;
- lane/fork/fresh/external spawn admission;
- execution permits and concurrency/depth/token/time/rollout budgets;
- direct messaging;
- observe/wait/status;
- cancel/interrupt/resume and cancellation ownership;
- deterministic recovery/reattachment;
- background completion routing.

This is family-scoped, not one process-global mutable registry. Separate roots must not accidentally share namespaces, budgets, or cancellation trees. Retained identity and active execution capacity are separate: a completed agent may remain observable after releasing its execution permit. `ChildManager` and the test-only delegation path are migration scaffolding rather than target architecture.

Swarm/reviewer/supervisor strategies are ordinary compositions over this API, not privileged phases in the model loop.

### 13.4 Admission, waiting, and messaging

Spawn is admission-first:

1. provision durable identity and requested topology;
2. privately construct and validate scoped capabilities/resources;
3. durably publish/admit exactly once;
4. return the stable address promptly;
5. start or attach execution residency;
6. observe completion separately.

Failed or racing setup rolls back private resources. Foreground behavior is `spawn + wait`; background behavior is spawn without that wait. Completion is not part of identity creation.

Waiting wakes from authoritative state transitions rather than polling. One/any/all are distinct semantics; cancellation/deadline is explicit; dropping a waiter cannot consume completion or mutate durable agent state.

User prompts, agent-to-agent messages, background completion, schedules, heartbeats, and future external events converge on durable session input rather than mutating another session's projected context directly. Delivery policy decides whether an input steers active work, becomes a follow-up, becomes the lane's next run, or remains informational.

## 14. Scoped model-facing contributions

Shared infrastructure may provide tools, prompt/context sections, hooks/interceptors, observers, commands/skills, and owned resources whose visibility/lifetime belongs to a particular agent/lane scope.

Borrow DSH/Cordis's ownership property without adopting a universal service locator. Rust ownership and explicit typed boundaries should make registration and teardown structural.

Behavior-changing hooks and observational events are different APIs.

## 15. Workspace and verification

Conversation topology and workspace topology are independent.

Git worktrees are useful for concurrent mutating agents but unnecessary overhead for many read-only/shared-work agents.

A global Ion-owned `WorkspaceGeneration` cannot prove workspace freshness because users, editors, git, hooks, builds, and other processes mutate outside Ion. Correctness-sensitive verification binds to concrete observations such as file hashes, git HEAD/index/worktree state, command/test identity and status, output/artifacts, relevant paths, and relevant environment inputs.

A generation counter may exist only as optimization/cache metadata.

## 16. Runtime and process ownership

The durable model is independent of process placement.

One loaded session should naturally correspond to one session/harness owner task with one mailbox and one mutation authority. The current public `Runtime` is a migration name for an object that actually owns one loaded session. Do not invent a process-wide session registry merely to make that old name true.

A future higher-level `Host` or family controller may own multiple loaded session harnesses when concrete lifecycle needs require it.

A likely eventual public shape is one session-oriented harness plus lane-targeting command surfaces. Exact names should be chosen once the topology migration is real; do not proliferate temporary `Runtime*`, `Manager`, `Service`, or `Handle` types merely to sketch it.

### 16.1 Client/host boundary

The execution host/session writer is authoritative. TUI, print/JSON, ACP, and future remote or multi-client protocols project an initial snapshot plus ordered runtime/session events/actions and send commands carrying stable semantic IDs. A client disconnect must not implicitly cancel durable work. Settle this boundary before redesigning the TUI so frontend ownership never leaks into execution semantics.

## 17. Rust API and module discipline

Follow the repository's Rust expert guidance and idiomatic Rust conventions:

- strong domain newtypes/enums instead of loose strings/integers;
- modules provide namespace context so surviving type names stay short;
- store rows/codecs, session tasks, and reducer internals stay `pub(crate)` unless external callers require them;
- public re-exports are deliberate API decisions;
- bounded channels and structured task ownership;
- ordinary `new`, `open`, `create`, `spawn`, `fork`, `send`, `wait`, `cancel`, and `close` vocabulary;
- `Manager`, `Service`, `Controller`, `Factory`, `Handle`, `Config`, and `Record` only when the term expresses a real domain/lifetime distinction;
- builders only when optional staged construction genuinely improves the API;
- split modules by ownership/cohesion, not line count;
- dependencies are acceptable when they simplify or strengthen the implementation enough to earn their maintenance and compatibility cost;
- do not add future types that have no production owner yet.

Current domain direction:

```text
ion-core/src/
  session/      # tree, lane topology, durable session-facing domain
  operation/    # pure operation reducer/state
  effect/       # typed external-effect vocabulary
  runtime/      # live owner/task while migration proceeds
  model/        # provider/context/model-step vocabulary as extracted
  tool/         # tool contract, preparation, catalog, native tools
  agent/        # family control once it owns real behavior
  workspace/    # worktree/execution abstraction when introduced
  store/        # SQLite persistence/codecs
  extension/
  mcp/
  policy/
  process/
```

## 18. Validation and CI

Ion currently targets and pins Rust **1.98.0**. The workspace `rust-version`, checked-in toolchain, and CI compiler should describe the same contract.

CI validates that contract with:

- `cargo fmt --check`;
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`;
- `cargo test --locked --workspace`;
- dependency/security auditing as a separate scheduled concern.

There is no separate older MSRV promise unless Ion deliberately chooses one later. Dependencies should be judged on correctness, fit, maintenance cost, and compatibility with the current toolchain rather than avoided merely to preserve an obsolete support floor.

CI is a validation mechanism, not an architecture authority. Update it when its checks no longer match the repository's declared toolchain contract.

## 19. Current implementation checkpoint

The current workstream has already established:

- authoritative UUIDv7 `EntryId` provisioning before persistence;
- the parent-linked conversation tree and durable lane rows;
- total lane state/configuration (`leaf`, `current_operation`, one `pending_next_run`, model config);
- `next_run` reserving only semantic entry identity while `OperationId` is created at acceptance;
- lane-addressable operation admission with immutable accepting lane and exact source leaf;
- later commits deriving lane ownership from immutable origin;
- model selection exclusively in lane config;
- tree-aware recovery that loads and validates every durable lane over the shared tree, with pending durable inbox restored per operation;
- Rust 1.98.0 as the workspace/toolchain/CI contract.

Storage, recovery, and live execution now support multiple concurrent lanes under one session writer. Operation residency/effects/continuation are operation-addressed, family-scoped retained agents have separate execution permits, waits are event-driven, and agent messaging uses the durable input path. The current client snapshot still projects `main`; scoped capabilities and replacement of the remaining child-only scaffolding are the active boundary.

## 20. Implementation order

1. Retain the full conversation tree and all durable lane state in the live session owner while preserving the current `main` projection.
2. Replace singleton active-operation/draft/effect residency with operation-addressed, lane-owned runtime state; route provider/tool signals by stable operation identity.
3. Add the runtime-owned lane admission surface together with its durable transaction, then allow concurrent slow effects across lanes under the one session writer.
4. Introduce family-scoped agent control with admission-first identity, separate retained registry/execution permits, explicit wait semantics, cancellation ownership, and deterministic reattachment.
5. Replace child-only topology with lane/fork/fresh agent admission. Add worktree/remote topology only when a concrete owner exists.
6. Add durable agent messaging/background completion through the common session-input path.
7. Make scoped capability publication/teardown structural at lane/agent admission and exact on recovery; capability narrowing must never be reset by unrelated lane configuration changes.
8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants.
9. Shrink/rename public Rust API and remove dead migration scaffolding once ownership is stable.
10. Redesign the TUI only after the authoritative session/agent host contract is coherent, with ACP as a first-class client boundary.

Research from here is question-driven at concrete implementation boundaries, not another broad framework survey.
