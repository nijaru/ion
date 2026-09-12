# Ion design

Status: accepted target architecture, revision 6, 2026-09-12 (America/Los_Angeles).

This document defines the core agent/runtime Ion should become. It is not a description of the legacy implementation and not a compatibility contract with it. The project is pre-1.0; if production evidence disproves an internal design, change the design rather than preserving unfinished abstractions.

R0.1–R0.5 were validated together at `81c344d713f13b73e19232b4ad36dfe40ba663b9` in CI run `34718467220`. [R0 kernel gates](docs/r0-kernel-gates-2026-09-12.md) records the evidence. [ROADMAP.md](ROADMAP.md) owns work order, [docs/core-runtime-migration.md](docs/core-runtime-migration.md) owns the clean cutover, [docs/source-layout.md](docs/source-layout.md) owns module organization, and [TERMINAL.md](TERMINAL.md) owns interaction requirements.

Current Pico/Pi 2 remains a useful minimal-harness reference and Codex a useful production-engineering reference. Neither is an API or compatibility target.

The core scope is deliberately narrow: **sessions, conversations, immutable history/context controls, durable tasks, inputs, workers, external execution/recovery, authority, persistence and clients**. Long-term memory/knowledge stores, shared task boards, vector stores, planner layers and similar higher-level systems are outside the core.

## 1. Product contract

Ion is a provider-neutral Rust coding agent with a first-class terminal interface. It runs one primary conversation by default. Optional multi-agent mode lets the user or model create, observe, steer and control cooperating worker conversations through the same runtime.

Root and workers use the same durable schema and task machinery. Researcher, reviewer, implementer and similar labels are configuration/instruction choices, not runtime subclasses.

Single-agent use must not pay a hidden multi-agent prompt/tool tax. Multi-agent controls are exposed only when enabled.

The eventual product must support streaming model turns, tools/files/process work, images where supported, model changes, approvals and compaction; durable acceptance, crash recovery, cancellation and uncertain external actions; fresh or history-inheriting workers with joined or retained lifetime; local macOS/Linux operation with no mandatory cloud daemon/account/telemetry service; local models as ordinary provider choices; and one semantic command/observation contract for Rust/headless/TUI/JSON/ACP clients.

The runtime does not mandate planning, reflection, voting, a task board, a memory system or a swarm policy.

## 2. Minimal durable domain

The canonical nouns are:

| Concept | Meaning |
|---|---|
| Session | Durable consistency, ownership and transaction boundary containing one primary conversation and related workers/branches. |
| Conversation | Durable agent thread: immutable transcript plus context controls, configuration/authority/workspace state, optional history parent and optional execution owner. |
| Entry | Immutable semantic transcript record with optional provider-neutral model projection/context controls. |
| Input | Admitted user/agent/host input with target, sender, mode, request identity and disposition. |
| Task | One recoverable async operation with immutable input, typed checkpoint, dependencies, ownership edges, invocation generation and terminal outcome. |
| Task output | Durable bounded task result/scratch/progress state; large opaque data may reference an artifact. |
| Artifact | Retained externalized content/evidence with integrity metadata. |

There is **no separate durable Agent object**. A worker is an owned `Conversation`. Public APIs may use `WorkerHandle`/`AgentHandle` terminology, but durable identity is the conversation ID.

There is **no generic durable Effect object**. Generation requests, tool calls, jobs and similar external operations are recoverable tasks or child tasks. R0.3 found no independent generic effect identity/lifetime worth preserving.

## 3. Relationships are separate graphs

Do not collapse Ion into one overloaded tree.

```text
history ancestry:     Conversation --parent/cutoff--> Conversation
execution ownership: Task --owns--> Conversation
execution ordering:  Task --after/depends-on--> Task
workspace binding:   Conversation/Task --binds--> Workspace
communication:       Input(sender,target)
```

History controls inherited transcript/context. Ownership controls provenance/control scope. Task dependencies control readiness. Workspace binding controls external-state visibility/mutation. Messaging carries explicit sender/target identity.

A history fork grants no cancellation/authority rights. Workspace sharing grants neither supervision nor history inheritance. A task-created conversation records reciprocal ownership atomically; the creating task may later become terminal while a retained worker remains addressable.

## 4. Session is the consistency boundary

A session is intentionally larger than one conversation. Root and cooperating workers stay in one session because ordinary operations may require atomic invariants across them: child conversation + initial task creation, messaging/input admission, task dependencies/successors, cancellation barriers, ownership transitions, authority narrowing, usage/budget/resource accounting, workspace-conflict metadata and observation publication.

Do not use one canonical database per worker. Clean model context is not a reason for another session.

Use another top-level session when consistency/lifecycle is genuinely independent: another user goal/project, security/credential boundary, independently archived/deleted workspace, or future independent remote ownership domain.

## 5. One semantic writer per session

One loaded session has one authoritative mutation line. External work may be concurrent; canonical writes are serialized.

```text
caller / task completion / external result
                  |
                  v
          session command line
                  |
          validate/read/build
                  |
            short commit
                  |
         publish + dispatch
          outside the line
```

No model request, HTTP call, subprocess, filesystem operation, timer, user interaction or plugin callback runs while holding session mutation authority or a database write transaction.

A command:

1. validates lifecycle, authority, request identity and relevant revision;
2. reads required committed state;
3. builds a bounded typed mutation batch;
4. validates cross-record invariants including earlier mutations in the same batch;
5. commits all-or-nothing;
6. publishes committed observations;
7. dispatches admitted external work only after mutation authority is released.

A persistence result that is uncertain fences the writable session handle. Reopen and recover durable state; never guess whether a batch committed.

The host owns an exclusive cross-process writable-session lock and releases it last during close. PID/heartbeat/stale-timestamp heuristics are not ownership.

## 6. Identity and ordering

`SessionId` is globally unique. All other durable IDs are session-scoped typed identities backed privately by one monotonic `LocalSeq` namespace per session store.

Conceptually:

```text
ConversationId(LocalSeq)
EntryId(LocalSeq)
InputId(LocalSeq)
TaskId(LocalSeq)
ArtifactId(LocalSeq)
CommitSeq(LocalSeq)
```

These Rust types are not interchangeable. Sharing one physical ordered namespace does not make identity kinds equivalent.

Within one atomic batch, the writer may reserve successive `LocalSeq` values for several new cross-referencing objects and then a later value for the batch's `CommitSeq`. Mutation-only commits still obtain a commit value. A rejected transaction publishes and consumes no durable sequence values.

The ordering provides compact SQLite INTEGER keys, stable historical entry cutoffs and one authoritative local clock. Public behavior must not depend on numeric adjacency or assume an ID equals its commit cursor.

New identities cannot escape to callers or external services until their creating transaction is durable. Cross-session references, when required, use `(SessionId, typed local ID)`.

## 7. Immutable transcript and derived context

Canonical history is append-only. Do not persist a mutable vector of provider payloads or a separately mutable canonical context list.

An entry has stable identity/kind and may materialize generic facets:

```text
data        kind-specific durable semantic data
projection  provider-neutral model message(s), if any
head        retained-context boundary, if any
edits       constrained omit/replace controls for older projections, if any
```

Context for a request is derived from the conversation's fork-visible immutable transcript:

1. choose the durable request cutoff;
2. find the newest visible context head;
3. read the visible range from that boundary;
4. fold constrained context edits in transcript order;
5. concatenate provider-neutral projections;
6. normalize complete tool exchanges for the chosen provider/model.

A disposable in-memory cache may accelerate current context construction. It is never a second source of truth.

### Compaction, handoff and reset

Compaction appends a summary entry with a new head. Handoff appends a model-visible new head. Reset appends a head that contributes no model message. Old entries remain queryable.

Context edits may omit or replace earlier projections, but the vocabulary stays constrained. Do not expose arbitrary reordering capable of creating impossible provider histories.

A context head used as a retained boundary must land on a complete tool exchange. P2 measures cold construction across long histories, dense edits and deep forks before selecting indexes/caches or claiming complexity bounds.

### Historical forks

A fork creates a conversation with a history parent and immutable source cutoff. Source entries are shared logically, not copied. Later source appends are invisible and source tasks are never inherited.

Initial inherited workers use a complete exchange cutoff. Arbitrary historical incomplete-exchange repair may be added later only if it justifies its semantics; the core never fabricates successful tool results merely to make a fork valid.

Configuration, authority and workspace inheritance are chosen independently from history inheritance.

## 8. Durable task model

A task is one logical recoverable async operation.

Generic lifecycle stays small:

```text
pending -> running -> terminal
```

A durable cancellation/abort mark may coexist with `running` while the old invocation is joining and a fresh abort invocation is prepared.

Terminal outcomes distinguish at least completed, failed, aborted/cancelled and indeterminate, plus orphaned/unsupported when missing task implementation policy requires it.

A task records kind/schema revision, owning conversation, immutable typed input, optional complete typed checkpoint, fixed dependencies, owned child conversations, required foreground/background metadata, durable cancellation state, invocation generation/fencing metadata, bounded output reference and terminal outcome.

### One authoring contract

The kernel exposes one typed task framework:

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

Async Rust stack state is process-local and **not durable continuation**. Process loss discards the future; replacement execution enters `recover` with immutable input plus the latest durable checkpoint/output.

Trusted task code receives an invocation-scoped `TaskContext`. Its durable commit path can replace the complete checkpoint and perform narrowly authorized canonical writes. Every commit revalidates task identity, invocation generation and cancellation authority on the session writer.

The terminal plan/closure is applied by the writer so outcome, final writes, successor creation/ownership changes and scratch retirement can settle atomically.

### Optional phase helper

Complex task kinds may use a typed checkpoint/phase helper for exhaustive dispatch. It compiles to the ordinary task contract and introduces no second scheduler, lifecycle or persistence framework.

## 9. External actions and recovery

Task state is the recovery boundary. Before a repeat-sensitive external action, the task durably records enough checkpoint/attempt information to classify recovery.

Representative states include prepared/not-dispatched, dispatched retry-safe attempt, dispatched reconcile/adopt handle, and dispatched no-safe-retry operation.

| Recovery class | Behavior |
|---|---|
| Retry-safe | record the uncertain prior attempt and perform another attempt. |
| Reconcile/adopt | query/use durable external identity or evidence and adopt state/result without repeating. |
| No safe retry | settle/park indeterminate rather than guessing or duplicating the action. |

Attempt/usage evidence may be a task-scoped ledger. It is not another generic effect lifecycle.

If one logical task would perform several independent repeat-sensitive operations, prefer child tasks. Otherwise its checkpoint must encode every uncertain boundary. Add a first-class effect entity only if a future concrete operation demonstrates an independent identity/lifetime the task model cannot represent cleanly.

Opening or inspecting a session starts no work. Drive/resume is explicit.

## 10. Turns and tools

The default coding turn is composed from ordinary tasks:

```text
input
  -> generation
      -> tool task(s)
          -> post-tools/join
              -> next generation or final answer
```

A generation settlement atomically appends its successful assistant entry and creates every required tool child plus the join/continuation before becoming terminal.

Tool tasks may finish in any order. Transcript chronology records completion order; model projection restores tool-result order to the originating assistant call order.

Independent read-only tools may run concurrently under resource limits. Mutating calls against one workspace serialize by default unless the execution-environment pass proves a stronger safe policy.

Ordinary tool definitions do not receive arbitrary session transaction authority. Trusted built-in task adapters mediate durable worker/job creation and other session-affecting operations.

## 11. Inputs and communication

Acceptance, placement, model consumption and answer settlement are distinct facts.

An input stores target conversation, sender, mode, payload, optional request key and disposition/result reference.

Initial modes:

| Mode | Busy conversation | Idle conversation |
|---|---|---|
| Submit | reject unless another mode selected | start turn |
| Steer | place at next safe model boundary | start turn unless paused |
| Follow-up | queue successor input | start turn unless paused |
| Queue-only | remain queued | remain queued |
| Notice/write | retain attributed input/entry according to policy | no implicit wake unless requested |

Exact duplicate request-key replay returns the original receipt. Rebinding the same key to different target/content/mode rejects `IdempotencyConflict`.

Inter-worker communication uses this same input substrate rather than another mailbox truth model.

Caller/waiter cancellation only cancels that caller's wait. It never cancels accepted durable work.

## 12. Workers and delegation

A worker is an owned conversation. Context seed and lifetime are orthogonal.

Context seed:

- **fresh** — no history parent;
- **inherited** — stable parent/cutoff at a complete exchange boundary;
- **reuse** — continue an existing retained worker when its specialized context remains useful.

Lifetime:

- **joined/foreground** — parent work depends on the child result;
- **retained/background** — creator may settle while the worker remains addressable.

Fresh/inherited/reuse remain in the same session unless consistency/lifecycle itself is independent.

Keep the human/model control surface small:

```text
run/spawn
send/follow-up
inspect/status
wait
interrupt/cancel
retire
```

The primary conversation is the default synchronizer. Parallelism is bounded and purpose-driven; more agents are not assumed better.

## 13. `ion-ai` model/provider boundary

The provider-neutral model contract is a separate small crate, `ion-ai`. `ion-core` may depend on it; `ion-ai` must not depend on session/task/store types.

Initial contract:

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
ScriptedModelService
```

The model service receives no `SessionId`, `ConversationId`, `TaskId`, database command, credential-store handle or runtime mutation signal.

Provider-specific opaque replay metadata may be attached to otherwise provider-neutral assistant content. Another provider may ignore incompatible metadata safely.

Typed provider failures cross the boundary as facts. Generation tasks own durable retry/backoff/compaction/usage policy. Hidden provider/SDK retries that bypass durable attempt accounting are disabled or controlled.

The later provider pass extends `ion-ai` roughly as:

```text
ModelService / registry
  Provider
    auth + model catalog + endpoint policy
      API adapter
        HTTP/SSE/WebSocket wire protocol
```

Provider and wire API are separate concepts so multiple providers can reuse one adapter. The host/application owns credential persistence. Credentials are never session truth. Dynamic model catalog caches are not session storage.

## 14. Authority, workspace and execution environment

Authority is structured runtime state, never reconstructed from prose.

Child authority is bounded by requested capability intersect parent ceiling intersect host policy. History inheritance, model changes, extension reload and identifier reuse cannot widen it.

Approvals bind the exact prepared invocation and relevant arguments/revision.

Workspace identity is independent from history and ownership. Multiple read-only workers may share a workspace. Parallel mutating workers normally use isolated worktrees/snapshots unless a stronger conflict policy is proven.

Filesystem/process/job/sandbox behavior belongs to an execution-environment boundary, not the session scheduler. The environment may expose durable job identities/reconciliation, but canonical session truth remains owned by the task/session runtime.

## 15. Cancellation and close

Durable cancellation marks exact task/conversation scope on the writer and revokes the current normal invocation's canonical write/settlement authority. The runtime then sends a process-local cancellation signal for prompt return.

Settlement committed before the durable mark wins. Otherwise late normal completion is fenced. After the old invocation returns/joins, a fresh higher-generation abort invocation performs allowed cleanup and terminal abort settlement.

Cancellation is not rollback. If an external action may already have happened, retain the uncertainty.

Subtree/group cancellation establishes its admission barrier before traversing descendants so concurrent child creation cannot escape the target scope.

Host close stops admission, signals/joins local invocations according to policy, preserves unfinished durable tasks for recovery, flushes committed state and releases the ownership lock last.

## 16. Observations and clients

Frontends attach without becoming execution owners.

A watch captures an atomic bounded view plus durable `CommitSeq`, then receives committed changes and provisional output frames. Overflow requires resnapshot rather than silently presenting incomplete state.

Live model/tool output is provisional and coalescible. Final durable output replaces matching provisional presentation.

Per-conversation drafts and delayed replies bind captured target IDs. Changing TUI focus cannot reroute an already-submitted command.

Conversation/session summaries must not require loading every historical transcript/task.

## 17. Persistence

Semantic rule: one session has one crash-atomic canonical store boundary.

Leading physical topology:

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/

  catalog.sqlite   # optional rebuildable discovery cache if needed
```

One `session.sqlite` contains every fact required for one session transaction: local sequence, conversations, entries/context controls, inputs, tasks/dependencies/checkpoints/outcomes, authority/approvals, usage/budgets/resource metadata and artifact references.

Do not split one session transaction across category-specific WAL databases. Independent sessions may have independent owners/connections/WAL files.

SQLite is the baseline. P2 validates physical topology, indexes, WAL/checkpoint behavior, history scale, backup/repair and artifact publication before making performance claims. A second engine such as Turso must earn inclusion through measurements or a concrete sync requirement; do not build a generic multi-backend framework preemptively.

Large opaque output may spill to files. Publish required content safely before committing its durable reference; crashes may leave reclaimable orphan files, never committed references to missing required data.

## 18. Source/module architecture

`docs/source-layout.md` is authoritative for file organization. The fresh core is organized by semantic owner, not by a catch-all runtime:

```text
ion-ai          provider-neutral model contract

ion-core
  id
  conversation
  task
  session
  view
  store
  builtin
  artifact
```

The session module owns writer/scheduler/lifecycle, not everything asynchronous. SQLite is contained under `store/sqlite/`. Context projection is pure under `conversation/context/`. Built-in generation/tool/join behavior uses the ordinary task contract.

Avoid broad `runtime.rs`, `manager.rs`, `common.rs`, `utils.rs`, giant `sql.rs` or giant TUI files. Most hand-written Rust files should remain small/cohesive; files around 700–800 lines trigger a responsibility review and splitting is the default above roughly 1,000 lines unless a recorded cohesive exception exists.

## 19. Clean rewrite strategy

The legacy runtime's core abstractions no longer match this design: lanes, durable Agent/Family identity, `OperationId`/`OperationMachine`, generic/singular effects, operation-bound provider signals and root-wide storage assumptions are replacement targets.

Do not perform a prolonged compatibility refactor and do not maintain old/new production runtimes side by side.

Implementation order is:

```text
K0  promote ion-ai contract
K1  storage-independent target domain
K2  one session writer + deterministic in-memory store
K3  task driver/recovery/cancellation
K4  fresh per-session SQLite schema/store
K5  scripted generation -> tools -> join -> generation
K6  workers as owned conversations
```

Potentially useful old path/process/policy/output/provider-wire algorithms may be ported only after their new boundary exists. MCP/extensions/ACP/RPC/full TUI/app behavior is re-audited from the new command/observation contract outward.

The old application binary may temporarily be absent or minimal during cutover. Keeping obsolete runtime semantics alive merely to preserve temporary usability is not a goal. The workspace itself remains green.

Temporary R0 and historical P1 prototypes remain evidence only until equivalent fresh production invariants are covered, then are deleted.

## 20. Validation path

- **K0–K6 / M1**: fresh production kernel, scripted model/tool chain, SQLite session store, one worker, crash/cancel tests; old core removed.
- **P1**: durable execution correctness including admission idempotency, task races/recovery, writer ownership, missing task kind, close and worker races.
- **P2**: physical storage/output/history/fork scaling and failure measurements.
- **P3**: execution/tools/extensions/MCP/authority/sandbox contribution boundaries.
- **P4**: TUI/group interaction, target-safe input, approvals, narrow layouts, overload/reconnect/terminal restoration.

Higher-level knowledge/task-board/memory systems remain outside these gates and require separate effectiveness evidence later.

No part of this document is a claim that the fresh production core already exists. Revision 6 fixes the accepted target after R0; implementation and production validation now have to catch up.