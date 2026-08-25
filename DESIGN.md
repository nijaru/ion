# Ion — Durable Minimal Harness Architecture

**Status:** Proposed target design for the clean-sheet Rust Ion implementation  
**Date:** 2026-08-20  
**Scope:** Whole-system architecture. This document supersedes phase-local design assumptions where they conflict with it.  
**Audience:** Coding agents and maintainers implementing Ion.  
**Primary implementation target:** stable Rust, Tokio, macOS + Linux first.

---

## 0. How to use this document

This is an implementation contract, not a feature wishlist.

Normative words:

- **MUST / MUST NOT**: correctness or architectural invariant.
- **SHOULD / SHOULD NOT**: strong default; deviate only with evidence.
- **MAY**: optional capability that does not define the core.

When this document and implementation disagree:

1. Inspect source, tests, Git history, and the current task.
2. If the implementation intentionally changed the design, update this document in the same change.
3. If the implementation drifted accidentally, restore the invariant rather than documenting the accident.

Do not copy Pi, DSH/Cordis, OTP, Codex, Jido, or any provider's wire format. They are evidence. Ion owns its contracts.

### 0.1 Read this first when implementing

The shortest correct mental model is:

> **Ion is a deliberately small model-facing agent loop inside a rigorous, durable, single-writer session runtime.**

The sophisticated parts are ownership, persistence, recovery, cancellation, policy, capability lifecycle, and context projection. The model-facing loop should stay boring.

### 0.2 Existing code is not disposable

The current Ion implementation already has useful pieces that should be migrated rather than rewritten gratuitously:

- one command path through `RuntimeHandle` / `RuntimeController`;
- typed provider-to-runtime signals;
- a single native/MCP/extension-facing `Tool` contract;
- a `ToolRegistry` with the small default tool set;
- tracked, cancellation-aware tool tasks;
- process-group cancellation for `bash`;
- provider/tool I/O already kept outside the controller mutation loop.

The redesign changes the durable execution model and ownership boundaries around those pieces. It does not justify a clean-slate rewrite of working code merely to match new names.

---

# 1. Product definition

Ion is a **user-owned, provider-neutral coding harness** for the terminal.

The harness owns:

1. model-facing instructions;
2. model-facing capabilities/tools;
3. the agentic loop;
4. translation to heterogeneous model providers;
5. durable session semantics;
6. policy, trust, approval, and capability boundaries;
7. lifecycle and recovery of external work;
8. presentation-neutral runtime state used by TUI, print, JSON/events, ACP, and future adapters.

A frontend is not an agent. MCP is not an agent loop. ACP is not a second runtime. An extension is not allowed to create a second transcript or persistence model.

## 1.1 Core user journey

The product must make this sequence correct:

```text
open/create session
    ↓
accept user intent durably
    ↓
project model context
    ↓
model generation
    ↓
zero or more tool/effect cycles
    ↓
steer / queued operation / approval / cancellation
    ↓
settle operation
    ↓
close process
    ↓
reopen
    ↓
recover / inspect / continue
```

The TUI is a view over this journey, not the journey itself.

## 1.2 North-star properties

Ion SHOULD feel Pi-like in the properties that matter:

- small system prompt;
- small default tool surface;
- model chooses how to work instead of a planner framework choosing for it;
- inspectable and extensible;
- sessions owned locally by the user;
- multiple providers without provider lock-in;
- complexity added only when it earns its keep.

Ion adds a stronger explicit Rust runtime contract for durability, lifecycle, cancellation, and recovery.

---

# 2. Non-goals

The following are not part of Ion's core architecture:

- Planner / Critic / Reviewer / Swarm framework types.
- Workflow-graph or DAG orchestration as the default reasoning model.
- A distributed-agent platform.
- Pi file/protocol compatibility.
- Provider-hosted session state as canonical truth.
- Exactly-once execution claims for arbitrary external effects.
- Deterministic replay of arbitrary Rust futures.
- Event-sourcing every token delta, UI state change, or internal task wakeup.
- A general actor framework.
- A Cordis-style universal reactive plugin/effect environment.
- A second runtime for the TUI, MCP, ACP, extensions, daemon mode, or child agents.
- A sandbox claim without actual OS/process isolation.
- Automatic git rollback as a correctness mechanism.
- Dynamic tool churn every model step.
- Hidden permissive repair of malformed provider/tool data.

---

# 3. Design principles

These are the highest-level invariants. Code and tests should make them obvious.

## P1. The agent loop stays small

The normal loop is approximately:

```text
project context
→ request model
→ if tool calls: execute admitted tools and append results
→ otherwise finish
→ repeat while continuation is required
```

Do not put product-specific planning algorithms into the core loop.

## P2. One writer owns each authoritative mutable state

No `Arc<Mutex<SessionState>>` shared across arbitrary tasks.

A loaded session has one mutation authority: `SessionRuntime`.

External effects run concurrently, but their results become authoritative only when the session owner accepts and commits them.

## P3. A process/task is an incarnation of durable state

A Tokio task is not the session.

`SessionRuntime` may stop because the process exits, crashes, hibernates, or a future daemon unloads it. The durable session survives and can be reconstructed.

This is the key synthesis of OTP-style ownership with Durable-Object-style persistence.

## P4. Accepted intent is durable before acknowledgment

When `submit_if_idle`, `enqueue`, `steer`, an approval decision, or semantic cancellation returns success, the runtime must already contain enough durable state to recover that acceptance after process failure.

Do not acknowledge queue insertion and persist later.

## P5. External effects are not exactly-once

A tool, model request, hook, process, or network call can complete while the process dies before its settlement is durable.

Ion handles this with:

- committed intent before execution;
- idempotency where available;
- safe replay declarations;
- reconciliation where possible;
- explicit `indeterminate` outcomes where neither replay nor reconciliation is safe.

Ion never silently replays a possibly mutating effect.

## P6. Durable semantic state and live presentation state are different domains

Persist semantic facts needed to resume, audit, and reconstruct the session.

Do not persist every streaming delta.

Live events are presentation/observability signals and can be regenerated or resynchronized from a snapshot.

Ion distinguishes three state classes:

- **durable canonical authority**: semantic entries, accepted inbox work,
  total operation state, exact effect intents/settlements, usage, model-step
  records, context manifests, capability snapshots, approvals, child
  lineage, and artifact metadata;
- **durable auxiliary recovery state**: bounded assistant stream frames,
  tool-progress checkpoints, and other explicitly non-authoritative recovery
  aids; these never select a restart point or prove effect completion;
- **ephemeral presentation/runtime state**: live deltas, TUI layout,
  subscribers, provider clients, sockets, task handles, and in-memory
  caches.

## P7. Model-visible context is a projection, not the source of truth

The canonical local session contains semantic entries and context-defining resources. `ContextProjector` derives what a particular model sees at a particular step.

Provider request payloads and provider-hosted state are derived representations.

## P8. Provider state may accelerate; it may not own meaning

Provider response IDs, cache handles, encrypted reasoning blobs, opaque compaction artifacts, and hosted tool handles MAY be retained as optimization artifacts.

The session MUST remain semantically understandable and continuable without dereferencing them wherever the provider API permits a portable path.

No opaque provider artifact may be the sole carrier of a user message, tool result, readable compaction, child-agent task, or other meaning Ion controls.

## P9. Stable context is a correctness-adjacent performance property

Prompt caching changes latency and cost by enough that tool/config/context design must respect prefix stability.

Ion SHOULD keep stable:

- base system sections;
- tool order and definitions;
- project instructions during an operation;
- serialization order;
- model-facing configuration during a model step.

Changes apply at explicit safe boundaries.

## P10. Processes/tasks model runtime properties, not code organization

Create a Tokio task/controller because something owns mutable state, a resource, concurrency, or lifecycle—not because a module exists.

Pure transformations stay plain Rust functions/modules.

## P11. Child agents use the same primitive

A child is another bounded Ion session/runtime, not a special Planner/Critic class.

Capabilities, budget, context handoff, workspace policy, cancellation, persistence, and lineage are explicit.

## P12. Capability acquisition and teardown are structural

Dynamic capabilities from MCP/extensions SHOULD be scoped to an owning service/extension and removed automatically when that owner disappears.

Use Rust ownership/RAII + structured task ownership before inventing a dynamic effect framework.

## P13. Failure is visible

Persistence failure, provider failure, tool failure, approval denial, cancellation, corruption, capability disappearance, and recovery uncertainty are distinct states.

Do not turn them into fake assistant success.

## P14. The codebase itself must be agent-friendly

Favor:

- local reasoning;
- explicit types;
- greppable names;
- shallow module ownership;
- few re-export mazes;
- minimal macros;
- stable package boundaries;
- one obvious validation command;
- deterministic tests.

The architecture should be easy for an agent to reconstruct from source without relying on an LSP or tribal knowledge.

---

# 4. System architecture

## 4.1 Target shape

```text
                              ┌──────────────────────────────┐
                              │            Host              │
                              │ CLI/config/credentials/trust │
                              └──────────────┬───────────────┘
                                             │
                                  ┌──────────▼──────────┐
                                  │       Runtime       │
                                  │ composition +       │
                                  │ session registry    │
                                  └──────┬───────┬──────┘
                                         │       │
                         ┌───────────────┘       └───────────────┐
                         │                                       │
                ┌────────▼────────┐                     ┌────────▼────────┐
                │ SessionRuntime A │                     │ SessionRuntime B │
                │ single writer    │                     │ single writer    │
                └──────┬──────┬────┘                     └─────────────────┘
                       │      │
             pure      │      │ effect intents/outcomes
          transitions  │      ▼
                ┌──────▼────────────┐
                │ OperationMachine  │
                │ + ContextProjector│
                └───────────────────┘
                              
   ┌───────────────────────────────────────────────────────────────┐
   │ Long-lived runtime services                                  │
   │ Provider adapters · SessionStore · ToolCatalog/Policy        │
   │ MCP service · Extension supervisor · observability           │
   └───────────────────────────────────────────────────────────────┘

Frontends:
TUI / print / JSON-events / ACP / future daemon clients
                  │
                  └── SessionHandle / RuntimeHandle only
```

## 4.2 Runtime is not a giant actor

`Runtime` owns process-level composition and session discovery/lifecycle. It SHOULD NOT absorb every session mutation or provider/tool concern.

A terminal invocation will often have only one loaded `SessionRuntime`, but the architecture must not require all sessions to serialize through one global mutation loop.

## 4.3 SessionRuntime is the single-writer boundary

One loaded session gets one `SessionRuntime` task with:

- a bounded mailbox;
- authoritative live semantic/session state;
- the active operation state;
- pending input/approval/cancel state;
- runtime snapshots and event cursor;
- references to immutable capability/config snapshots;
- owned effect tasks for its operation.

It MAY await bounded local persistence operations required to make a state transition durable.

It MUST NOT await slow provider/tool/MCP/subprocess I/O on its mutation line.

## 4.4 OperationMachine is data + transitions, not another actor by default

An operation has a durable state machine, but that alone is not a reason for another Tokio task.

Prefer a reducer-like core:

```text
OperationState + AcceptedInput/EffectOutcome
    → NewOperationState
    + DurableWrites
    + EffectIntents
    + RuntimeEvents
```

The `SessionRuntime` serializes these transitions. `EffectRunner` performs external work.

---

# 5. Ownership table

| Concern | Authoritative owner | Notes |
|---|---|---|
| process lifetime, composition, CLI config, credential resolution | `Host` / `Runtime` | no session truth |
| loaded-session mutable state | `SessionRuntime` | one writer |
| operation state | `SessionRuntime` via `OperationMachine` | durable checkpoints |
| durable semantic session | `SessionStore` | SQLite; semantic, not token deltas |
| provider wire format/quirks | provider adapter | never session authority |
| model-step snapshot | immutable `ModelStepPlan` | frozen once effect starts |
| tool definitions/capability snapshot | `ToolCatalog` + scoped providers | session consumes immutable snapshot |
| action admission/approval | `PolicyEngine` + session state | canonical args first |
| native tool I/O | effect task | no direct session mutation |
| MCP process/connection lifecycle | `McpService` | sessions do not supervise servers |
| extension process lifecycle | `ExtensionSupervisor` | scoped contributions |
| project trust | host/runtime trust service | persisted separately from transcript |
| child session lifecycle | parent session + runtime registry | child owns its own state |
| TUI state | `UiState` reducer | presentation only |
| terminal cells/diffing | Ratatui | presentation only |
| ACP translation | ACP adapter | same session API |
| live tracing/metrics | observability service | not correctness state |

---

# 6. Domain vocabulary

Use these words consistently.

## Session

The user-owned durable semantic history and configuration lineage for one conversational work context.

A session is not a Tokio task, provider conversation ID, terminal tab, or model cache.

## SessionRuntime

The currently loaded in-process owner of one session's mutable live state.

## Operation

The durable unit of accepted work while a session is busy.

For an interactive run, an operation begins with an accepted prompt and includes all automatic continuations needed to return the session to idle: model steps, tool calls, accepted steering, approvals, retries, and automatic compaction. A later prompt is a distinct queued operation, not a continuation of the current operation.

Manual maintenance actions such as an explicit compaction MAY later be represented as other operation kinds.

## ModelStep

One immutable model request attempt boundary: projected context + model/config/tool snapshot → one provider generation result or failure.

Do not use `Turn` for both a durable user operation and a provider call. If the code keeps the word `Turn`, reserve it for a narrowly defined model interaction and document it.

## SessionEntry

An append-only semantic item such as:

- user message;
- completed assistant message;
- tool call/result visible to the model;
- readable compaction summary;
- model/config/instruction/capability change that affects semantic reconstruction;
- child delegation/result reference;
- custom extension semantic entry where explicitly permitted.

Streaming text deltas are not entries.

## InboxItem

Durably accepted input that has not yet been applied to model-visible session history.

Kinds initially:

- `Prompt` — the root input for one accepted operation;
- `Steer` — joins the active operation and is applied at the next safe continuation boundary.

## Effect

Work outside pure state calculation whose result arrives later:

- provider request;
- tool call;
- approval wait;
- timer/retry wait;
- subprocess/MCP call;
- durable store transaction is also an effect at the interpreter boundary, although it is the commit mechanism for state transitions.

## EffectIntent

The durable fact that Ion is allowed/required to perform one repeat-sensitive external effect with exact effective inputs.

## CapabilitySnapshot

The immutable set of tools/capabilities available to one model step. Each
provider-facing tool name is paired with an internal capability identity and
generation. A call is resolved against the snapshot that produced it; a
replacement with the same public name is not an implicit rebind.

## ContextManifest

The stable model-facing resources that define the prefix/configuration for a span of steps: ordered system sections, trusted instruction material, and tool schemas.

## ContextPlan

The exact semantic projection for one model step: manifest + selected session entries/compaction baseline + model settings.

---

# 7. Runtime and task model: OTP ideas translated to Rust

Ion should borrow OTP semantics, not recreate BEAM.

## 7.1 When to create a task

A dedicated task is justified when a component owns one or more runtime properties:

- mutable authoritative state;
- a shared/external resource;
- concurrency;
- initialization/shutdown/restart lifecycle;
- timers/monitoring.

Do not hide pure calculations behind channels.

## 7.2 Rust primitives

Default toolkit:

- bounded `tokio::mpsc` for owned mailboxes;
- `oneshot` for request/acknowledgment;
- `CancellationToken` for hierarchical cooperative cancellation;
- `JoinSet` and/or `TaskTracker` for owned task drainage;
- explicit state enums for state machines;
- RAII guards/scopes for capability/resource registration;
- `tracing` spans for observability.

Do not add an actor crate until a concrete missing semantic appears.

## 7.3 Supervision semantics

Ion needs supervision *policies*, even if it has no generic supervisor framework.

Long-lived services declare behavior roughly equivalent to:

```text
restart: never | transient | bounded
shutdown: graceful_timeout + force
failure_scope: service | session | runtime
backoff: bounded exponential when applicable
```

Examples:

| Component | Restart default | Failure scope |
|---|---|---|
| provider request | operation retry policy | operation |
| read-only tool call | operation replay policy | tool call |
| mutating bash call | never implicit | effect/operation uncertainty |
| MCP server | bounded transient restart | MCP service |
| extension process | bounded/configured | extension |
| SessionRuntime panic | reconstruct session, do not continue corrupted state | session |
| SessionStore fatal error | stop affected runtime; no fake success | runtime/session |

Repeated crash loops MUST trip a restart limit/circuit rather than respawn forever.

## 7.4 Panics are not normal failure

Expected provider/tool/policy/persistence errors use typed `Result` values.

A panic is an invariant failure or bug. A panicked child task is reported as abnormal termination. Do not implement “let it crash” as “panic for control flow.”

---

# 8. Public command model

## 8.1 Handles

Target API shape:

```text
RuntimeHandle
  create_session(...)
  open_session(...)
  list_sessions(...)
  shutdown(...)

SessionHandle
  submit(...)
  steer(...)
  enqueue(...)
  switch_model(...)
  cancel(...)
  approve(...)
  reject(...)
  snapshot(...)
  subscribe(...)
  close(...)
```

The exact Rust signatures are implementation work. Preserve narrow typed surfaces.

For one-shot CLI convenience, `RuntimeHandle` MAY delegate directly to its sole session, but the core ownership model remains session-scoped.

## 8.2 Call semantics

Commands with correctness significance use request/reply semantics.

Example:

```text
SessionHandle.submit_if_idle(message)
    ↓ mailbox
SessionRuntime validates admission
    ↓
SQLite transaction commits InboxItem + Operation acceptance
    ↓
SessionRuntime updates live state
    ↓
reply Accepted { operation_id }
```

Success means accepted durably, not merely queued in RAM.

A model-selection command is correctness-significant. The host supplies
an initial default, but `SessionRuntime` owns the accepted per-session
selection. `switch_model` appends a semantic configuration entry before
acknowledgment. It never mutates an in-flight `ModelStep`; the new
selection applies to the next step started after the commit.

## 8.3 Bounded mailboxes

All runtime channels are bounded.

Never silently drop:

- user input;
- steer/queued operation;
- cancellation;
- approval decision;
- effect outcome;
- persistence outcome.

A full correctness channel applies backpressure or returns a visible overload error.

---

# 9. Input, steering, queued operations, and cancellation

## 9.1 Prompt

When idle, an accepted prompt:

1. creates a durable `Operation`;
2. creates/accepts the root `InboxItem`;
3. commits the initial total operation state;
4. only then acknowledges the caller.

The operation consumes the prompt into a user `SessionEntry` atomically with the state transition that removes it from pending input.

## 9.2 Steer

`steer` modifies the current operation's next reasoning boundary without mutating an already-started provider request.

Rules:

- it is durable on acknowledgment;
- it is ordered relative to other accepted inbox items;
- it is applied at the next documented safe checkpoint;
- v0 does not attempt magical mid-token provider injection;
- if a provider later supports a clean native steering primitive, the adapter may optimize without changing semantic ordering.

## 9.3 Queued operation

`enqueue` always creates one distinct durable operation. If an operation is
active, the new operation stays in `Accepted` state until the current
operation reaches a terminal outcome; promotion follows acceptance order.
If the session is idle, `enqueue` may start immediately. `submit_if_idle`
rejects when any active or queued operation exists.

`steer` is the only active-operation inbox path. It modifies the current
operation's next safe reasoning boundary and never creates a queued
operation.

## 9.4 Cancellation

Cancellation is semantic state, not just `CancellationToken.cancel()`.

Order:

1. persist `cancel_requested` / cancellation transition;
2. acknowledge cancellation intent;
3. signal descendant effect tasks;
4. settle/reconcile any in-flight effect;
5. finish operation as `cancelled` or `indeterminate` if an external mutating effect cannot be classified safely.

Parent cancellation descends. Child cancellation does not cancel siblings or parent.

## 9.5 Close is not cancel

Process/session-runtime close means lifecycle shutdown:

- stop accepting new work;
- cooperatively stop in-process effects;
- drain owned tasks and required store writes;
- restore terminal state;
- release process-local ownership.

It MUST NOT silently write a user cancellation outcome merely because the process is closing.

An open durable operation may remain recoverable on restart.

---

# 10. Operation state machine

## 10.1 Requirement

Every accepted operation has an explicit, total durable state. Recovery reads the latest state; it does not infer correctness by guessing across unrelated transcript rows.

A practical initial state vocabulary may look like:

```text
Accepted
NeedAssistant
AssistantEffectPending
AssistantRetryWait
ToolsPlanned
ApprovalPending
ToolEffectPending
NeedContinuation
CompactionPending
Suspended
Finishing
Finished { completed | failed | cancelled | declined | indeterminate }
```

A checkpoint MUST carry everything needed to rebuild the live machine on
reopen: the total state, the current model-step capability snapshot, the
operation prompt, pending inbox ownership, and any pending effect intent.
The snapshot is refreshed at each safe model-step boundary; it is not an
operation-wide freeze.
`Suspended` is recoverable state, not a black hole: reopening rebuilds
the operation from its checkpoint and surfaces it; what resuming it
means is recovery policy (Step 3).

The final Rust enum can refine this. Do not create states without distinct recovery semantics.

## 10.2 Transition rule

A durable transition is one transaction that writes all semantic consequences that must agree.

Example:

```text
ToolEffectPending + ToolOutcome
    TX:
      append tool result SessionEntry
      settle Effect
      append next total OperationStateRecord
      append usage/diagnostic records if any
```

A crash cannot expose a tool result without advancing the operation, or advance the operation while losing the corresponding result.

## 10.3 Model-step flow

Typical run:

```text
Accepted
  → apply prompt entry
NeedAssistant
  → build immutable ContextPlan
  → persist provider EffectIntent
AssistantEffectPending
  → spawn provider effect
  → stream ephemeral draft events
  → provider completes
  → atomically commit completed assistant semantic output + usage + next state
      ├─ no tool calls → checkpoint / finish
      └─ tool calls    → ToolsPlanned
```

Partial assistant output is never a completed assistant entry. Ion may persist
bounded auxiliary assistant frames keyed to the pending effect so a reopened
session can show the interrupted draft. Frames do not prove provider
completion, do not enter normal model context, and are deleted transactionally
with the settled effect.

## 10.4 Tool flow

```text
ToolsPlanned
  → canonicalize tool + effective args
  → policy decision
      ├─ deny → durable model-visible denial result
      ├─ approval → ApprovalPending
      └─ allow
  → persist Tool EffectIntent with exact effective args
ToolEffectPending
  → execute tool
  → atomically settle outcome + semantic tool result + next state
```

The executor must receive the exact effective args that policy approved.

## 10.5 Automatic retries

Retry behavior is explicit and attempt-counted.

Provider requests SHOULD auto-retry transient failures only when doing so cannot confuse the user or duplicate external semantic state. Conservative initial policy:

- before any model-visible delta: transient retry is allowed;
- after visible streaming output: fail or restart only through an explicit attempt-reset event; do not silently splice a new generation onto old text;
- stateful provider APIs require explicit adapter semantics/idempotency before retry.

Tool retries follow their recovery class, not generic exponential retry middleware.

---

# 11. Persistence model

## 11.1 SQLite is the canonical local store

Use one SQLite database per Ion data root, not one file per message/turn.

Initial recommendation:

- `rusqlite`;
- one dedicated blocking store thread/task owning the write connection;
- WAL mode;
- foreign keys enabled;
- busy timeout;
- `synchronous=FULL` initially for strong accepted-intent durability; benchmark before weakening it;
- explicit schema version gate (see §33.12);
- no async connection pool until measured need exists.

Why `rusqlite` over `sqlx` initially: Ion has a local single-writer durability problem, not a high-concurrency web-database problem. Direct transaction control and a simple DB thread are more valuable than async pool machinery.

## 11.2 Durable identifiers

Use UUIDv7 for durable externally meaningful IDs:

- session;
- operation;
- effect;
- entry;
- child relationship;
- approval where separately addressed.

Do not use UUID ordering as semantic ordering.

Each session has a storage-assigned monotonic integer `seq` for durable ordering.

Live runtime events use a separate in-memory cursor.

## 11.3 Core schema

The exact SQL may evolve, but preserve these logical tables/contracts.

### `sessions`

```text
id UUIDv7 PK
created_at
updated_at
project/cwd metadata
parent_session_id nullable
fork/context seed metadata nullable
status/labels/title
```

No provider credential secrets.

### `entries`

```text
id UUIDv7 PK
session_id
seq UNIQUE(session_id, seq)
kind
payload
created_at
```

Append-only semantic history.

### `operations`

Immutable operation identity/basic metadata:

```text
id
session_id
kind
accepted_at
root_inbox_id
```

### `operation_state`

One replaceable **total** checkpoint per operation:

```text
operation_id
state_seq
state_kind
state_payload
created_at
PRIMARY KEY(operation_id)
```

Each row is enough to know exactly what may happen next. `state_seq` is a
diagnostic transition counter, not a second history. Do not store a chain of
tiny patches that must be replayed correctly to infer recovery state; entries
and effects retain the semantic and external-effect history.

### `inbox_items`

```text
id
session_id
operation_id
kind: prompt | steer
accepted_seq/order
payload
status: pending | applied | rejected
```

Application/removal and corresponding session entry/state transition occur atomically.

### `effects`

```text
id
operation_id
kind
attempt
recovery_class
status: pending | settled | indeterminate
idempotency_key nullable
effective_input
settlement
started_at/settled_at
```

The row or equivalent durable record is committed before repeat-sensitive execution begins.

### `model_steps`

Persistent metadata for inspection/portability without duplicating the full prompt:

```text
id
operation_id
model_ref
context_manifest_id
projection boundary/compaction baseline
context fingerprint
provider settings relevant to semantics
usage/cache/latency metadata
provider artifact refs nullable
```

### `assistant_frames`

Bounded auxiliary recovery output for an unsettled assistant effect:

```text
effect_id
session_id
operation_id
step
frame_seq
text/thinking bounded draft
updated_at
```

Frames are ordered snapshots, not a delta journal. They are never treated as
completion evidence and are removed in the same transaction that settles the
owning model effect.

### `tool_progress`

Bounded auxiliary checkpoints for long-running tools use the same shape:

```text
effect_id
session_id
operation_id
call_id
output tail/preview
updated_at
```

A tool controls its checkpoint cadence (the initial bash cadence is about two
seconds). Progress is drained through the runtime's bounded tool channel,
never treated as settlement, and deleted with the consuming tool effect.

### `context_manifests` / resources

Store context-defining material only when it changes, preferably content-addressed:

- ordered system sections/instructions;
- active tool specs/schemas;
- semantic configuration needed to reconstruct what the model was told.

A model step references the manifest rather than storing a full duplicated prompt.

This is especially important once extensions/MCP/skills make the capability set dynamic.

### `usage`

Append-only model/tool usage and cache metadata sufficient for totals and diagnostics.

## 11.4 What is not persisted

Do not persist as correctness state:

- every text delta (bounded assistant frames are auxiliary recovery state);
- spinner/frame/UI state;
- terminal cells;
- every internal mailbox wakeup;
- arbitrary provider SDK objects;
- raw Tokio task state.

## 11.5 No writer leases yet

Local v0 has one process writing its DB. Do not copy Pi's SQLite writer-lease/fencing machinery before Ion has a real multi-writer requirement.

If a future daemon becomes the sole store owner, clients route through it.

Add lease/fence semantics only if independent processes genuinely need competing write ownership.

---

# 12. Durable effect semantics and recovery

## 12.1 Fundamental protocol

For each repeat-sensitive external effect:

```text
1. derive exact effective input
2. commit EffectIntent + operation state saying effect is pending
3. start effect
4. receive outcome
5. atomically commit settlement + semantic consequences + next operation state
```

The hard crash window is between steps 3 and 5. Recovery policy exists for that window.

## 12.2 Recovery classes

Initial semantic classes:

### `ReplaySafe`

Repeating the effect cannot create an unacceptable duplicate external mutation.

Examples:

- read file;
- search/find;
- stat/list;
- provider generation when Ion supplied canonical local context and provider-side state is not mutated in a way Ion depends on.

Duplicate billing or latency can still occur and SHOULD be observable.

### `Reconcile`

The effect may mutate state, but Ion can inspect postconditions before deciding whether to execute again.

Examples:

- deterministic file write/edit with stored pre/post hashes;
- an API call with a queryable idempotency key/status endpoint.

### `NeverReplay`

Ion cannot safely know whether repeating the effect duplicates an external mutation.

Examples:

- arbitrary `bash` command;
- unknown MCP/extension tools without declared semantics;
- sending an external message without an idempotency contract.

An unresolved pending effect of this class becomes `indeterminate` after process loss. The user/agent must inspect and decide what to do next.

## 12.3 File-write reconciliation

For `write`/`edit`, persist enough evidence before execution to classify recovery:

- target path after canonicalization;
- preimage existence/hash where applicable;
- intended postimage hash or deterministic patch/result hash.

Recovery:

```text
current == intended_postimage  → effect completed; settle without repeating
current == recorded_preimage   → safe to execute intended write
otherwise                      → conflict / indeterminate; do not overwrite
```

Prefer temp-write + atomic rename where platform semantics permit.

## 12.4 Bash

`bash` remains `NeverReplay` by default.

Killing its process group on cancellation is necessary but does not prove that earlier filesystem/network side effects were rolled back.

After a crash with a started-but-unsettled bash call, Ion must not rerun it automatically.

## 12.5 Provider calls

Provider generation is special:

- local semantic state remains canonical;
- opaque provider response/conversation state is optional;
- where an API provides request idempotency/resume semantics, adapter may use them;
- otherwise an unsettled generation may be semantically replayable but can duplicate billing;
- if a stateful provider endpoint creates hidden state Ion cannot reconstruct, mark that limitation explicitly and preserve a portable local fallback when possible.

Never claim exactly-once provider billing or hidden-state mutation.

---

# 13. Session portability and provider neutrality

## 13.1 Practical portability test

A session export should contain enough intelligible state that a different capable provider can understand what happened and continue, even though it will not reproduce identical hidden reasoning or next tokens.

Ion should be able to answer:

- What did the user ask?
- What completed assistant content was produced?
- Which tools were available and called?
- What exact model-visible tool results were returned?
- What instructions/context changes mattered?
- What readable compaction replaced older context?
- What child was delegated which readable objective?
- Which actions were approved/denied?
- Which external effects may be indeterminate?

## 13.2 Provider artifacts

Provider-specific artifacts MAY be stored:

```text
ProviderArtifact {
    provider
    kind
    opaque bytes/id
    portability: optimization_only | required_by_provider_feature
}
```

They must never replace local readable semantic state when Ion has control over that state.

## 13.3 Thin provider adapters, not fake uniformity

The core needs normalized semantic types, but adapters must preserve important provider differences explicitly.

Do not force every provider through a lowest-common-denominator abstraction that hides:

- tool-schema restrictions;
- reasoning controls;
- cache controls;
- deferred/additive tool definitions;
- provider-hosted state;
- usage semantics;
- streaming event ordering;
- context limits;
- native compaction/resume support.

Expose capabilities as typed flags/data and branch deliberately at the adapter/context-planning layer.

---

# 14. Context architecture

## 14.1 ContextProjector

`ContextProjector` is a deterministic transformation from local semantic state to a model-neutral `ContextPlan`.

Conceptually:

```text
Session semantic entries
+ readable compaction state
+ trusted instruction resources
+ CapabilitySnapshot
+ model/config snapshot
    ↓
ContextPlan
    ↓ provider adapter
provider-native request
```

The projector itself should be mostly pure code.

## 14.2 Freeze each model step

Once a provider effect starts, the step's:

- model;
- reasoning level;
- system sections;
- tool set;
- projected messages;
- provider-relevant semantic settings

are immutable.

Changes accepted while the request is running apply to a later safe boundary.

## 14.3 ContextManifest

Model-facing resources that change infrequently are grouped into a content-addressed/stable manifest.

Why:

- inspectability;
- session portability;
- deterministic projection;
- cache diagnostics;
- extension/MCP unload does not erase what a prior model saw.

Do not build a general reactive context graph. A stable manifest plus explicit boundary changes is enough initially.

## 14.4 Prompt-cache discipline

Stable-prefix rules:

- deterministic system-section ordering;
- deterministic tool ordering/schema serialization;
- no timestamps/random values in early prompt sections;
- do not rewrite old messages merely to reduce token count;
- avoid adding/removing/reordering tools every turn;
- make compaction an explicit cache reset;
- record provider cache-read/cache-write metrics when available. This
  is required, not optional: the compaction safety net (14.7.3)
  computes `context_tokens` from full usage including cache reads.

Tool/config changes that invalidate the prefix should be observable as such.

## 14.5 Skills

Prefer skills/instructions over permanent tool proliferation when a capability can be expressed as guidance over existing primitives.

A skill should be:

- discoverable cheaply;
- loaded only when relevant;
- represented as readable model-facing content;
- auditable in the session/context manifest if activated;
- trust-gated when project-local.

Do not ambient-load a large skills corpus into every prompt.

## 14.6 Dynamic instructions

Avoid silently rewriting the system prompt mid-operation.

If project/extension/user instruction content changes:

1. accept the change explicitly;
2. create a new context manifest/version at a safe boundary;
3. record enough readable content/lineage for later reconstruction;
4. apply only to future model steps.

## 14.7 Compaction

Compaction is lossy projection maintenance, not deletion of canonical history.

Persist a readable semantic entry such as:

```text
CompactionEntry {
    covers_through_seq,
    summary,
    summarization_instructions/version,
    optional source metadata
}
```

Original entries remain durable.

The projector uses the summary + suffix for future model context.

Provider-native opaque compaction MAY be retained as an optimization only if there is also a readable portable representation.

Do not aggressively prune arbitrary middle history by default. It harms both evidence and prompt-cache reuse.

### 14.7.1 Who decides to compact

The initial Ion baseline keeps compaction harness-owned and predictable:

1. an explicit user request at a safe continuation boundary;
2. the harness pressure threshold, relative to the selected model's actual
   context window;
3. one bounded overflow recovery when the provider rejects an oversized
   request.

The baseline does not expose a model-directed `compact` tool or synthetic
context-usage hints. A future extension/provider integration MAY add model-aware or
provider-native compaction only when it retains a readable local summary and
does not become the sole semantic source.

### 14.7.2 Explicit compaction

`SessionHandle.compact(instructions?)` records a maintenance request while an
operation is active. The harness consumes it at the next continuation
boundary, commits a ReplaySafe compaction effect, and persists a readable
summary entry. The request does not create a hidden recovery turn; ordinary
continuation proceeds from the summary baseline.

### 14.7.3 Safety-net automatic compaction

At continuation boundaries only (run settled, before the next model
step — compacting mid-run buys nothing), the harness compacts when:

```text
context_tokens > context_window − reserve_tokens
```

- `context_tokens` comes from the usage ledger's most recent settled
  step: input + output + cache_read + cache_write (14.4), plus an
  estimate for trailing content newer than the last usage report.
- `reserve_tokens` defaults to 16k (summary prompt + output headroom)
  and is configurable.
- Requires a known context window from model metadata (15.x). When the
  window is unknown, the safety net is disabled and overflow recovery
  (14.7.4) is the backstop.
- The re-compaction guard stands: never compact twice in a row on the
  same boundary.

### 14.7.4 Overflow recovery

When the provider rejects a request because the context exceeded the
window, the harness MAY run one compaction and retry the step once.
This is the only path that may discard the failed attempt's partial
request state, because the attempt produced no durable effect beyond
the already-committed intent. Repeated overflow after a compaction
retry fails the operation visibly; it must not loop.

---

### 14.8 Model metadata and selection

Provider adapters expose per-model metadata, minimally the context
window size, as part of the adapter contract (15.x). The compaction
harness compaction (14.7.3) consumes it; unknown windows disable the
safety net and leave overflow recovery as the backstop. Adapters MUST NOT
guess a window.

The host-selected launch model is only an initial default. Once a
session exists, `SessionRuntime` is the sole authority for the selected
model:

- the initial model reference is durable before the first model effect;
- a mid-session change is an append-only semantic configuration entry,
  committed before `switch_model` acknowledges;
- an in-flight step keeps its frozen model/config snapshot; a committed
  change applies only to later steps;
- every provider attempt persists its exact `model_ref`, semantic
  provider settings, and metadata snapshot in the model-step record and
  effect input before execution;
- recovery reuses that exact persisted snapshot. If the provider,
  credentials, or model are unavailable, recovery fails visibly; it
  never substitutes the current launch default;
- child sessions persist their own initial selection. Parent changes do
  not mutate existing children.

Provider construction and credentials remain host composition. A
provider resolver may cache providers by model id, but it is not the
selection authority. It selects from the immutable request, releases any
cache lock before metadata or generation I/O, and never holds a lock for
a whole model step.

Changing models resets model-relative derived compaction metadata.
A context window cached for one model MUST NOT be reused for another.

---

# 15. Provider engine

## 15.1 Preserve normalized streaming, refine semantics

The current `EngineSignal` idea is good. The redesign should converge on a provider-neutral stream vocabulary such as:

```text
TextDelta
ReasoningSummaryDelta (if provider exposes it and policy allows)
ToolCallDelta / ToolCallCompleted
UsageUpdate
Completed(ModelResponse)
Failed(ProviderError)
Cancelled
ProviderExited
```

Core rule: a provider stream becomes durable semantic assistant content only at a validated completion boundary.

## 15.2 Tool calls

Do not execute a tool from partial streamed JSON.

Wait for one complete provider-native tool call, normalize and validate it, then pass it through policy/effect admission.

## 15.3 Strict validation

Provider adapters may implement documented provider-specific normalization, but the core must not silently ignore malformed unknown fields or invent missing required arguments merely to keep the model moving.

Malformed tool calls should be visible and bounded so a model cannot loop forever emitting invalid structures.

## 15.4 Retry

Retries are part of operation semantics, not hidden SDK middleware.

Persist attempt count where it affects recovery/diagnostics.

---

# 16. Tools

## 16.1 One tool contract

Native, MCP, and extension tools ultimately enter the same invocation/policy/effect path.

Keep the current object-safe `Tool` direction.

A normalized invocation should include at least:

```text
ToolInvocation {
    call_id
    tool_identity/version
    canonical_effective_args
    cwd/workspace identity
    capability source
    recovery metadata
}
```

## 16.2 Small default surface

Keep the default tools small:

- read;
- write;
- edit;
- bash;
- search;
- find/list.

Do not add tools merely because another agent has them. A CLI command or skill over `bash` is often cheaper and more general.

## 16.3 Sequential execution first

Execute model-requested tool calls sequentially until a measured use case justifies parallel execution.

Parallel tools require explicit answers for:

- read/write conflicts;
- ordering of model-visible results;
- cancellation;
- approvals;
- effect reconciliation;
- filesystem arbitration.

Do not pay that complexity preemptively.

## 16.4 Model-visible result vs artifact

Persist exactly what the model saw as the semantic tool result.

Large/full output MAY live as an artifact referenced by the result. This keeps context bounded without making the session unauditable.

## 16.5 Error semantics

Expected tool-level failures—nonzero command exit, file not found, test failure—are usually model-visible tool outcomes so the model can react.

Harness failures—corrupt state, lost persistence, impossible invariant—are runtime/operation failures and must not be disguised as ordinary tool text.

---

# 17. Policy, trust, and approvals

## 17.1 Separate trust from approval

**Project trust** answers whether project-local instructions/config/executable extensions/MCP definitions may influence the harness.

**Action approval** answers whether one concrete effective action may execute.

Do not conflate them.

## 17.2 Project trust

Project-local:

- instructions;
- skills;
- prompts;
- MCP configuration;
- executable extension configuration

must pass an explicit persisted trust decision before being activated.

Retrieved/model-produced text cannot grant trust.

Non-interactive operation fails closed when trust or confirmation is required unless the caller supplied an explicit trusted policy through a documented mechanism.

## 17.3 Canonicalize before approval

Policy sees the same effective input the executor will use:

```text
raw model args
→ schema validation
→ path/cwd/env/default normalization
→ canonical ToolInvocation
→ PolicyEngine
→ approval if needed
→ persist exact invocation
→ executor
```

Never approve one string and execute a materially different resolved path/command.

## 17.4 Approval result

Approval/rejection is durable.

Interactive rejection generally becomes a model-visible tool denial so the model can choose another path.

In non-interactive mode, an approval-required action SHOULD terminate/park the operation with a clear `ApprovalRequired` error rather than invite an endless model retry loop.

---

# 18. Capability lifecycle: Rust-native scoped composition

Ion should borrow the useful lifecycle property from Cordis without adopting Cordis wholesale.

## 18.1 Scoped registrations

Conceptual API:

```text
ExtensionScope / McpServerScope / SessionScope
    register_tool(...)
    register_command(...)
    publish_capability(...)
    spawn_owned_task(...)
```

Everything registered through a scope is owned by that scope.

When the owner stops/unloads:

- future capability snapshots no longer include its tools;
- subscriptions are removed;
- owned tasks are cancelled/drained;
- downstream resources are torn down in dependency order.

RAII guards and explicit owned collections should implement this structurally.

## 18.2 Immutable snapshots for operations

A live registry may change, but a model step sees an immutable `CapabilitySnapshot`.
The snapshot is created and persisted at each model-step boundary, not once
for the whole operation.

An extension/MCP server disappearing cannot mutate an already-started provider request.
Each snapshot records a stable internal identity and generation. A planned
call executes only when that identity is still the current capability; a
missing or replaced generation produces a visible capability-loss/tool
failure rather than dispatch by public-name coincidence.

If a capability disappears before its planned call executes, the operation receives a visible capability-loss/tool failure and continues or fails according to policy.

## 18.3 No reactive coeffect graph initially

Do not implement arbitrary runtime dependency reactivity, waterfalls, or invertible effects for every state mutation.

Use explicit service dependencies and scoped ownership. Add more dynamic composition only when a concrete extension requirement proves necessary.

---

# 19. MCP

MCP is a capability transport/provider, not an agent runtime.

## 19.1 Ownership

`McpService` starts configured peers. The `ToolCatalog` owns the spawned
supervisors for its lifetime and drains them on close; the peer monitor owns
transport shutdown and is joined with a bounded deadline.

The capability service owns:

- server definitions;
- process/transport lifecycle;
- protocol negotiation;
- reconnect/backoff/restart limits;
- published tool/resource capability descriptors;
- server health.

A session does not directly supervise MCP processes.

## 19.2 Session integration

At a safe context boundary:

1. `McpService` publishes current available tool descriptors;
2. `ToolCatalog` forms a new immutable capability snapshot;
3. context manifest changes if model-facing tools changed;
4. future tool calls go through normal Ion policy/effect recovery.

## 19.3 Cache discipline

Do not automatically inject every MCP tool from every configured server into every model request.

Prefer:

- explicitly active small sets;
- skills/CLI wrappers for broad service APIs;
- provider-native additive/deferred tool loading only when it preserves prefix stability and semantics.

## 19.4 Recheck protocol versions at implementation

MCP evolves. Do not let today's SDK/version details shape core storage or operation semantics.

---

# 20. Child agents / bounded delegation

## 20.1 Same primitive

A child is a durable Ion session with explicit parent lineage.

Do not create separate child-agent runtime code.

## 20.2 ChildSpec

Conceptually:

```text
ChildSpec {
    objective
    context_seed
    capability_policy
    model_override?
    budget
    workspace_mode
}
```

## 20.3 Context handoff is explicit

Default child context should not be an implicit lossy copy of “whatever the parent currently knows.”

Support explicit modes such as:

- `Fresh`: base + trusted project context + explicit objective/resources;
- `ForkContext`: deliberate projection of parent semantic context through a known boundary.

Persist the readable child objective and lineage before launch.

The parent receives a compact result plus child session ID/reference. The full child transcript remains inspectable but is not injected automatically into the parent.

## 20.4 Capabilities

A trusted child may inherit parent capabilities, but the child's capability set may only equal or narrow what the parent is allowed to delegate unless a separate user policy explicitly grants more.

Research children SHOULD be easy to run read-only.

Write-enabled concurrent children require explicit workspace arbitration. Future worktree isolation is a good option; it is not required for the initial child primitive.

## 20.5 Budgets

Runtime-enforced budget dimensions MAY include:

- max active children;
- max nesting depth;
- token/cost budget;
- model-step count;
- wall-clock deadline;
- tool-call count.

Keep exact numeric defaults configuration/policy, not architecture. Start conservative and benchmark.

Nested child creation SHOULD be disabled or depth-bounded initially.

## 20.6 Cancellation

Parent operation cancellation cancels descendants.

Child cancellation never implicitly cancels parent or siblings.

A child crash/restart uses its own durable session recovery.

---

# 21. Frontends and live event model

## 21.1 One runtime contract

TUI, print mode, JSON/events, ACP, and a future daemon protocol all consume the same `SessionHandle` semantics.

No frontend writes the SQLite transcript directly.

## 21.2 Snapshot + live events

A subscriber gets:

```text
SessionSnapshot {
    durable semantic view
    active operation summary
    current ephemeral assistant draft/tool statuses
    event_cursor
    runtime_instance_id
}
```

then bounded live `RuntimeEvent`s.

If a consumer falls behind or reconnects, it requests a new snapshot. Do not require persistence/replay of every live event.

## 21.3 Event domains

Keep at least these conceptual domains distinct:

### Durable semantic

`SessionEntry`, operation state/effect records, approvals, usage.

### Live execution/presentation

```text
OperationStarted
AssistantDraftStarted
AssistantTextDelta
ReasoningDelta
AssistantDraftReset
ToolStarted
ToolProgress
ToolSettled
ApprovalRequested
OperationFinished
OperationFailed
OperationCancelled
RuntimeWarning
```

### Observability

Tracing spans, timing, provider wire diagnostics, cache hit/miss metrics.

An observability event does not become model context just because it exists.

Provider-reported reasoning is a live-only presentation surface. Adapters
MUST distinguish raw reasoning from a provider-produced reasoning summary
when the wire protocol distinguishes them. Exposure is controlled by one
host policy shared by frontends; a TUI visibility toggle is not authority
to leak reasoning through ACP. Reasoning never becomes assistant content
or a durable semantic entry, and every terminal/reset path clears its
live buffer.

`ToolSettled` MAY carry a derived tail preview for presentation after the
durable settlement commits. The full semantic tool result remains
canonical. Preview line and byte limits are hard total bounds, including
one-line and multibyte output plus any truncation marker.

## 21.4 Backpressure

Critical lifecycle/approval/error events are not silently dropped.

High-frequency deltas MAY be coalesced when the snapshot contains the authoritative current draft. A slow UI must not create an unbounded memory queue that can destabilize the agent runtime.

Lag notification itself must be reliable: a full event queue cannot be
used to enqueue its own overflow signal. Detach/closure is an acceptable
signal only when subscribers classify it as lag and resubscribe. The
fresh snapshot includes the runtime instance id and enough current draft
and tool status to reconstruct the live view; display-only reasoning MAY
be discarded explicitly.

---

# 22. TUI architecture

Ion renders its TUI through its own line-diff screen layer over crossterm:
committed scrollback and the live composer/footer region are one growing
line array; each frame diffs against the previous and writes changed rows.
This mirrors the pi-tui model proven as a daily driver. Ion owns application
state/update semantics.

## 22.1 One UI state owner

Use one top-level `UiState` / reducer-style update path.

Widgets/components receive state and emit UI intents. They do not own independent agent runtimes or hidden durable state.

Conceptually:

```text
RuntimeEvent | KeyEvent | Resize | Tick
        ↓
update(UiState, UiMessage)
        ↓
new UiState + UiEffect
        ↓
render(UiState)
```

## 22.2 Runtime interaction

UI effects call `SessionHandle`; responses return as messages/events.
Model changes follow this path; a frontend never owns or swaps the
provider directly.

The UI never blocks rendering on provider/tool/metadata I/O. Cache locks
are not step-boundary synchronization and MUST NOT span provider futures.

## 22.3 Inline-first

Ion SHOULD support the Pi-like inline terminal experience as a first-class mode: completed transcript remains useful terminal scrollback while the live composer/status area redraws efficiently.

Use the line-diff screen (§22 intro). The screen layer owns, as an
acceptance contract:

- **Origin ownership**: the host reserves the window's rows before the
  first draw (`reserve_rows`); the window occupies the bottom rows of
  the physical screen and never overwrites content above the launch
  point.
- **Committed-vs-mutable scrolling**: physical scroll happens only when
  committed history advances. The live band is fixed-height, so
  reversible edits (composer wrapping, preview toggles) redraw in place
  and can never leak rows into terminal scrollback.
- **Invalidation**: resize or any loss of previous-window state repaints
  every row from the fresh buffer; freed rows are erased, never left as
  ghosts.
- **Cursor translation**: the hardware cursor maps absolute wrapped row
  -> visible row by subtracting the window offset; cursors outside the
  window are hidden, never mispositioned.
- **Width model**: wrapping, cursor mapping, and cell output use the
  same display-width model; rows are diffed at row granularity so wide
  characters and grapheme clusters always render from complete rows.

## 22.4 Terminal restoration

Terminal raw mode/mouse/paste/keyboard protocol changes use an RAII guard and a final panic/error restoration path.

Runtime shutdown and terminal restoration have one owner each; do not scatter cleanup across widgets.

---

# 23. ACP

ACP is a frontend/session-control adapter.

It maps protocol operations to `RuntimeHandle` / `SessionHandle`:

- initialize/open/resume;
- prompt/steer/cancel;
- receive content/tool/status events;
- permission requests/responses;
- session metadata.

ACP MUST NOT introduce:

- a second transcript;
- ACP-specific agent state machine;
- ACP-specific persistence semantics;
- direct tool execution bypassing Ion policy.

Protocol-specific data stays at the adapter boundary.

Recheck the current ACP schema/version when implementation begins.

---

# 24. Extensions

## 24.1 Goal

Ion should preserve Pi's important property: the harness can be extended without bloating its default surface, including extensions authored by an agent.

## 24.2 Initial transport

Subprocess extensions first.

Prefer a language-neutral stdio RPC/event protocol with a small manifest. The executable may be Rust, TypeScript, Python, etc.

WASM Component Model is deferred until a concrete requirement justifies the lifecycle/tooling cost.

## 24.3 Contribution types

Start closed and explicit:

- tools;
- user commands;
- skills/instruction resources;
- presentation metadata/observability events where safe.

Do not begin with arbitrary hooks that can intercept every critical persistence/state-machine transition.

If later extension hooks are justified, define named boundaries with clear replay/durability semantics.

## 24.4 Scope/lifecycle

Each extension gets an `ExtensionScope` owning:

- registrations;
- subscriptions;
- subprocess;
- owned tasks;
- capability snapshot contributions.

Unload tears down the scope structurally.

## 24.5 Self-extension

Ion SHOULD make it easy for the agent to:

1. inspect extension documentation/template;
2. write an extension into a user/project-approved location;
3. run tests/validation;
4. request an explicit reload/activation boundary.

Executable project extensions remain trust-gated. Do not hot-reload arbitrary newly written code in the middle of a model step.

---

# 25. Shutdown and lifecycle

## 25.1 Hierarchy

Conceptual cancellation/lifecycle tree:

```text
Process
└── Runtime
    ├── SessionRuntime
    │   └── Operation
    │       ├── ProviderEffect
    │       └── ToolEffect
    ├── McpService
    │   └── server processes/connections
    └── ExtensionSupervisor
        └── extension processes
```

Parent lifecycle cancellation descends.

## 25.2 Graceful shutdown

Order:

1. reject new public work;
2. tell sessions/services to close;
3. signal owned effects/tasks;
4. wait bounded time for cooperative completion;
5. force-kill owned subprocess groups if required;
6. commit/drain required local persistence;
7. release resources;
8. restore terminal;
9. exit.

Do not detach tasks to “finish later.”

## 25.3 Recoverable close

If shutdown interrupts an operation after an effect intent but before settlement, leave the durable operation open. Reopening runs ordinary recovery.

This is not corruption and not automatic cancellation.

---

# 26. Failure model

## 26.1 Error taxonomy

Keep errors specific enough that frontends/recovery can act:

```text
AdmissionError
PersistenceError
ProviderError { transient?, phase }
ToolError / ToolOutcome
PolicyError
ApprovalRequired
CapabilityUnavailable
Cancelled
RecoveryIndeterminate
CorruptSession
InvariantViolation
ResourceExhausted
```

The exact enum hierarchy may differ. Preserve semantic distinctions.

## 26.2 Persistence failure

A failed required transaction means the corresponding state transition did not happen.

Do not update authoritative in-memory state first and then continue after a failed commit as though durability succeeded.

If a transition must optimistically stage memory, it remains explicitly uncommitted/parked until DB success and cannot initiate downstream external effects.

## 26.3 Corruption

Impossible durable state is corruption, not a reason to guess.

Fail visibly with diagnostic information and preserve the DB for inspection/recovery tooling.

Do not add permissive readers that silently reinterpret invalid state merely because an agent generated an old/bad record.

---

# 27. Observability

## 27.1 Tracing identity

Every significant span should carry appropriate IDs:

```text
session_id
operation_id
model_step_id
effect_id
tool_call_id
child_session_id
provider/model
```

## 27.2 Usage ledger

Persist model usage at settlement boundaries independently of later operation success:

- input/output tokens;
- cached read/write tokens;
- cost when known;
- latency;
- provider/model;
- retry attempt;
- cache-hit diagnostics when exposed.

A tool or later persistence failure must not erase already incurred provider usage from accounting.

## 27.3 Privacy

Never log credentials/secrets by default.

Raw provider payload logging is an explicit debugging mode with redaction and retention rules, not baseline tracing.

---

# 28. Code and crate organization

## 28.1 Layer by real dependency boundary

Target eventually:

```text
ion-core
    pure domain vocabulary
    operation transitions
    context projection contracts
    policy/result types
    provider-neutral semantic types

ion-runtime
    Tokio orchestration
    SessionRuntime
    SQLite store implementation
    provider execution adapters
    tool execution
    MCP/extensions/services

ion
    CLI
    TUI
    composition/config
```

`ion-protocol` should exist only when a real external wire boundary (daemon/client) justifies it.

Do not split into many crates preemptively. Establish module boundaries first; extract crates when dependencies/lifecycle/reuse make the boundary real.

## 28.2 Avoid `ion-core` becoming a dumping ground

A module belongs in `ion-core` because it is part of the stable pure/domain contract, not because “everything imports core.”

Provider SDKs, SQLite, terminal code, MCP transports, subprocess management, and host configuration do not belong there.

## 28.3 Agent-friendly source rules

Prefer:

- one obvious declaration location per important type;
- explicit imports over broad glob/prelude magic;
- minimal re-exporting;
- descriptive filenames matching domain concepts;
- small modules with local invariants;
- enums/structs over macro-generated hidden control flow;
- comments explaining *why an invariant exists*, not narrating obvious code.

Use `cargo fmt`, `cargo clippy --all-targets --all-features`, and `cargo test --workspace` (or one repository wrapper command invoking them) as obvious gates.

---

# 29. Current-code migration

The current code should evolve in place.

## 29.1 Keep

- `RuntimeHandle` command-sender concept;
- typed provider stream signals;
- provider adapter boundary;
- object-safe tool abstraction;
- common tool result contract;
- six-tool minimal default;
- strict required-arg/schema validation direction;
- tracked, cancellable tool tasks;
- bash process-group kill;
- no provider/tool I/O awaited on the state-owner loop.

## 29.2 Change

### Current `RuntimeController`

It is currently both process/session/turn authority. Split the concepts:

- process-level `Runtime` = composition/session registry;
- session-level `SessionRuntime` = state owner.

For v0 one-shot mode the code may still instantiate only one session task, but ownership names/boundaries should reflect the durable model.

### Current turn-centric execution

Replace “one live turn” as the top-level correctness unit with durable `Operation` state spanning model steps, tools, steering, queued operations, cancellation, and recovery.

### Counter IDs

Replace durable counter-based IDs with UUIDv7. Keep integer sequence numbers for database/session order.

### No persistence

Insert persistence at admission and every recovery-significant boundary before building more frontends/features.

## 29.3 Do not rewrite for style

A migration commit should preserve working provider/tool behavior where possible and move it behind the new interfaces. Avoid simultaneous aesthetic rewrites that make correctness changes impossible to review.

---

# 30. Testing strategy

The architecture is only real if failure/race tests prove it.

## 30.1 Pure transition tests

Most operation logic should be testable without Tokio, SQLite, or a real model:

```text
given OperationState + input/outcome
assert exact next state + writes + effects
```

Cover every state/trigger pair intentionally. Invalid pairs return typed errors/no transition.

## 30.2 Crash-point matrix

Inject failure after every boundary:

```text
before admission commit
after admission commit
before effect intent commit
after effect intent commit / before effect starts
after external effect / before settlement commit
after settlement commit
while applying steering or promoting queued work
while cancelling
while compacting
while closing
```

Restart from the DB and assert:

- no accepted intent vanished;
- no forbidden effect replayed;
- no semantic result duplicated;
- indeterminate effects are visible;
- recovery reaches a defined state.

## 30.3 Tool recovery tests

At minimum:

- read/search safe replay;
- write already-applied detection;
- write untouched-preimage retry;
- write conflicting external modification → indeterminate/conflict;
- bash started/unsettled → never auto-replay.

## 30.4 Cancellation race tests

Exercise cancellation:

- before model intent;
- after model intent / before spawn;
- while streaming;
- after assistant completes / before commit;
- before tool intent;
- after tool starts;
- during approval;
- during store failure;
- during child work.

## 30.5 Persistence failure

Inject SQLite failures and prove:

- no downstream effect starts from an uncommitted state;
- UI sees the error;
- in-memory state does not pretend success;
- restart remains classifiable.

## 30.6 Provider tests

Fake provider must support:

- deterministic chunks;
- malformed tool calls;
- failure before/after first delta;
- cancellation races;
- delayed completion;
- usage updates;
- tool-call batches.

## 30.7 Frontend tests

- reducer/unit tests for TUI state;
- lagged subscriber → snapshot resync;
- PTY/integration test for terminal restoration;
- print/JSON/TUI produce equivalent final semantic result.

## 30.8 Child tests

- capability cannot widen accidentally;
- depth/concurrency budget enforced;
- parent cancellation cascades;
- child cancellation isolated;
- objective/context handoff is persisted and inspectable.

## 30.9 MCP/extension lifecycle tests

- service crash removes future capability;
- bounded restart/backoff;
- in-flight call gets defined failure;
- unload removes all scoped registrations/tasks;
- no stale tool remains in future capability snapshots.

---

# 31. Required invariants suitable for property/integration tests

1. **Accepted input durability:** every successful admission is durably pending, applied, rejected, or terminal after restart.
2. **Single transition authority:** session semantic state changes only through `SessionRuntime`/store transition path.
3. **No effect without intent:** no repeat-sensitive external effect starts before its durable intent.
4. **No unsafe replay:** unresolved `NeverReplay` effect is never started automatically again.
5. **Atomic settlement:** a semantic effect result and the operation state that consumes it agree transactionally.
6. **Completed assistant integrity:** only validated completed provider output becomes a completed assistant entry.
7. **Frozen step:** in-flight model request/config/tool snapshot never changes underneath the effect.
8. **Exact approval:** executor uses the canonical invocation policy approved.
9. **Fail-closed noninteractive policy:** approval-required work cannot execute without explicit policy.
10. **Portable semantic record:** provider opaque state is not the only copy of Ion-controlled meaning.
11. **Bounded runtime queues:** no unbounded subscriber/tool/task queue grows with session duration.
12. **Owned shutdown:** runtime exit leaves no untracked tasks/process groups it created.
13. **Child authority narrowing:** child cannot silently gain capability above delegated policy.
14. **Snapshot recovery:** a frontend can discard live events and reconstruct current presentation from a fresh snapshot.
15. **Context determinism:** same semantic state + manifest + model config yields the same model-neutral `ContextPlan`.

---

# 32. Implementation dependency order

This is an engineering dependency sequence, not a permanent product roadmap. Reorder only when dependencies remain valid.

## Step 0 — Adopt vocabulary and boundaries

Before adding features:

- update design/task language to `SessionRuntime`, `Operation`, `Effect`, `ModelStep`, etc.;
- specify public command acknowledgment semantics;
- specify cancel vs close;
- stop new features from binding to the old turn-only lifecycle.

## Step 1 — Extract pure operation/session transition core

- define operation states/transitions;
- make current provider/tool outcomes feed transitions;
- separate process `Runtime` from session state ownership;
- keep one session initially if necessary;
- add exhaustive pure tests.

No persistence shortcut yet: the interface should be designed for committed transitions.

## Step 2 — Durable SQLite substrate

Implement:

- UUIDv7 IDs;
- session/entry/operation/state/inbox/effect schema;
- dedicated `rusqlite` store thread;
- durable admission;
- semantic entries;
- operation checkpoints sufficient to rebuild the machine;
- effect intents/settlements with exact effective input and recovery
  class;
- resume/open (reconstruct transcript and open operations).

Migrate the existing one-provider + tool loop through it.

## Step 3 — Recovery correctness

Before TUI/multi-agent/MCP:

- crash injection;
- pending-effect recovery;
- file reconciliation;
- bash indeterminate handling;
- cancellation persistence;
- close-vs-cancel;
- store-failure propagation;
- recoverable startup.

**Proof milestone:**

```text
ion -p "read Cargo.toml and tell me the package name"
```

must work through a real provider/read tool, stream live output, persist semantic state, tolerate injected crashes at every supported boundary, reopen, recover safely, and continue.

## Step 4 — Context/provider correctness

The minimal model-step plan (projected input plus frozen capability
snapshot) is persisted with every step from Step 2 onward — Step 3
recovery cannot replay a provider request without it. What Step 4 adds
is the full context architecture:

- `ContextProjector` replacing the placeholder transcript projection;
- stable `ContextManifest`;
- model-step snapshot;
- prompt-cache discipline/metrics;
- provider capability flags;
- strict tool-call handling;
- readable compaction per 14.7: explicit/harness-owned maintenance, the
  window-relative safety net, bounded overflow recovery, and full usage
  capture (cache reads) in the ledger;
- instruction/context changes at safe boundaries;
- trust + approval pipeline.

## Step 5 — Frontends

Build Ratatui TUI, print, and JSON/events over the stable `SessionHandle`.

Do not add TUI-only session logic.

## Step 6 — MCP service

Add dynamic capability lifecycle only after the tool/effect/policy/context boundaries are proven.

## Step 7 — Bounded child sessions

Implement same-runtime child primitive, explicit context seed, budgets, lineage, cancellation, and capability narrowing.

## Step 8 — ACP

Map ACP onto existing session semantics. Do not redesign core around protocol convenience.

## Step 9 — Subprocess extensions

Add explicit contribution types, scoped ownership, trust, reload boundaries, and agent-friendly extension authoring.

## Step 10 — Optional long-lived/remote runtime

Only if needed:

- daemon/session hibernation;
- Unix socket/remote client adapter;
- stronger multi-client arbitration;
- workspace/worktree managers;
- background operations.

The same durable session model should make this an adapter/lifecycle expansion, not a new agent design.

## Step 11 — WASM only if justified

Do not prepay Component Model complexity.

---

# 33. Deliberately rejected alternatives

## 33.1 One giant RuntimeController

Rejected because it will become a serialization/bloat point as sessions, MCP, extensions, children, and frontends arrive.

Use process-level runtime + per-session state owner.

## 33.2 Actor per module/concept

Rejected. OTP itself warns against processes for code organization. Use tasks only for runtime ownership/lifecycle.

## 33.3 Full Cordis architecture

Rejected as the default because Ion's primary problem is durable suspend/restart and safe external effects, not arbitrary live recomposition of every subsystem.

Borrow scoped registration/cleanup and explicit capability boundaries.

## 33.4 Deterministic workflow engine

Rejected. Ion does not need Temporal-style deterministic replay of arbitrary code.

Persist explicit state/effect boundaries instead.

## 33.5 Provider session as source of truth

Rejected for portability, auditability, failure recovery, and provider switching.

## 33.6 Event-source every live event

Rejected. Token/UI events are high-volume presentation artifacts. Persist semantic entries and total operation checkpoints.

## 33.7 Pi lanes/tree in v0

Do not copy them merely because Pi has them.

Ion's terminal/ACP use case can use one linear semantic history per session and separate child/fork sessions initially. Add branch-tree/lane machinery only when real user-facing rewind/multi-thread semantics require it.

Schema migrations can add parent-entry relationships later; do not distort the initial runtime around hypothetical Slack/email lanes.

## 33.8 Automatic multi-tool parallelism

Rejected until conflict/order/recovery semantics are proven worth the complexity.

## 33.9 Dynamic tool registry as context strategy

A registry may exist operationally, but model-visible tool sets are stable immutable snapshots. Avoid context churn and ambient giant catalogs.

## 33.10 Hidden schema repair

Rejected. Provider/model incompatibilities belong in explicit adapter policy and diagnostics.

## 33.11 Shadow Git as session durability

Rejected. Git is useful workspace history; it is not the agent runtime's effect journal or cancellation mechanism.

## 33.12 Per-version schema migrations before a released format

Rejected while Ion is v0 with no compatibility guarantees. Ordered
migration steps are machinery for moving databases between released
formats; before the first release there are no databases in the wild,
only developer machines, so the steps would be speculative
compatibility code (v0 rule: no speculative abstractions, no
compatibility aliases).

The store keeps an explicit `PRAGMA user_version` gate instead: a
fresh empty database receives the current schema; a database from any
other build — older development artifact or newer Ion — is refused
visibly with a move-it-aside error, never migrated and never silently
reinterpreted (§26.3). When Ion first promises on-disk compatibility,
ordered migrations become a requirement and this decision is
revisited.

---

# 34. Open decisions that do not block the architecture

These should be resolved by evidence during their owning implementation work:

1. Exact Rust variants/fields of total `OperationState`.
2. Whether the replaceable `operation_state` row needs a separate diagnostic transition log later.
3. Exact context manifest storage encoding/content-addressing scheme.
4. ~~Exact automatic compaction thresholds~~ Resolved (2026-08-23, §14.7.1–14.7.4): harness safety net at `window − reserve_tokens` (16k default); overflow recovery retries once. Model-directed tools and synthetic hints remain deferred until a provider/extension contract justifies them.
5. Exact default child concurrency/depth/token budgets.
6. Exact initial child workspace modes and when worktree support becomes worth it.
7. OS credential backend and migration from environment variables.
8. Exact TUI inline APIs / enhanced keyboard fallback based on current Ratatui/crossterm behavior.
9. Current ACP and MCP SDK/wire versions at implementation time.
10. Whether user-facing fork/rewind justifies a tree inside a session or remains separate-session lineage.
11. Whether a future daemon needs DB writer leases/fencing or can remain sole owner behind a socket.

None of these should be allowed to introduce a second runtime, transcript, policy path, or cleanup path.

---

# 35. Source weighting and rationale

This design intentionally does **not** average the architecture of popular coding agents.

## Tier 1 — primary design evidence

### Pi / Earendil current harness work

Why it matters:

- closest product philosophy to Ion;
- current redesign directly addresses durable accepted operations, single-writer session ownership, recovery, steering, queued work, and external-effect uncertainty;
- maintains a minimal model-facing harness while adding durable substrate.

Primary material:

- https://github.com/earendil-works/pi/blob/main/packages/agent/docs/harness-v2.md
- https://github.com/earendil-works/pi/blob/main/packages/agent/docs/harness-v2-state-machine.md
- https://github.com/earendil-works/pi/blob/main/packages/agent/docs/agent-harness.md
- https://earendil.com/posts/what-is-a-harness/
- https://earendil.com/posts/prompt-caching/
- https://earendil.com/posts/session-portability/
- https://earendil.com/posts/pi-autoresearch-and-databricks/

Important lessons adopted:

- accepted prompt as durable operation;
- single writer;
- explicit total recovery state;
- effect intent before repeat-sensitive effect;
- external effects are not exactly-once;
- stable append-oriented context/cache discipline;
- local user-owned semantic session;
- minimal default harness + extensibility.

Ion deliberately does **not** copy Pi's current lane/tree/storage compatibility requirements unless Ion needs them.

### Armin Ronacher's systems/agent work

Primary material:

- https://lucumr.pocoo.org/2025/11/22/llm-apis/
- https://lucumr.pocoo.org/2025/11/3/absurd-workflows/
- https://lucumr.pocoo.org/2026/2/9/a-language-for-agents/
- https://lucumr.pocoo.org/2025/11/21/agents-are-hard/

Important lessons adopted:

- canonical vs derived/provider state;
- local replay/portability;
- durable execution via explicit checkpoints rather than magic;
- thin provider-specific handling over fake universal APIs;
- codebases optimized for local/greppable agent reasoning.

### Erlang/OTP + Elixir design principles

Important lessons adopted:

- process/task for runtime properties, never code organization;
- single ownership/mailboxes;
- supervised lifecycle;
- restart policy/intensity;
- explicit state machines for complex lifecycle.

Ion translates these to Rust/Tokio rather than importing BEAM semantics wholesale.

### Durable Objects / durable execution patterns

Important lessons adopted:

- one coordination atom/single writer;
- in-memory state is disposable;
- persistent state survives process lifetime;
- external I/O creates interleaving, so revalidate after awaited work;
- commit state before externally observable consequences where possible.

## Tier 2 — targeted independent convergence

### Jido / Jidoka

Useful because BEAM practitioners independently model:

```text
state + command → new state + directives/effects
```

and Jidoka exposes explicit plans, journals, snapshots, interruption/approval, and resume.

Source:

- https://github.com/agentjido/jido
- https://github.com/agentjido/jidoka

Jidoka is beta and is not treated as a production reference architecture.

### DeepSeek Harness / Cordis

Useful ideas adopted selectively:

- separate durable session facts from live runtime events;
- derive model history from canonical session state;
- scoped capability registration and structural cleanup;
- explicit model/tool/session/loop seams.

Sources:

- https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md
- https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/core.md

Not adopted:

- “everything is a plugin” as Ion's foundation;
- full reactive coeffect/dependency machinery;
- reversible-effect abstraction for all runtime mutation.

DSH is developer preview and rapidly changing.

### DimAgent

Used only as experimental independent convergence on a minimal single-transition-authority kernel/effect runner. Public architectural evidence is thinner; do not make Ion depend on its unverified details.

## Tier 3 — production Rust sanity checks

Codex and other production Rust agents are useful for:

- queue/event frontends over one engine;
- task/session concurrency boundaries;
- provider protocol implementation patterns;
- seeing where large `core` crates become dumping grounds;
- bounded multi-agent limits.

They are cross-checks, not blueprints.

## Discovery-only material

X/Grok research was used to identify current discussions and source leads—especially the relationship among Pi's durability work, DSH/Cordis, Durable Objects, Absurd, actor ideas, and effect-system discussions.

Claims from social posts should not become implementation requirements unless confirmed by source, docs, code, or a maintainer-authored design note.

---

# 36. Final architecture test

Before approving any substantial design change, ask these questions in order:

1. **Does this make the model-facing loop simpler or more complicated?** If more complicated, why is the complexity necessary?
2. **Who owns the authoritative state?** There must be exactly one answer.
3. **What is durable before acknowledgment?** User intent must not disappear.
4. **What happens if the process dies immediately before and after every external effect?** The answer must be explicit.
5. **Could the effect have happened without settlement becoming durable?** If yes, what is replay/reconciliation/indeterminate policy?
6. **Can another provider understand the semantic session without dereferencing the old provider?** If not, is the limitation unavoidable and visible?
7. **Does this mutate an in-flight model/context/capability snapshot?** It must not.
8. **Does this harm prompt-prefix stability?** If yes, is the cache reset intentional and observable?
9. **Does this add a task/process for code organization rather than runtime ownership?** If yes, remove it.
10. **Does this create another transcript/runtime/policy/cleanup path?** If yes, reject it.
11. **Can the behavior be tested as a pure transition or with deterministic failure injection?** If not, simplify the boundary.
12. **Will an agent modifying Ion later be able to find and understand this invariant locally?** If not, improve code structure/documentation.

If a feature cannot pass this test, it should not enter the core.

---

# 37. One-page implementation handoff

For an implementation agent starting from the current repository:

```text
GOAL
Turn the existing one-controller, non-persistent scripted/tool loop into a durable
minimal harness without expanding product surface first.

KEEP
- current normalized provider streaming shape
- current Tool abstraction + six default tools
- tracked cancel-aware tool tasks
- bash process-group cancellation
- one runtime contract for all future frontends

CHANGE FIRST
- RuntimeController is no longer the whole process/session/turn concept
- introduce process Runtime + per-session SessionRuntime
- replace top-level Turn correctness with durable Operation
- make Operation transitions explicit/pure where possible
- UUIDv7 durable IDs + per-session seq

PERSIST BEFORE MORE FEATURES
- SessionEntry semantic history
- InboxItem acceptance
- Operation + total OperationState checkpoints
- Effect intent/settlement + recovery class
- usage

HARD EFFECT RULE
No repeat-sensitive provider/tool effect starts until exact intent is durable.
On crash: replay safe, reconcile, or mark indeterminate. Never guess.

CONTEXT RULE
Local semantic state is canonical. Context is derived. Stable system/tool prefix is
intentional. Compaction adds a readable summary and never destroys canonical history.
Provider opaque state is optional acceleration, never sole meaning.

TASK RULE
Use Tokio tasks only for runtime ownership/lifecycle/concurrency. Do not actorize modules.
No Arc<Mutex<SessionState>>.

NEXT PROOF
Before TUI/MCP/children: pass crash injection across real provider + read/write/bash
boundaries and demonstrate restart/resume without unsafe replay.

THEN
context/provider semantics → TUI/print/JSON → MCP → bounded children → ACP →
subprocess extensions → optional daemon/remote → WASM only if justified.
```
