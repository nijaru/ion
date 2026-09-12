# Ion design

Status: proposed target architecture, revision 1, 2026-09-11 (America/Los_Angeles).

This document specifies the agent Ion should become. It is not a description of the current implementation or a claim of demonstrated state-of-the-art performance. Product requirements below are established; architecture choices are proposals to validate through the gates in [ROADMAP.md](ROADMAP.md). Pi and Pico are primary references, not compatibility contracts. Existing Ion code does not constrain the design.

[TERMINAL.md](TERMINAL.md) owns interaction and presentation. [Research](docs/research.md) records sources, alternatives, and evidence limits. [Historical documents](docs/history/README.md) describe the preceding design and are not a second target. Source and tests remain the evidence for what the current binary does.

## 1. Product contract

Ion is a user-owned, provider-neutral Rust coding agent with a first-class terminal interface. It runs one agent by default. Enabling multi-agent operation lets that agent and the user create, observe, steer, and supervise cooperating workers through the same runtime.

A worker is an ordinary agent with its own identity, conversation, configuration, authority, and assignments. Researcher, implementer, and reviewer are configurations, not special runtime types. Single-agent use needs no coordinator prompt, task board, or swarm configuration. Multi-agent mechanisms exist without adding their tool definitions or instructions to a single-agent model request.

The product must support:

- A complete coding conversation: streaming, files, shell, images where supported, model changes, editing, approvals, context management, durable history, and recovery.
- Optional agent groups: fresh or forked context, nested delegation under limits, peer messages, structured results, background jobs, and explicit workspace choices.
- Human control of every agent from the TUI, including its transcript, tools, approvals, budget, changes, and cancellation. Narrow terminals retain these capabilities.
- The same semantics through the Rust library, TUI, print/JSON, and ACP adapters. The terminal owns presentation, not execution truth.
- Local operation on macOS and Linux first; no mandatory cloud service, telemetry, account, or daemon. Local models are ordinary providers.

Runtime capabilities and orchestration policy are separate. The runtime does not mandate planning, reflection, voting, role taxonomies, or a particular swarm strategy. These may be supplied as behavior and evaluated. An agent group is not presumed better than one agent at an equal budget.

## 2. Architecture direction

Use a small durable execution runtime with typed behavior implementations, a provider-neutral conversation model, an explicit execution environment, and client projections.

| Layer | Owns | Does not own |
|---|---|---|
| Host | Session residency, OS ownership locks, provider/plugin lifetimes, client attachments, credentials | Model decisions or a second transcript |
| Session runtime | Serialized mutation, durable acceptance, task lifecycle, invocation fencing, grants, reservations, observation | Provider-specific payloads or terminal layout |
| Agent behavior | Turns, request preparation, tool exchanges, compaction, delegation policy | Raw database writes or another scheduler |
| Environment | Files, commands, jobs, sandbox enforcement, workspace and artifact operations | Conversation topology or agent roles |
| Frontends | Input drafts, focus, rendering, inspection, explicit commands | Provider clients, session persistence, agent lifecycle ownership |

Pico motivates the lifecycle/behavior separation; its foundation remains early implementation rather than a proven replacement. Goose provides a competing composable-operation approach. The preferred Ion design keeps typed behavior state machines behind a common execution boundary instead of translating either project's interfaces. [R1, R5](docs/research.md#references)

Initially, a CLI may embed the host. A later local service or remote transport uses the same session command boundary. Logical separation does not require a process or crate per layer.

## 3. Domain and identity

| Concept | Meaning and lifetime |
|---|---|
| Session | Durable coordination boundary containing one root agent and its group. It outlives process residency. |
| Agent | Addressable, retained participant. Owns configuration, grants, workspace binding, and a current conversation. Outlives any one turn or assignment. |
| Conversation | Immutable transcript with a fixed inherited prefix and local appends. It can be inspected without activating an agent. |
| Turn | One foreground input-to-answer lifecycle, identified by its root task. Its behavior owns the active input group and tool-exchange boundaries. |
| Task | Recoverable unit of execution with typed input, checkpoint, and outcome. A durable task is not a Tokio task. |
| Assignment | Optional shared objective with an owner, revision, dependencies, and result references. It is not the runtime task graph. |
| Job | Environment-backed work that may outlast its initiating tool or turn. Its durable task owns execution from the first effect. |
| Artifact | Retained content or an immutable reference to evidence, output, or a proposed change. |

Use distinct Rust ID newtypes. The proposal uses a globally unique SessionId and opaque session-local identifiers for agents, conversations, entries, tasks, inputs, and artifacts. IDs are allocated by the writer and returned only after commit. Cross-session references include SessionId. CommitSeq orders atomic batches and is not a substitute for an object's identity. Prototype P1 must settle the numeric representation and allocation mechanics before a durable schema is declared stable.

Keep four relationships separate: conversation ancestry, agent supervision, task dependencies, and workspace sharing. A history fork confers neither ownership nor permission. Peer communication is not a supervision edge. An assignment dependency does not automatically create a runtime dependency.

An agent's supervisor is another retained agent, or the session root authority. A short-lived spawn tool does not own the child's entire lifetime. Temporary child work may instead be task-owned with explicit cleanup on owner settlement. Admission records which lifetime applies; no unowned detached work exists.

## 4. Mutation and process ownership

One loaded session has one authoritative writer. Slow effects execute concurrently outside its mutation path. The host acquires an exclusive cross-process session lock before opening a writable runtime and holds it through shutdown. A PID file, heartbeat, or stale timestamp is not ownership. No timed takeover while the old writer might still run.

The writer processes typed commands. A command supplies its target IDs, authority, and, where relevant, expected revision and request key. One transition:

1. Checks lifecycle, authority, expected revision, and duplicate-request identity.
2. Reads the required committed state and constructs a bounded typed batch.
3. Validates references and constraints against committed state plus earlier mutations in the batch.
4. Persists the complete batch.
5. Applies it to resident indexes, captures corresponding events, and acknowledges acceptance.
6. Dispatches effects and observer callbacks outside the mutation path.

A task's settlement, successor creation, and ownership transfer commit together. Observers and scheduling never see a false idle interval between them.

No provider request, process start, plugin callback, timer wait, or user interaction runs while holding mutation authority. SQLite access may block a dedicated storage thread, never the Tokio executor or the TUI. The storage worker executes transactions; it does not become a second semantic state owner. Reads used to decide writes carry revisions that the writer revalidates.

A dropped caller before admission creates no work. After admission starts, dropping its response future cannot abandon an in-progress commit. Durable acceptance and completed execution have separate receipts. Repeating an identical request key returns its original receipt; reusing it with different target, content, or mode rejects as IdempotencyConflict. This deliberately differs from Pico's first-receipt-wins policy. [R1](docs/research.md#references)

## 5. Durable execution and recovery

The common task lifecycle is pending, running, terminal. Terminal outcomes distinguish completed, failed, cancelled, and indeterminate. Readiness, missing code, paused admission, and waiting are separate reasons/status projections, not dozens of kernel phases.

A task records its kind and schema revision, immutable input, owning agent and optional parent task, dependencies, invocation generation, optional full checkpoint, output references, and terminal outcome. Kind-specific phases are typed Rust enums inside the behavior implementation. The scheduler does not interpret model/tool/compaction checkpoints.

The preferred authoring shape is a typed task contract with associated Input, Checkpoint, and Output types, execution/recovery methods, and cancellation cleanup. A private erased adapter supports registry dispatch. Do not expose a JSON-first API or replicate TypeScript type-witness machinery. Prototype P1 must compare an async authoring method with typed checkpoint/settlement commands against an explicit re-entrant step method. Choose one production path, not two public frameworks.

Task creation commits before capacity acquisition. Running reservation and invocation identity commit before external work. A task may replace its full checkpoint at explicit recovery boundaries. Execution returns a typed completion plan; the writer validates the current invocation and commits the outcome, semantic entries, successor work, and scratch retirement atomically.

A pending task has not begun effects. A running task found after process loss invokes recovery, never blindly invokes execute again. Opening and inspecting a session start no tasks; drive/resume is an explicit host action. A temporarily missing task kind blocks recovery visibly while retaining its data. It does not silently erase output or substitute unrelated code. Explicit abandonment records its own terminal decision.

### Effect uncertainty

A running task record alone is not enough for a task that performs multiple repeat-sensitive effects. Each such effect needs a durable invocation identity and intent before dispatch, and a durable result or reconciliation handle afterward. Prefer one primary external effect per tool/job task. Compound implementations must checkpoint each uncertain boundary or create child tasks.

Each adapter declares one recovery policy for the captured effect:

| Policy | Recovery contract |
|---|---|
| Retry-safe | Re-execution is permitted. A new attempt is recorded; prior possible billing is not erased. |
| Reconcile | Query/adopt an external operation using its durable identity and authenticated environment. Never infer identity from an arbitrary PID. |
| No safe retry | Record an indeterminate outcome and stop automatic dependent action until it is resolved or explicitly abandoned. |

An interrupted mutating shell command is not retry-safe merely because it returned no result. A successful spawn call is not proof that its job later completed. A checkpoint of partial output is not a terminal assistant message.

Ordinary provider/tool failures settle as task outcomes. Persistence uncertainty, corrupt durable state, or a violated mutation invariant fences the session and prevents further effects. A failing observer closes that observer, not the session. A task implementation panic must be surfaced and handled under a tested ownership policy; ignoring a JoinError is not recovery.

## 6. Cancellation, pause, and shutdown

Semantic cancellation is durable. The writer marks its exact target and scope, revokes the current invocation's normal write authority, and then signals execution. It joins the old invocation before permitting recovery or cancellation cleanup. Late completions are fenced by task ID and invocation generation.

Cleanup can release resources and settle cancellation evidence; it cannot start a replacement model turn or regain revoked permissions. Cancellation is not rollback. When an effect might already have happened, preserve that uncertainty rather than reporting a clean cancellation with invented certainty.

The race rule is explicit: settlement committed before the cancel mark wins; otherwise the normal completion plan is rejected and cleanup owns the outcome. Caller wait cancellation only removes the waiter. It does not cancel accepted work.

Pause prevents new model/tool dispatch in its declared scope and lets already-dispatched effects reach a safe boundary. The TUI shows pausing until that boundary; pause is not an immediate process stop. Cancellation remains available. Group cancellation records its admission barrier before traversing descendants, so concurrent spawn cannot escape the target subtree.

Tokio cancellation tokens carry process-local signals; they are not durable facts. Aborting or dropping a future does not establish that a subprocess or blocking operation stopped. In particular, running spawn_blocking work cannot generally be aborted. Keep bounded ownership and explicit join/escalation paths. [R9](docs/research.md#references)

Host close stops admission, quiesces or terminates owned execution according to adapter policy, flushes admitted commits, joins resources, and releases the ownership lock last. It preserves recoverable tasks rather than marking every task user-cancelled. Terminal restoration does not wait indefinitely for runtime cleanup; the host can continue cleanup after restoring terminal modes. Unsupported detach must not pretend work will continue after process exit.

## 7. History and context

Store append-only entries with identity, conversation, attribution, typed data, and an optional materialized provider-neutral model projection. Context changes are explicit stored controls: summary/head, reset/handoff, or omit/replace edits. Canonical history is not a mutable vector of provider payloads. [R1](docs/research.md#references)

A conversation fork records a source conversation and inclusive cutoff. Inherited entries keep their identity; later source appends, edits, and compactions are invisible. No live tasks, approvals, pending inbox, or execution authority are inherited. Cross-session export/import is a separate remapping operation, not a cheap in-session fork.

For each request, derive context from the fork-visible transcript, newest applicable head, and ordered edits. Preserve complete tool exchanges and source call order. A historical fork cutting an unfinished exchange may use explicitly labelled request-local missing-result errors, never fabricate successful results or restart source tools. Head movement cannot split a live exchange.

Compaction prepares a summary against a captured context cutoff and head revision. Commit validates the boundary and competing-head revision. Ordinary tail appends remain visible. Security/configuration changes are checked separately; compaction cannot revive revoked instructions or tools. The default implementation starts with boundary compaction; speculative compaction must pass its own races before being enabled.

Persist the effective managed instructions and tool definitions used at the request boundary, with their source revisions. Missing renderers must not make historical context unreadable. Unknown custom entry kinds retain their stored model projection and a safe generic display. Security decisions never rely on parsing prose.

Conversation state may be historical or current-only. Grant state, budget consumption, cancellation, and credentials never rewind with history. Fork initialization chooses history, configuration, and authority separately and records that choice.

Large sessions use indexed range/point queries. Current context and live execution may be resident; old transcripts and terminal tasks do not remain loaded simply because they were once viewed. Cold context construction can depend on candidate range, edit density, and fork depth, not just final prompt length. Do not claim O(context) performance without measuring those cases.

## 8. Agent turns and input

The default behavior remains request, tools, request again, finish. One foreground turn owns a conversation's input group and exchange boundaries. Its trusted generation/tool children share that turn authority. Unrelated tasks use input admission rather than directly appending model-visible content into an active exchange.

| Input mode | Active conversation | Idle conversation |
|---|---|---|
| Submit | Reject busy unless the caller explicitly selects another mode | Start a new turn |
| Steer | Join at the next safe model boundary, after the current tool exchange | Start a turn unless paused |
| Follow-up | Queue for a successor turn after the current turn settles | Start a turn unless paused |
| Queue-only | Remain queued for explicit start or later eligible submit | Remain queued |
| Notice | Retain an attributed notification; model visibility is explicit | No model wake-up |

Admission persists identity, sender, target, delivery mode, payload reference, and receipt. Placement into history and input-group ownership transfer are atomic. Acceptance, delivery, consumption by a request, and final answer are different facts. Every placed input eventually references a terminal answer or an explicit unanswered reason.

Tool calls carry stable call IDs. Independent read-only calls may execute concurrently within a bound. Mutating calls in a shared workspace serialize by default; broader concurrency requires a proven environment policy. Tool-name labels are not sandbox guarantees. Complete outcomes become durable immediately, even if earlier calls are still running; request projection presents results in call order and waits for the exchange's required settlements.

On turn cancellation, unplaced steer/follow-up inputs remain durable and paused for explicit withdrawal or resubmission; they do not silently start a new turn. Cancelling one turn does not automatically retire its agent or unrelated background jobs. The caller can explicitly include turn-owned jobs or the agent subtree.

Retries are bounded and classified, with durable attempt identity, deadline/backoff information, and usage uncertainty. A scheduling timer is not a busy-poll loop. Protocol-required status polling is allowed only in an owned adapter with a bounded backoff and cancellation path.

## 9. Optional agent groups

Multi-agent enablement controls the model-facing capability set. It does not create a different runtime. Disabling new spawning while workers exist does not hide them, lose their results, or remove the user's control surface.

Agent admission atomically records identity, supervisor, conversation seed, effective configuration, authority ceiling, budget account, workspace request, and initial assignment/input. Provisioning failure remains visible under that identity. Execution starts only after the environment and capability requirements are satisfied.

The initial model-facing control surface should cover spawn, inspect, send, wait, cancel, and result retrieval. Exact tool schemas are a milestone decision and must be evaluated for discoverability and context cost. The host API provides the same underlying operations. No special role-specific executors or model-dependent lifecycle states.

Fresh and forked contexts are explicit. Nested spawning is allowed only within depth, retained-agent, execution, budget, and permission bounds. Peer messaging is an explicit group capability independent of supervision. Supervisors control their subtree; ordinary peers do not acquire cancellation or read authority merely by knowing an ID.

Within a session, messages and target inbox admission can commit atomically under the same writer. Repeated delivery uses the same message identity. Future cross-session delivery needs an outbox/inbox acknowledgment protocol; it is not emulated by writing directly into another session's history.

A shared assignment board is optional. Claims and updates compare expected revisions; dependencies must remain acyclic. Advisory file scopes produce conflict warnings, not filesystem locks. Completing an assignment attaches result/evidence references and does not automatically approve or merge changes. The board is human-visible even when its model-facing tools are disabled. DSH is evidence for these distinctions, not a mandate to copy its delivery behavior. [R3](docs/research.md#references)

Agent inspection is read-only. Resume, restart, reassign, cancel, and retire are explicit and distinct. A retired agent remains inspectable; starting over creates a new identity and records lineage. A supervisor turn finishing does not kill its retained workers. Explicit supervisor retirement must reparent or settle its descendants first.

## 10. Capacity, budgets, and liveness

Separate retained identity from active execution capacity. Model-request permits are held only for real requests, not while a coordinator awaits workers. Process/job permits and output/disk limits are separate. Waiting must not consume the resource needed by the dependency.

Scheduling uses event-driven readiness with bounded fairness across agents. Admission, cancellation, settlement, and approval control traffic cannot be starved by streaming output. Bound queued inputs, retained agents, active tasks, pending approvals, output memory, and disk artifacts as well as model concurrency.

Budgets are hierarchical reservations, not copied balances. A child allocation reduces available parent capacity; charging a child is not charged twice again in totals. Check available balance and reserve an attempt atomically before dispatch. Settle with reported usage and release only the unused portion. After an uncertain external attempt, retain an uncertain debit/reservation until reconciliation or explicit administrative resolution; never restore a full balance simply because the process restarted.

Use integer token/usage counters and an explicit money unit, not floating-point balances. An estimated cost is not a billing guarantee. A hard spend policy requires a known conservative bound and an adapter that can enforce the relevant request limit; otherwise reject that policy or expose a token/request limit honestly. Provider price changes and unreported billing must remain visible.

Detect explicit dependency cycles and self-waits at admission. Apply cancellation/deadline semantics to waits. Hidden dependencies in user code or model plans cannot all be inferred; expose stalled work and the dependency evidence the runtime actually knows. Do not promise generic deadlock freedom.

## 11. Workspace and result integration

Workspace binding is independent of conversation and supervision. Support shared read-only inspection, shared mutation under policy, and isolated Git worktrees. The proposed default for parallel mutating workers is separate worktrees; read-only workers can share an environment. An unrestricted shell invalidates a claim of read-only execution unless the environment enforces it.

An isolated worker starts from an explicit base: a clean commit or a deliberately captured local snapshot. A worktree from HEAD does not contain arbitrary uncommitted or untracked user files. Dirty-checkout handling is a product decision surfaced before provisioning, not a hidden best guess.

A change result names the base revision/snapshot, resulting revision or patch artifact, changed paths, environment identity, and verification records. Verification binds to concrete revisions/content, command, exit status, relevant inputs, and output artifacts. A global workspace generation counter cannot prove that external edits did not happen.

Integration is a separate admitted effect. Recheck the target base and current dirty state; serialize Ion's integrations, show conflicts, preserve unrelated changes, and request approval when policy requires it. Never silently reset, overwrite, or discard the user's checkout. Tests that passed in a worker do not prove the integrated result passes. Reverify after application. Worktree removal requires confirmed result retention and explicit cleanup policy. [R6](docs/research.md#references)

## 12. Tools, providers, and extensions

Provider adapters translate canonical requests and streams. They expose capability information, supported input/output types, context limits, effort semantics, usage, and stop reasons. Unknown capabilities are not silently assumed. Provider/model identifiers are data, never architectural enum variants.

Preserve provider-native opaque continuation/reasoning data with its origin where needed, while retaining portable semantic history. Switching providers may invalidate acceleration or opaque continuation; report the limitation instead of claiming lossless transfer or fabricating reasoning. Freeze actual model identity and adapter/configuration revision for each attempt.

Tool admission resolves the exact tool implementation, validates arguments, evaluates authority/policy, captures execution and recovery class, reserves resources, and records intent before dispatch. Approval is bound to the canonical action, agent, workspace, grant revision, and argument digest. A later decision cannot approve different arguments or bypass a revoked grant.

Use compiled Rust traits for trusted runtime behaviors and provider/environment interfaces. User/agent-authored extensions start as versioned, supervised subprocess RPC with explicit contributions: tools, commands, skills/prompts, context providers, observational events, and limited typed hooks. MCP tools use the same admission/effect path. ACP is a frontend adapter, not a second agent loop.

Extensions do not receive raw storage mutation, terminal control, ambient credentials, or unrestricted session handles. Hooks return typed decisions. Pure preparation can repeat; effectful hooks must be idempotent or run as identified durable work. Cancellation, response-size limits, deadlines, and failure policy belong to each hook contract. Required security hooks fail closed; optional presentation failures must not stop a healthy agent.

Registration availability and durable authority differ. A transient peer outage does not revoke a grant. Explicit replacement/removal creates a new authority revision or tombstone. New effects and recovery recheck that revision; reusing a name does not resurrect an old grant. Prepare replacement privately, close conflicting admission, publish its grants/definitions atomically for the target session, then drain old resources. In-flight irreversible actions remain possible; state that boundary and report teardown failures.

Begin with a small effective tool set. Deferred discovery and programmatic tool calling are evaluated additions, not prerequisites for simple coding. Programmatic calls must cross the same capability, budget, approval, and durable-effect boundaries as direct calls. A persistent interpreter's heap is not durable session truth. OpenAI's Agents API makes these useful comparison topics without requiring Ion to depend on that managed service. [R8](docs/research.md#references)

## 13. Persistence and observation

Use SQLite on local storage for the first durable backend, with WAL, explicit FULL synchronization, foreign keys, ownership locking, and a supported patched SQLite version. Keep the backend interface narrow and private until another implementation proves its need. SQLite transactions protect the database; correct fsync/filesystem behavior remains part of the durability assumption. Backups must include committed WAL state through a supported backup path. [R10](docs/research.md#references)

Proposed logical records:

| Records | Required constraints/indexes |
|---|---|
| Session, agent, conversation | Stable identity; supervision/ancestry validation; current agent conversation and configuration revision |
| Entry | Immutable content; conversation/order, head, and kind indexes; bounded fork-aware range queries |
| Task and effect | Indexed live status, owner, dependencies, exact attempt identity, intent/checkpoint/outcome |
| Input/message/receipt | Unique scoped request key and message identity; ordered target inbox; explicit disposition |
| Grant and approval | Current authority revision, narrowing lineage, immutable approved action binding |
| Reservation and usage | Unique attempt charges, ancestor accounts, retained uncertain reservations |
| Assignment and artifact | Revision-based mutation; retained results; base/workspace/evidence references |

These are a logical schema, not an instruction to create every table before the first working slice. Version the actual schema and serialized kind payloads separately. Before an incompatible change, choose and test migration or explicit archive/refusal; never silently reinterpret old bytes or delete old sessions.

Large outputs use bounded buffers and owned spool/artifact files. Publish a durable file reference only after its content and required directory metadata are safely retained. A crash may leave a reclaimable orphan file, never a committed reference to missing required content. Truncation and quota exhaustion are explicit results.

Three classes of data remain distinct:

1. Semantic facts and control decisions: durable before acknowledgment/publication.
2. Recovery output/checkpoints: bounded, periodically persisted, never proof of completion.
3. Live display deltas: provisional and coalescible; loss is repaired from a fresh view or final durable content.

Do not fsync each token or rewrite an ever-growing complete message per token. Prototype P2 selects output batching/spill thresholds from measurements. Each output channel has one runtime owner, invocation identity, monotonic offsets, and explicit retention references. A foreground tool handing work to a background job transfers/references output durably before the tool finishes.

A watch atomically captures the requested view and registers subsequent delivery under the session writer. The snapshot includes its durable CommitSeq, current live output snapshots/cursors, and a process-local observation epoch. Durable events are whole commit envelopes. Provisional output frames carry channel/invocation identity and offsets. No ordering is inferred from wall-clock timestamps or adjacency of filtered commit numbers.

Overflow closes the affected subscription and requires a new atomic snapshot; it never blocks execution or silently drops a correctness-visible event while pretending the view is complete. Old-epoch frames reject. Final durable output replaces the matching provisional output exactly once. Only active context, live state, selected transcript pages, and bounded output need residency. Session/group summaries do not require loading every worker transcript.

## 14. Public contract and interoperability

Expose session/agent/conversation commands and bounded queries with stable IDs, typed errors, receipts, and explicit cancellation scopes. Keep database rows, codecs, invocation tokens, raw tool registries, and transaction builders crate-private unless a demonstrated embedding use requires otherwise.

A frontend attaches without becoming an execution owner. Prompt acceptance and eventual completion are separately observable. A compatibility adapter may wait for completion when its protocol requires that response shape. Pin the negotiated ACP schema/version and test its actual prompt, cancellation, permission, resume, and close semantics; do not assume all protocol revisions mean the same thing.

Default local access uses OS-local trust and restrictive socket/file permissions. Remote authentication, multi-user authorization, and distributed writable sessions are outside the first implementation. They require explicit designs before exposure, not an unauthenticated port added to the host.

## 15. Decision gates

The recommended direction is a single-owner, task-based durable runtime with typed agent behavior and integrated single/multi-agent clients. The following choices remain deliberately prototype-gated:

| Gate | Decision to resolve | Required evidence |
|---|---|---|
| P1 | Async task authoring versus re-entrant typed steps; transaction/ID representation | One turn, two tools, child wait, cancel/settle races, reopen, and clear Rust APIs without duplicated mechanisms |
| P2 | Output checkpoint cadence, channel sharing, spill thresholds, query/index strategy | Long output and deep forks; RSS, write amplification, restart cost, and loss-boundary tests |
| P3 | Extension hooks and reload authority | Replace/remove during execution; required-hook failure; no stale approval or grant resurrection |
| P4 | TUI layout, terminal substrate, and keymap | Main plus worker; narrow/wide layouts, target-safe input, approvals, overload, resize, and reconnect |

No existing Ion crate, renderer, ID scheme, or storage schema wins by default. Reuse is decided after these contracts, based on the cost and correctness of the implementation slice. No claim of optimality follows from a design document; the evaluation plan in ROADMAP.md supplies the evidence.
