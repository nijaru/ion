# Ion design

Status: proposed target architecture, revision 5, 2026-09-12 (America/Los_Angeles).

This document defines the core agent/runtime Ion should become. It is not a compatibility contract with the current implementation. The project is pre-1.0; when evidence disproves an internal design, change the design and implementation rather than preserving unfinished abstractions.

Current Pico/Pi 2 work is the primary minimal-harness design reference. Codex is a primary production-engineering reference when its public implementation answers a concrete question. Other agents are mechanism-specific evidence, not baselines to copy.

[TERMINAL.md](TERMINAL.md) owns interaction requirements. [ROADMAP.md](ROADMAP.md) owns validation gates. [Agent topology research](docs/research/agent-topology-context-2026-09-12.md), [storage research](docs/research/storage-engines-2026-09-12.md), and [Pico/core/AI review](docs/research/pico-core-and-ai-boundaries-2026-09-12.md) record the evidence behind this revision.

The scope is deliberately narrow: **sessions, conversations, immutable history/context controls, durable tasks, inputs, workers, external execution/recovery, authority and clients**. Long-term memory/knowledge stores, shared task boards, vector stores, planner layers and similar higher-level systems are outside the core.

## 1. Product contract

Ion is a provider-neutral Rust coding agent with a first-class terminal interface. It runs one primary conversation by default. Optional multi-agent mode lets the user and model create, observe, steer and control cooperating worker conversations through the same runtime.

Root and workers use the same durable schema and task machinery. Researcher, reviewer, implementer and similar labels are configuration/instruction choices, not runtime subclasses.

Single-agent use must not pay a hidden multi-agent prompt/tool tax. Multi-agent controls are exposed only when enabled.

The core must support:

- streaming model turns, tools, files, shell/process work, images where supported, model changes, approvals and compaction;
- durable acceptance, crash recovery, cancellation and uncertain external effects;
- fresh or history-inheriting workers, foreground/joined or retained/background lifetime, messages and results;
- local macOS/Linux operation with no mandatory cloud daemon/account/telemetry service;
- local models as ordinary provider choices;
- one semantic command/observation contract for Rust/headless/TUI/JSON/ACP clients.

The runtime does not mandate planning, reflection, voting, a task board, a memory system or a swarm policy.

## 2. Minimal durable domain

The target canonical nouns are:

| Concept | Meaning |
|---|---|
| Session | Durable consistency, ownership and transaction boundary containing one primary conversation and related workers/branches. |
| Conversation | Durable agent thread: immutable transcript plus context controls, configuration/authority/workspace state, optional history parent and optional execution owner. |
| Entry | Immutable semantic transcript record with optional materialized model projection/context controls. |
| Input | Admitted user/agent input with target, mode, request key and disposition. |
| Task | One recoverable async operation with immutable input, typed checkpoint, dependencies, ownership edges, invocation generation and terminal outcome. |
| Task output | Durable/bounded result or progress state owned by a task; large opaque data may reference an artifact. |
| Artifact | Retained externalized content/evidence with integrity metadata. |

There is **no separate durable Agent object initially**. A worker is an owned `Conversation`. Public APIs may call a handle `AgentHandle` or `WorkerHandle`, but the durable identity is the conversation ID.

There is also **no generic durable Effect object initially**. The new task model makes a generation request, tool call, job launch and similar external action their own recoverable task. A separate effect entity must earn its existence in a production prototype rather than survive from the old operation model.

## 3. Relationships are separate graphs

Do not collapse Ion into one overloaded tree.

```text
history ancestry:     Conversation --parent/cutoff--> Conversation
execution ownership: Task --owns--> Conversation
execution ordering:  Task --after/depends-on--> Task
workspace binding:   Conversation/Task --binds--> Workspace
communication:       Input(sender,target)
```

A conversation can have both a history parent and an owner. Those relationships are independent.

- history controls inherited transcript/context;
- ownership controls execution/control provenance and scope;
- task dependencies control readiness;
- workspace binding controls external state visibility/mutation;
- messaging carries explicit sender/target identity.

A history fork grants no cancellation or authority rights. Workspace sharing grants neither supervision nor history inheritance.

A task-created conversation records reciprocal ownership in the same atomic creation transaction. The task may later become terminal; the ownership/provenance edge remains so a retained worker stays addressable.

## 4. Session is the consistency boundary

A session is intentionally larger than one conversation. Root and cooperating workers stay in one session because ordinary operations may require atomic invariants across them:

- child conversation + initial input/task creation;
- messaging/input admission;
- task dependency/successor creation;
- cancellation barriers and ownership transitions;
- authority narrowing;
- usage/budget/resource accounting;
- workspace ownership/conflict metadata;
- observation publication.

Do not use one canonical database per worker. That would turn common same-group operations into cross-database protocols.

Use a separate top-level session only when the consistency/lifecycle domain is actually independent: a separate user goal/project, security/credential boundary, independently archived/deleted workspace, or future remote ownership domain. Clean model context alone is not a reason for another session.

## 5. One semantic writer per session

One loaded session has one authoritative mutation line. External work is concurrent; canonical writes are serialized.

```text
caller/tool/model completion
          |
          v
   session command line
          |
   validate/read/build
          |
     short DB commit
          |
  publish + dispatch outside line
```

No model request, HTTP call, subprocess, filesystem operation, timer, user interaction or plugin callback runs while holding session mutation authority or a database write transaction.

The host owns an exclusive cross-process writable-session lock and releases it last during close. A PID file, heartbeat or stale timestamp is not ownership.

A command:

1. validates lifecycle, authority, request identity and relevant revision;
2. reads required committed state;
3. builds a bounded typed mutation batch;
4. validates cross-record invariants including earlier mutations in that batch;
5. commits all-or-nothing;
6. publishes the committed observation;
7. dispatches admitted external work after the line is released.

A persistence result that is uncertain fences the session handle. Reopen and recover durable state; do not guess whether work committed.

## 6. Identity and ordering

`SessionId` is globally unique. Other durable IDs are session-scoped typed identities.

The exact physical representation remains prototype-gated. Compact session-local integers are attractive with a per-session store; UUID-style IDs avoid allocator coupling. A single session mutation sequence that can also mint object IDs, as current Pico explores, is a valid candidate and should be compared against separate object IDs plus `CommitSeq`.

Public behavior must not depend on the representation.

New identities cannot escape to callers or external services until their creating transaction is durable.

## 7. Immutable transcript and derived context

Canonical history is append-only. Do not persist a mutable vector of provider payloads and do not require a separately mutable canonical context list.

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
5. concatenate stored provider-neutral projections;
6. normalize complete tool exchanges for the selected model/provider.

A disposable in-memory cache may accelerate the current conversation. It is never a second source of truth.

### Compaction, handoff and reset

Compaction appends a summary with a new context head. Handoff appends a model-visible new head. Reset appends a head that contributes no model message. Old entries remain queryable.

Context edits may omit or replace earlier model projections where required, but the public/control vocabulary stays constrained. Do not expose arbitrary reordering that can create impossible provider histories.

P2 measures cold context construction across long histories, dense edits and deep forks before choosing indexes/caches or claiming a complexity bound.

### Historical forks

A fork creates a new conversation with a history parent and stable cutoff. Source entries are shared logically, not copied. Later source changes are invisible and source tasks are never inherited.

For initial worker creation, inherited context should select a safe complete exchange boundary. Arbitrary historical/user forks may later support incomplete-exchange request-local repair if that proves useful; the core does not need to fabricate successful results or inherit source tasks.

Configuration, authority and workspace inheritance are chosen separately from history inheritance.

## 8. Durable task model

A task is one logical recoverable async operation.

Generic status is deliberately small:

```text
pending -> running -> terminal
```

A durable abort/cancel mark can coexist with `running` while the old invocation is being joined and a fresh abort invocation is prepared.

Terminal outcomes distinguish at least:

```text
completed
failed
aborted/cancelled
indeterminate
orphaned/unsupported when required by missing implementation policy
```

A task records:

- kind + schema revision;
- owning conversation;
- immutable typed input;
- optional complete typed checkpoint;
- fixed dependencies (`after`);
- owned child conversations;
- foreground/turn/background metadata where required;
- durable abort mark;
- invocation generation/fencing metadata;
- task output reference when the kind exposes one;
- terminal outcome.

### Authoring contract

The kernel exposes one task framework:

```text
execute(running_task, runtime, call) -> terminal closure/plan
recover(running_task, runtime, call) -> terminal closure/plan
abort(running_task, restricted_runtime, call) -> abort closure/plan
```

The methods are ordinary async Rust. Their stack is **not** durable state. A process loss discards the future; the replacement invocation enters `recover` with immutable input plus the last durable checkpoint/output.

While running, trusted task code may make controlled durable commits through its `TaskContext`:

- replace the complete checkpoint;
- create child tasks/conversations under capability rules;
- append authorized transcript entries for turn tasks;
- update bounded task-scoped scratch/output;
- read/wait/cancel only targets allowed by ownership/dependency scope.

Every commit revalidates task identity, invocation generation and cancellation authority on the session writer.

The returned terminal closure/plan executes on the mutation line and atomically stores terminal outcome, final writes, successors/ownership changes and scratch retirement.

### Optional phased authoring helper

Complex task kinds may use a typed phase/checkpoint helper that performs exhaustive dispatch over checkpoint variants. It compiles to the ordinary task trait and adds **no second scheduler/lifecycle model**.

This supersedes P1's earlier conclusion that every production task must expose a mandatory re-entrant `step -> plan` API. P1's crash/race evidence remains valid; the stricter authoring surface is no longer assumed optimal.

## 9. External effects and recovery

There is no generic effect gate by default. The task is the logical recoverable operation.

Before a repeat-sensitive external action, the task durably records enough checkpoint state to classify/recover that action. Examples:

- provider/model request: exact model/request identity, attempt and any provider reconciliation handle;
- shell/process spawn: prepared command/environment and durable external/process identity when one exists;
- remote tool call: request/idempotency identity or explicit no-safe-retry classification.

Task kinds define recovery semantics for each nonterminal checkpoint phase:

| Recovery class | Behavior |
|---|---|
| Retry-safe | execute another attempt; retain prior possible usage/cost. |
| Reconcile/adopt | query the durable external identity and adopt its state/result. |
| No safe retry | settle/park as indeterminate until explicitly resolved or abandoned. |

Attempt history/usage may use a ledger keyed by task + attempt. That is not a second effect lifecycle.

If a task genuinely performs several independent repeat-sensitive operations, prefer child tasks. Otherwise it must checkpoint every uncertain boundary. Introduce a first-class `EffectId` only if a concrete case proves task identity is insufficient.

Opening or inspecting a session starts no work. Explicit drive/resume claims pending/recoverable tasks.

## 10. Turns and tools

The default coding turn is still:

```text
input
  -> generation task
      -> tool task(s)
          -> post-tools/join task
              -> next generation or final answer
```

One generation settlement atomically appends its successful assistant result and creates every required tool task plus the join/continuation before becoming terminal.

Tool tasks can finish in any order. Transcript chronology records completion order; model projection restores tool-result order to the originating assistant call order.

Independent read-only tools may run concurrently under resource limits. Mutating calls against one workspace serialize by default unless environment policy proves a stronger safe scheme.

Ordinary `ToolDefinition` code does not receive arbitrary session transaction authority. The trusted built-in tool task owns durability/cancellation and calls the tool through a narrow execution/environment capability. Worker/job creation is exposed through similarly narrow mediated capabilities rather than handing arbitrary mutation access to every extension.

## 11. Inputs and communication

Input acceptance, placement, model consumption and answer settlement are distinct facts.

An input stores target conversation, sender (user/host/conversation), mode, payload, optional request key and disposition/result reference.

Core modes begin with:

| Mode | Busy conversation | Idle conversation |
|---|---|---|
| Submit | reject unless another mode selected | start turn |
| Steer | place at next safe model boundary | start turn unless paused |
| Follow-up | queue successor input | start turn unless paused |
| Queue-only | remain queued | remain queued |
| Notice/write | retain attributed entry/input according to policy | no implicit wake unless requested |

Exact duplicate request-key replay returns the original receipt. Rebinding the same key to different target/content/mode rejects `IdempotencyConflict`.

Inter-worker messages use this same substrate rather than a second mailbox truth model.

Caller cancellation only cancels that caller's wait. It never cancels already accepted durable work.

## 12. Workers and delegation

A worker is an owned conversation. Context seed and lifetime are orthogonal.

### Context seed

**Fresh**: no history parent. Prefer for independent exploration/review, self-contained subtasks and work where parent reasoning would mostly add noise/anchoring.

**Inherited**: history parent + cutoff. Prefer for nuanced continuation/debugging, exact shared requirements or alternative solutions from one established state.

**Reuse** an existing worker for closely related follow-up work when its specialized accumulated context is useful. Reset/handoff it when the context becomes noisy.

Fresh/inherited/reuse all remain inside the same session unless the consistency/lifecycle boundary itself is independent.

### Lifetime

**Joined/foreground**: parent work requires the child result before completion.

**Retained/background**: creation returns the conversation ID; the creator may settle and the worker remains addressable.

Same schema, different dependency/cancellation policy.

### Control surface

Keep the model/human surface small:

```text
run/spawn
send/follow-up
inspect/status
wait
interrupt/cancel
retire
```

The primary conversation is the default synchronizer. Delegate bounded parallelizable work; do not treat more agents as automatically better.

## 13. Model/provider subsystem boundary

The session kernel must not contain HTTP/API-key/OAuth/provider-specific parsing logic.

Use a separate logical AI subsystem inspired by `pi-ai`'s strongest boundaries:

```text
Model registry/service
  Provider
    auth + model catalog + endpoint policy
      API adapter
        wire protocol (OpenAI Responses/compatible, Anthropic, Google, ...)
```

A provider is the runtime/configuration unit. Multiple providers may reuse one wire/API adapter.

The AI subsystem owns:

- provider-neutral message/content/tool/request/result/stream types;
- provider/model registry and dynamic catalog refresh;
- provider capabilities/pricing metadata;
- auth resolution and app-owned credential-store interface;
- wire API adapters and provider endpoint policy;
- provider-specific opaque replay/continuation metadata.

The host owns credential persistence. Credentials are never session truth.

The session/conversation stores the selected model configuration and request-relevant durable snapshot needed for replay/recovery; it does not duplicate the global model catalog.

The provider interface accepts a provider-neutral model request and returns a provider-neutral stream/result. It does **not** receive `SessionId`, `ConversationId`, `TaskId`, database commands or runtime mutation signals.

Provider-specific opaque replay metadata may be attached to provider-neutral assistant content so the producing provider can preserve continuity and another provider can safely ignore incompatible hints.

Provider adapters classify errors into typed facts (transport, timeout, rate-limit/retry-after, overload/server, auth, quota, invalid request, context overflow, unsupported, safety, cancelled, unknown). Generation behavior owns retry/backoff/compaction policy. Avoid hidden SDK retries that bypass durable attempt/usage accounting.

The exact Rust crate/API layout for this subsystem is a separate component-design pass after the kernel contract is frozen.

## 14. Authority, workspace and environment

Authority is structured runtime state, never reconstructed from prose.

Child authority is bounded by requested capability intersect parent ceiling intersect host policy. History inheritance, model changes, extension reload and identifier/name reuse cannot widen it.

Approvals bind the exact prepared invocation and relevant arguments/revision.

Workspace identity is independent from history/ownership. Multiple read-only workers may share a workspace. Parallel mutating workers normally use isolated worktrees/snapshots unless a stronger conflict policy is proven.

Filesystem/process/job/sandbox behavior belongs to an execution-environment boundary, not the session scheduler. The environment may expose durable job identities/reconciliation, but session truth remains owned by the task/runtime.

## 15. Cancellation and close

Durable cancellation marks exact task/conversation scope on the writer, revokes the current invocation generation's normal write authority, then signals local execution.

Settlement committed before the mark wins. Otherwise the old normal completion is fenced; after it returns/joins, a fresh abort invocation performs allowed cleanup and terminal settlement.

Cancellation is not rollback. If an external action may already have happened, retain the uncertainty.

Group/subtree cancellation establishes its admission barrier before traversing descendants so concurrent creation cannot escape the target scope.

Host close stops admission, joins current local invocations according to policy, preserves unfinished durable tasks for recovery, flushes committed state and releases the ownership lock last.

## 16. Observations and clients

Frontends attach without becoming execution owners.

A watch captures an atomic bounded view plus durable sequence/cursor, then receives committed changes and provisional output frames. Overflow requires resnapshot rather than silently presenting an incomplete state.

Live model/tool output is provisional and coalescible. Final durable output replaces matching provisional presentation.

Per-conversation drafts and delayed replies bind captured target IDs. Changing TUI focus cannot reroute an already-submitted command.

Conversation/session summaries must not require loading every historical transcript/task.

## 17. Persistence

Leading physical candidate:

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/

  catalog.sqlite   # optional rebuildable discovery cache if needed
```

One `session.sqlite` contains every fact required for one session command transaction: conversations, entries/context controls, inputs, tasks/dependencies/checkpoints/outcomes, authority/approvals, usage/budgets/resource metadata and artifact references.

Do not split one session transaction across category-specific WAL databases. Independent top-level sessions may have independent owners/connections/WAL files.

SQLite remains the baseline engine. Turso is a P2 benchmark candidate only if measurements or a real sync requirement justify it. Keep the store interface private, but do not build a generic multi-backend framework before a second engine earns inclusion.

Large opaque output may spill to files. Publish content safely before committing a required reference; a crash may leave reclaimable orphan files, never committed references to missing required data.

## 18. Clean rewrite strategy

The current runtime's core abstractions no longer match this design: lane, durable `AgentId`, `OperationId`, singular open effect, operation-bound provider signals and root-wide schema are all replacement targets.

Do not perform a prolonged compatibility refactor.

Once the remaining pre-rewrite prototypes below settle, replace the core directly in `ion-core` and port only reviewed leaf algorithms/tests. Git history is the archive.

Likely rewrite/delete:

- current agent/family/host orchestration;
- lane/operation/runtime state machines;
- old store schema and SQL tied to them;
- current provider-to-operation contract;
- generic effect orchestration;
- old context machinery where it conflicts with immutable entry controls.

Potentially port after interface review:

- path/sandbox/process safety algorithms;
- output bounding/artifact handling;
- provider wire parsing/auth logic into the later AI subsystem;
- policy checks;
- editor/rendering primitives;
- fault/race test scenarios.

MCP/extensions/ACP/RPC/TUI application layers are re-audited after the new command/observation boundaries exist.

## 19. Remaining gates before the rewrite

These are the last design/prototype questions that should block deleting the old core:

| Gate | Evidence required |
|---|---|
| K0.1 Task API | Rust prototype of async `execute/recover/abort`, invocation-fenced `commit`, scratch and terminal closure; optional typed phase adapter without a second scheduler. |
| K0.2 Context | Immutable entry projection/head/edit prototype with compaction/reset/fork and out-of-order tool result normalization. |
| K0.3 Task-level recovery | Provider/tool/job crash traces showing task checkpoint/attempt identity is sufficient without generic `EffectId`, or concrete evidence that an effect entity is required. |
| K0.4 IDs/schema | Provisional session-local ID/sequence representation with same-batch references and rollback/no-ID-leak behavior. |
| K0.5 AI port | Minimal provider-neutral model request/stream/result/error contract sufficient for a scripted generation task; no production provider catalog required yet. |

After these pass, the old core should be removed rather than gradually translated.

## 20. Core validation path

- **P1**: fresh production kernel: input idempotency, task execution/recovery/cancellation, two tools completing out of order, worker ownership/waits, process-loss recovery, missing task kind and writer ownership.
- **P2**: per-session SQLite/storage/output/history/fork scaling and failure measurements; optionally compare Turso under the same workload.
- **P3**: tool/extension contribution, authority/reload/failure/sandbox boundaries.
- **P4**: TUI/group interaction, target-safe input, approvals, narrow layouts, overload/reconnect/terminal restoration.

Higher-level knowledge/task-board systems remain outside these gates and require separate effectiveness evidence later.