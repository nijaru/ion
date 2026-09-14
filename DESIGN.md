# Ion design

Status: accepted target architecture, revision 7, 2026-09-13 (America/Los_Angeles).

Revision 7 accepts the targeted repair direction from the source review at `888d103c`; it does not claim the repairs implemented. `ROADMAP.md` §1 owns their order and acceptance evidence. The core nouns and recovery architecture remain unchanged.

This document defines the core agent/runtime Ion should become. It is not a description of the legacy implementation and not a compatibility contract with it. The project is pre-1.0; if production evidence disproves an internal design, change the design rather than preserving unfinished abstractions.

R0.1–R0.5 were validated together at `81c344d713f13b73e19232b4ad36dfe40ba663b9` in CI run `34718467220`. [R0 kernel gates](docs/r0-kernel-gates-2026-09-12.md) records the evidence. [ROADMAP.md](ROADMAP.md) owns work order, [docs/core-runtime-migration.md](docs/core-runtime-migration.md) owns the clean cutover, [docs/source-layout.md](docs/source-layout.md) owns module organization, and [TERMINAL.md](TERMINAL.md) owns interaction requirements.

Current Pico/Pi 2 remains a useful minimal-harness reference and Codex a useful production-engineering reference. Neither is an API or compatibility target.

The core scope is deliberately narrow: **sessions, conversations, immutable history/context controls, durable tasks, inputs, workers, external execution/recovery, authority, persistence and clients**. Long-term memory/knowledge stores, shared task boards, vector stores, planner layers and similar higher-level systems are outside the core.

Concrete designs for boundaries that do not exist yet live in `docs/design/`, and consequential choices with their status live in `docs/decisions.md`; `DESIGN.md` states the accepted architecture and invariants those documents must remain consistent with.

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

The batch is the durable write set: a persistence backend must be able to reconstruct equivalent semantic records from it without depending on the observation vocabulary. Observation invalidations are carried separately.

Resident `SessionState` belongs to the session owner, not the persistence backend. The writer prepares and validates journaled changes in place while mutation authority excludes readers, commits the batch through a narrow private persistence interface, then publishes observations. Rejection rolls back prepared changes; persistence failure rolls them back and fences the owner. There is no second resident-application step or fallible semantic work after durability.

Any persistence error conservatively fences the writable session handle and fault-stops local invocations. Failed persistence rolls back prepared resident changes and does not publish committed observations. Reopen and recover durable state; never guess whether a batch committed.

The host acquires an OS-held exclusive cross-process writable-session lock before reconstruction or recovery and releases it last, after local invocations join and storage closes. Reconstruction reads one consistent snapshot. PID/heartbeat/stale-timestamp heuristics are not ownership. Commit-cursor CAS remains defense-in-depth: it fences stale canonical writes, but cannot by itself prevent two processes executing an external action. The OS advisory lock is implemented (`store/sqlite/ownership.rs`), acquired before reconstruction and released last during close; acquisition waits briefly for a lock that may be mid-handover, because the kernel releases the lock at exit/exec and a process that has just forked shares its open file description with the child until that child execs. Read-only inspection by a second process would take a shared lock instead; every open is currently a writable owner.

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

A context head used as a retained boundary must land on a complete tool exchange. The writer validates the resulting projection whenever a control claims to establish a usable context: appending a head or edit rejects an incomplete exchange or an orphaned tool result and consumes no sequence values. Plain appends stay unvalidated so an in-flight tool exchange remains durably recordable. Projection rejects a tool result whose originating assistant call is not present in the same selected range, so omitting a call cannot leave its result behind. Tool-call identity is scoped to its originating assistant message rather than treated as globally unique.

P2 measures cold construction across long histories, dense edits and deep forks before selecting indexes/caches or claiming complexity bounds.

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

A process-local interruption is not a terminal outcome. A handler that panics, the async runtime dropping a future, or a `TaskRunError` leaves the durable task running with its checkpoint intact; only an explicit later drive enters `recover` or `abort`. A terminal `failed` is written only for a known application failure with no unresolved external action, and a terminal `indeterminate` records work whose external effect may have happened but cannot be safely reconciled. Converting an interruption into `failed` would discard that distinction, so the driver never does it.

A terminal `unsupported` is written only for work that never dispatched. If a task is already running and its `(kind, schema_version)` implementation is unavailable, recovery is blocked: the driver rejects the drive without consuming a generation or writing an outcome, and the record stays durable until the implementation is registered. This keeps a temporarily missing extension or task version from irreversibly discarding recoverable work.

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

An invocation also reads through `TaskContext`, and only through it:

- its own conversation's transcript, one bounded page at a time, at a request cutoff once R7 adds a finite request-basis read (the current paging API has only an exclusive lower cursor);
- the resolved outcomes of its own fixed dependencies, in dependency order;
- the admitted inputs durably bound to this task, in admission order.

These are fallible reads of committed state. A dependency is terminal before an invocation is reserved, so reading outcomes is not a wait. Reads are scoped to the invocation's conversation, its own dependency list and its own input binding rather than exposing the session: an invocation cannot observe unrelated conversations, cannot read an unbound input and cannot widen its own input.

A settlement makes its successors runnable, and the driver dispatches work a settlement made runnable: the successors that plan created, plus dependents of the settled task whose dependencies are now all terminal. Dispatch is scoped to what the settlement touched, so admitting a task still never starts it. A candidate whose kind is not registered stays pending for an explicit drive rather than being settled `unsupported`, so an implementation can still be registered later. Each dispatched candidate is an ordinary owned invocation: close joins it, a second local drive for the same task is rejected, and cancellation still fences reservation.

The terminal plan/closure is applied by the writer so outcome, final writes, successor creation/ownership changes and scratch retirement can settle atomically. The implemented part of that contract is a bounded `TaskPlan` attached to a task completion: it queues conversations the plan creates, immutable transcript entries and successor tasks, and both a successor and an entry may target an existing conversation or one this plan creates. Accepted input is not part of a plan: the session writer placed its entry when it bound the input to the answering turn, so a settlement neither places nor consumes input. A planned successor also names its turn: it inherits the settling task's, opens the target conversation's own, or stays background. Plan-local handles become real session-local IDs only when the writer applies the plan, so no ID escapes before commit. The writer revalidates invocation generation, cancellation and authority, applies planned conversations, then entries, then successors (opening a successor's turn where the plan asked for it), and commits outcome and plan in one batch; any failure rolls back the outcome and every planned write, so a worker conversation cannot exist without the outcome that created it, and a plan cannot add a second entry for an input the session already placed. A plan-created conversation is owned by the settling task, which records the reciprocal ownership edge in the same commit. Retiring conversation history and scratch artifacts remains open.

### Optional phase helper

Complex task kinds may use a typed checkpoint/phase helper for exhaustive dispatch. It compiles to the ordinary task contract and introduces no second scheduler, lifecycle or persistence framework.

## 9. External actions and recovery

Task state is the recovery boundary. Before a repeat-sensitive external action, the task durably records enough checkpoint/attempt information to classify recovery.

Representative states include prepared/not-dispatched, dispatched retry-safe attempt, dispatched reconcile/adopt handle, and dispatched no-safe-retry operation. The built-in tool kind is the first concrete instance: it records a durable dispatch before handing a call over, so a replacement invocation can tell "never dispatched" from "outcome unknown". A tool that does not declare itself retry-safe settles `Indeterminate` instead of repeating the call or claiming it was stopped, and its recorded result entry carries that uncertainty into the transcript rather than inventing a success.

| Recovery class | Behavior |
|---|---|
| Retry-safe | record the uncertain prior attempt and perform another attempt. |
| Reconcile/adopt | query/use durable external identity or evidence and adopt state/result without repeating. |
| No safe retry | settle/park indeterminate rather than guessing or duplicating the action. |

Attempt/usage evidence may be a task-scoped ledger. It is not another generic effect lifecycle.

Absent checkpoints and unreadable/unsupported checkpoints are distinct. Recovery settles on unreadable evidence instead of decoding it as “not dispatched” or rebuilding a frozen request: the built-in adapters read a three-way checkpoint (absent / readable / unreadable) and an unreadable one becomes an Indeterminate settlement with no dispatch. Prepared tool evidence identifies the call and the recovery policy that was in force when it was handed over, not the implementation that served it: recovery repeats a dispatched call only when the recorded policy *and* the current one allow a repeat, so a same-named implementation replaced between dispatch and recovery is not distinguished from the original. Closing that needs recorded implementation identity or explicit compatibility evidence plus a regression that reopens a session after a same-name replacement; until then this protection is policy-level, not implementation-level. 
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

A generation settlement atomically appends the transcript entries the answer produced and creates every required tool child plus the join/continuation before becoming terminal. User-message placement is independent of answer success: the writer appends the user entry and records its input placement in the commit that creates the answering turn, before any invocation runs. A still-queued input is deliberately not placed, because a running generation re-reads the transcript when it freezes a request. An explicit retry creates a new answer attempt against that same placed input/entry, not a second admission or duplicate user message. Assistant settlement remains separate and never turns incomplete output into a successful answer.

Tool tasks may finish in any order. Each tool task commits its own result entry with its outcome, so transcript chronology records completion order while model projection restores the results to the originating assistant call order. The post-tools join appends nothing: it is the barrier that makes the continuation generation runnable only once every call has a recorded result, which keeps a split exchange from becoming model context.

Independent read-only tools may run concurrently under resource limits. Mutating calls against one workspace serialize by default unless the execution-environment pass proves a stronger safe policy.

Ordinary tool definitions do not receive arbitrary session transaction authority. Trusted built-in task adapters mediate durable worker/job creation and other session-affecting operations.

### Foreground turns

Dependency edges order work; they are not cancellation or ownership scope. A conversation instead holds one authoritative foreground-turn slot, and each task records the turn root it belongs to (`None` is work outside any turn). A turn root records its own id; the slot lasts until the whole turn closes, not until that root operation becomes terminal.

A generation settlement's successors inherit the settling task's turn by default. A plan may instead give a successor its own turn (`Own`), which opens that conversation's foreground slot in the same commit so the successor roots an independent turn there, or leave it outside every turn (`Background`), which is how retained work survives cancellation; that is an authorized lifetime choice for a trusted kind, not a general way for any plan to escape cancellation scope. Opening a turn a conversation already holds is rejected and rolls the whole plan back, so the one-turn-per-conversation rule holds for plan writes too.

Cancelling a turn durably marks every non-terminal task scoped to that turn root, including the root, then signals the affected local invocations and drives abort cleanup for members that never dispatched, because those cannot observe a local signal. It does not touch terminal tasks, background tasks, owned conversations or unrelated turns. A successor created by cleanup after the turn was cancelled is born cancelled, so an abort cannot smuggle new runnable work into a stopped turn; such a member still needs an explicit drive to settle, which bounds cleanup from recursively generating more cleanup.

Turn control has its own durable cancellation barrier, independent of whether the root operation is already terminal: the conversation holding the slot records `turn_cancelled`, set with the member marks and cleared when the slot is released. Successor inheritance reads that barrier with slot ownership, so cancelling a turn whose root has settled still fences the cleanup work a live member creates, without rewriting a terminal outcome.

A completed turn records **which member closed it**: the turn root carries a durable receipt (`turn_closed_by`) written in the same commit as the settlement and the slot release. The receipt proves closure, not success or answer selection: cleanup or a failed member may close the turn. Joined runs require an explicit result-selection contract distinct from the receipt. The member that ends a chain is only decided while the chain runs; a generation settles as soon as it has planned its children.

The slot is released only when the turn has no remaining non-terminal member, not when the root settles. A terminal root with live tools or a continuation still owns the slot, so a second foreground chain cannot interleave with the first. Background work never holds the slot and never delays its release. Starting a new turn while one is live is rejected until the slot is free.

This reference is deliberately lightweight: there is no separate durable `Turn` entity and no long-running coordinator task. Admission decides whether an input opens the conversation's turn or queues behind it, and a settlement that releases the slot starts the next queued turn. Queued follow-ups and idle-conversation scheduling are this slot plus the admission policy, and a consumed entry reference is only one of several distinct facts about an input.

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

Admission and turn start are one durable decision, and it follows the mode/state table above. A turn-starting mode on an idle conversation admits the input and opens the turn that answers it in the same commit, binding the input to the new turn root; a rejected admission, such as submit on a busy conversation, admits nothing. Steering or following up on a busy conversation queues the input instead of failing, and a notice or queue-only input is retained without waking the conversation.

Queued input is drained when a settlement releases the conversation's turn slot — and only the conversations whose slot that settlement actually released, so unrelated work cannot start a turn — one input per successor turn and in admission order, so a queued follow-up continues the conversation without a client polling. Nothing schedules queued input on its own before that: opening or inspecting a session still starts no work, and a client may ask for the next turn explicitly, which is what a conversation reopened with durable queued input needs. The scheduler never invents a task kind: a conversation's automatic turn shape is configuration (`TurnTemplate`), and without one nothing is scheduled automatically and a turn-starting mode on an idle conversation is refused rather than silently dropped. Scheduling also refuses a turn whose kind the driver cannot run, leaving the input queued for when the implementation is registered instead of binding it to work that would settle `Unsupported`.

Admission, placement into an entry and answer-attempt settlement are separate durable facts: `InputDisposition` is `{Queued, Placed{entry, turn}, Abandoned{entry, turn}, Cancelled}`. Placement is never erased, so abandoning a placed input keeps its entry and cancellation is only for input withdrawn before placement; a failed answer therefore cannot strand an accepted request, and explicit retry/abandon remain possible through `retry_input`/`abandon_input`. Which attempt included a placed input is evidence in that attempt's frozen request, not a second state on the input. A task with no assigned input simply reads its transcript. A duplicate request key replays the original admission without a new commit: the receipt reports the bound turn when one exists and otherwise reports the input as queued, including when the original admission queued it. A queued input is not a lost input, so replaying it is not an error. Steering a busy conversation is deferred to the next turn boundary; injecting input into a running generation mid-turn is not built yet.

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

The implemented part of this contract is the **retained spawn**: a trusted built-in adapter creates the worker conversation, its brief and its initial task in one commit with the outcome that created it, seeds the brief as a transcript entry with a user projection (so a worker with no admitted input still reads it as context), and opens the worker's own foreground turn. A crash before that settlement leaves no worker at all, and a recovery drive creates exactly one.

**Retire** archives an owned, quiescent worker: one durable `Conversation.retired` flag, set only when the conversation is an owned worker with no foreground turn and no non-terminal task. A retired conversation keeps its history, ownership, terminal outcomes and checkpoints and stays readable, but every writer path rejects it — transcript entries, task creation, input admission, opening a turn and settlement plans — and input that was queued but never started is cancelled in the same commit, because retirement stops future work rather than forgetting it. Reactivation clears the flag and starts nothing. Retiring a conversation never invalidates a cutoff another conversation inherited from it. This is the archive half of the control surface; it is not deletion, and it is node-local rather than propagated from a creator's termination.

A spawned worker's run opens **its own conversation's turn**, so the worker is never idle while it works: a follow-up admitted to a busy worker queues and drains into its own successor turn once the first chain finishes, `cancel_turn` on the worker's root stops exactly that run, and the creator's turn and slot are unaffected. Retained lifetime now follows from that scope rather than from being background, so cancellation of the creator's turn simply does not reach the worker's members.

**Joined runs** build on that closure receipt, not on an assumption that the closing member produced the answer. `wait_turn` is the client-side completion wait; a collector must select the result explicitly. Still open is the durable dependency edge on a turn, so a creator *task* can depend on it, plus the collector that carries the result back; that edge must also reject the aggregate cycle where a task depends on turn R while a member of R depends on that task, since backward-only ID references no longer prove acyclicity. Then: the command surface for send/follow-up, inspect and wait; interruption scoped to one worker run as a first-class operation rather than the `cancel_turn` primitive; and reuse of a retained worker.

## 13. `ion-ai` model/provider boundary

Conversation configuration and request assembly resolve instructions/project context, selected model and generation controls, visible tools, context policy and run limits at a request boundary. History inheritance does not implicitly choose those settings or grant authority. The resolved request and relevant implementation revisions are frozen before dispatch; credentials remain host-owned and are never frozen into session truth. Current built-ins use one registered model/catalog and have no such configuration contract yet. R7 defines the narrow interface against two materially different provider APIs rather than designing a speculative configuration framework.

Compaction is an ordinary built-in task/policy over the immutable head/edit primitives, not another runtime. Its trigger and safe placement, output bounds and configurable step/cost/deadline limits are required by the real coding baseline (R8). Provider capabilities, ordered content/replay, usage and typed retry facts must be validated with real wire fixtures; the scripted contract alone does not establish provider neutrality.

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

Provider-specific opaque replay metadata may be attached to otherwise provider-neutral assistant content, carrying its originating provider so an adapter explicitly decides whether reuse is valid. It is preserved verbatim; dropping or reconstructing it is an explicit adapter decision, never a silent default.

A finished transport stream is not necessarily a complete answer. Responses carry termination state (`Completed` or `Incomplete` with a reason such as output-token or context exhaustion), and a generation task must not settle an incomplete response as a successful final answer. Reported usage distinguishes unknown from a reported zero, so an interrupted request is never silently accounted as free.

Typed provider failures cross the boundary as facts. Generation tasks own durable retry/backoff/compaction/usage policy, and they freeze request-relevant identity (model/settings, context cutoff, tool specifications, implementation revision) at the durable boundary so recovery cannot silently continue against changed inputs. The built-in generation kind implements that by recording the complete request, its transcript cutoff and its bound inputs in its checkpoint before dispatch, and by recording what it currently can about the attempt (a counter and its invocation identity); recovery replays the recorded request. A durable per-attempt usage/error ledger is **not** implemented: the provider boundary exposes typed failures, the generation adapter currently reduces them to string interruptions, and the attempt counter is persisted, so "accounting for each attempt" is a counter, not a history of attempts. R7 must preserve those typed facts through generation policy and durable attempt evidence. Provisional stream output carries stable task/invocation identity so late frames cannot attach to a successor generation. Hidden provider/SDK retries that bypass durable attempt accounting are disabled or controlled.

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

Initial workspace-mutation policy, until the execution-environment pass replaces it with enforcement. The session writer serializes that session's *canonical* writes; it does not serialize workspace execution, because model, tool and process work runs outside mutation authority (see §9). What the writer cannot do is serialize two sessions that bind the same workspace, so concurrent writable bindings of one workspace are rejected rather than silently interleaved. A mutating worker therefore requires an isolated workspace under this policy; runtime enforcement remains unimplemented. This is a policy gate, not a cross-session database transaction.

Read-only sharing is not snapshot-consistent: one reader can observe a file before a mutation and another after. Verification and review results bind to a revision, snapshot or recorded content state rather than to a live shared view. A declared capability is not enforced isolation: a shell or extension that claims read-only status does not authorize parallel mutation. Approvals and overwrites recheck the expected base revision, so a file changed since approval is not silently replaced.

Filesystem/process/job/sandbox behavior belongs to an execution-environment boundary, not the session scheduler. The environment may expose durable job identities/reconciliation, but canonical session truth remains owned by the task/session runtime.

## 15. Cancellation and close

Durable cancellation marks exact task/conversation scope on the writer and revokes the current normal invocation's canonical write/settlement authority. The runtime then sends a process-local cancellation signal for prompt return.

Settlement committed before the durable mark wins. Otherwise late normal completion is fenced. After the old invocation returns/joins, a fresh higher-generation abort invocation performs allowed cleanup and terminal abort settlement.

Cancellation is not rollback. If an external action may already have happened, retain the uncertainty.

Subtree/group cancellation establishes its admission barrier before traversing descendants so concurrent child creation cannot escape the target scope.

Host close stops admission and fences canonical writes before joining local invocations. Graceful close signals normal invocations cooperatively and waits for normal/abort handlers to return. Fault close aborts and joins local async futures; it cannot interrupt blocking code that does not yield. Neither mode writes durable cancellation or terminal outcomes for unfinished work. Graceful close may be escalated to fault close. Persistence flush and ownership release remain K4 responsibilities; ownership must be released last.

An admitted local drive is owned by the driver, not the caller awaiting its receipt. Dropping a caller does not detach an untracked handler or permit a duplicate invocation.

Client task/dependency waits subscribe before checking committed state and recheck on coalescible commit notifications. They acquire no resource permits. Dependencies are fixed at task creation and reference only existing tasks, forming a DAG by creation order; self/forward references reject. Invocation contexts do not expose dynamic task waits. Eligible drives may acquire one task-kind-selected resource domain (model, tool or process), with independent process-local limits. Unconfigured domains are unlimited. Cancellation interrupts normal capacity admission and reclassifies the drive before reservation. Abort uses separate bounded cleanup admission (one permit by default), never the saturated normal domain; a joined normal invocation releases its permit before cleanup admission. All invocation kinds use one dispatch path with their appropriate context. These permits and notifications are not durable task truth.

## 16. Observations and clients

Frontends attach without becoming execution owners.

A watch captures an atomic bounded view plus durable `CommitSeq`, then receives committed changes and provisional output frames. Overflow or a restart coverage gap requires resnapshot rather than silently presenting incomplete state. An in-memory observation buffer lost on reopen cannot report an old cursor caught up merely because the new buffer is empty: the session records the earliest commit it can serve a delta for, which a live session takes from its own commits and a reopened one takes from the commit it loaded, so a cursor that predates a restart resnapshots while a cursor at the loaded commit resumes streaming. Provisional frames require task/invocation identity and a current attachment epoch; the current committed-invalidations surface is not yet the streaming contract (R8).

Live model/tool output is provisional and coalescible. Final durable output replaces matching provisional presentation.

Per-conversation drafts and delayed replies bind captured target IDs. Changing TUI focus cannot reroute an already-submitted command.

Conversation/session summaries must not require loading every historical transcript/task. The kernel exposes a bounded `SessionSummary` carrying counts and cursors, and a paginated fork-visible transcript read (`Session::conversation_entries`) with an exclusive cursor. These are interfaces: their K2 implementation still derives visible entries in memory, and K4 must back them with indexed range reads rather than full-history materialization.

## 17. Persistence

Semantic rule: one session has one crash-atomic canonical store boundary. Semantic ownership of a session does not imply full-history residency: the owner reads committed records through typed indexed queries and stages a small transaction overlay, so a commit never costs O(history) and checkpointing an active task never hydrates unrelated historical payloads. Session summaries and observation recovery use bounded views rather than full-state snapshots.

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

The durability floor is WAL journalling with `synchronous = FULL`: a committed transaction survives process death. `NORMAL` trades that for fewer syncs and is deliberately not used. Machine power loss is a filesystem property rather than a store guarantee, so it is not claimed from a single-machine test. The store writes only committed write sets, and it advances the commit cursor with a compare-and-set on the cursor the batch was built against, so a second live authority is fenced instead of silently interleaving with the first.

Opening a session reads durable records only. A task that was running when the process died is reconstructed as running and is entered through an explicit recovery drive; opening never starts work.

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

The original kernel build order was (current repair/delivery order is `ROADMAP.md` §1):

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

K0–K5 foundations and part of K6 exist, but this document remains the target contract, not a claim that every requirement is implemented or validated. Revision 7 preserves the architecture and accepts the repair direction; `ROADMAP.md` distinguishes open work, implemented behavior and observed evidence.