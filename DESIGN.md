# Ion design

Status: proposed target architecture, revision 4, 2026-09-12 (America/Los_Angeles).

This document specifies the core agent/runtime Ion should become. It is not a description of the current implementation or a claim of demonstrated state-of-the-art performance. Architecture remains reopenable when implementation evidence disproves a choice.

Current Pico/Pi 2 work is the primary minimal-harness design reference. Codex is a primary production-engineering reference where its public implementation answers a concrete runtime, storage, multi-agent, or client question. Other agents are consulted only for specific unresolved mechanisms. Existing Ion code does not constrain the target.

[TERMINAL.md](TERMINAL.md) owns interaction and presentation. [Research](docs/research.md) records the broader source ledger. [Agent topology research](docs/research/agent-topology-context-2026-09-12.md) records the current worker/fork decision. [ROADMAP.md](ROADMAP.md) defines validation gates. [Core runtime migration](docs/core-runtime-migration.md) translates this design into implementation slices.

The scope is deliberately narrow: **agent execution, sessions, conversations, tools, durable state, workers, recovery, and clients**. Long-term/project knowledge, memory systems, shared task boards, semantic/vector stores, autonomous planning layers, and similar higher-level systems are not part of the core design. They may be evaluated later against a working baseline and must remain removable without changing core session correctness.

## 1. Product contract

Ion is a user-owned, provider-neutral Rust coding agent with a first-class terminal interface. It runs one primary agent thread by default. When multi-agent operation is enabled, that thread and the user may create, observe, steer, and supervise cooperating worker threads through the same runtime.

Root and workers use the same durable conversation/task machinery. Researcher, implementer, reviewer and similar roles are configuration/instruction choices rather than scheduler types or Rust subclasses.

Single-agent use must not pay a hidden multi-agent prompt/tool tax. Multi-agent capabilities are exposed only when enabled.

The core must support:

- complete coding turns with streaming, provider-neutral messages, files, shell/process work, images where supported, approvals, model changes, context management, durable history and crash recovery;
- workers with fresh or inherited context, nested delegation under explicit limits, messages/results and explicit workspace choices;
- human control of every active/retained worker through the same runtime semantics used by library/headless clients;
- local operation on macOS and Linux without a required cloud service, account, daemon or telemetry backend;
- local models as ordinary providers;
- deterministic, inspectable handling of acceptance, cancellation, uncertain external effects and process loss.

Runtime capability and model strategy are separate. The core does not mandate planning, reflection, voting, task boards, memory retrieval, role taxonomies or a swarm policy.

## 2. Architecture

Use a small durable execution runtime with one semantic mutation owner per session, typed behavior implementations, provider-neutral conversations, explicit effect adapters and bounded client projections.

```text
frontends / adapters
TUI / CLI / JSON / ACP / Rust API
              |
              v
+-------------------------------+
| Host                          |
| residency / locks / providers |
| credentials / client attach   |
+---------------+---------------+
                |
                v
+-------------------------------+
| Session owner                 |
| one serialized mutation line  |
| commands / scheduling / view  |
+----------+--------------------+
           |
           | short atomic commits
           v
+-------------------------------+
| Canonical session store       |
| conversations / entries       |
| inputs / tasks / effects      |
| authority / usage / refs      |
+-------------------------------+
           ^
           |
           | post-commit settlement
           |
+----------+--------------------+
| Effect adapters               |
| model / tool / process / job  |
| timers / user approval        |
+-------------------------------+
```

Slow work never executes while holding session mutation authority or a database write transaction. Provider requests, tools, subprocesses, timers, network calls and user interaction run concurrently outside the writer.

One transition is:

```text
read committed state
  -> validate command
  -> build bounded mutation batch
  -> commit atomically
  -> publish observation
  -> dispatch admitted effects outside the writer
```

This supplies concurrency without multiple competing authorities for canonical session truth.

## 3. Core domain model

| Concept | Meaning |
|---|---|
| Session | Durable coordination, ownership and transaction boundary containing one primary conversation and any related worker/branch conversations. |
| Conversation | Durable agent thread: immutable transcript, model-context controls, configuration/authority/workspace state, optional history provenance and optional execution owner. |
| Turn | One foreground input-to-answer lifecycle represented by a root task and dependent tasks. |
| Entry | Immutable semantic transcript fact. |
| Input | Admitted user/agent input with sender, target, mode, request identity and disposition. |
| Task | Recoverable unit of execution with typed kind state, dependencies, invocation generation and terminal outcome. A durable task is not a Tokio task. |
| Effect | Durable external-work intent plus attempt/recovery classification and settlement. |
| Job | Environment-backed work represented through durable tasks/effects and allowed to outlive its initiating turn when explicitly backgrounded. |
| Artifact | Retained content/evidence stored inline or by durable external reference. |

### No separate durable Agent object initially

A model-visible or human-visible worker is a conversation with execution ownership/configuration. The public API may expose an `Agent`/`Worker` handle, but it wraps the same durable `ConversationId`; it is not a second canonical row.

Do not add `AgentId -> current ConversationId` merely for taxonomy. A separate durable agent identity becomes justified only if a concrete requirement appears for one participant to own several simultaneously distinct conversations while preserving one identity across them.

Current needs do not establish that requirement:

- related follow-ups continue the same conversation;
- reset/handoff can start a clean model context while preserving durable history;
- a genuine branch is naturally another conversation;
- parallel branches should be independently addressable anyway.

The session stores `primary_conversation_id`. The primary/root conversation has no execution owner. Worker conversations are ordinary conversations created by tasks.

## 4. Relationships are separate graphs

Do not represent Ion as one overloaded agent tree.

```text
history ancestry:     Conversation --parent/cutoff--> Conversation
execution ownership: Task --owns--> Conversation
execution ordering:  Task --depends-on--> Task       (DAG)
workspace relation:  Conversation/Task --binds--> Workspace
message flow:         Input(sender,target)
```

A conversation may have both a history parent and an owner, but those edges mean different things.

- History ancestry determines inherited immutable transcript/rewindable state.
- Ownership determines execution/control scope and provenance.
- Task dependencies determine readiness, not supervision.
- Workspace binding determines what external state the work observes/mutates.

A history fork grants neither cancellation authority nor execution ownership. Sharing a workspace grants neither history nor supervision.

### Ownership through the creating task

A task-created child records its immutable owner task in the same commit that creates the conversation. Because every task belongs to a conversation, control ancestry can be derived:

```text
parent conversation
  -> spawning task
      -> owns child conversation
```

The owner task may later become terminal; the ownership/provenance edge remains. This lets a background worker stay addressable after its spawning tool/turn finishes without inventing a separate retained-agent object.

Host-created conversations/forks may have no owner and are independently controlled inside the same session.

## 5. Sessions are consistency domains, not context windows

A session is deliberately larger than one conversation. Cooperating root/worker conversations stay in one session because ordinary transitions may need atomic invariants across them:

- child conversation + initial input/task creation;
- message/input admission;
- task dependency/successor creation;
- ownership/cancellation barriers;
- authority narrowing;
- resource/budget accounting;
- workspace ownership/conflict metadata;
- observations derived from one commit.

Do not create one canonical database per worker. That would turn common group transitions into cross-database protocols.

Independent top-level sessions need no such atomicity and may have independent owners/stores.

Use a separate top-level session when the lifecycle/consistency domain is actually independent: a separate user goal/project, security/credential boundary, independently archived/deleted workspace, or future remote ownership domain. Do **not** create a separate session merely to give a worker clean model context.

## 6. Identity and ordering

Use distinct Rust newtypes for semantic identities. `SessionId` is globally unique. Conversation, entry, input, task, effect and artifact identities are scoped by the session unless a concrete external boundary requires otherwise.

`CommitSeq` orders successful atomic session mutations. It is not object identity.

The physical representation of local IDs remains a P1/P2 decision. Compact session-local integers are attractive once per-session storage is established; UUID-style identities avoid central allocation. Public behavior must not depend on the representation.

IDs created by a command cannot escape to callers or external effects before the creating transaction commits. Persistence uncertainty fences the current session handle; reopen and recover durable state rather than guessing.

## 7. One writer per session

One loaded session has one authoritative semantic writer. Concurrent independent writers to the same session are not supported.

The host acquires an exclusive cross-process session lock before writable open and holds it through shutdown. A PID file, heartbeat or stale timestamp is not ownership.

A command carries target identities, authority and, where relevant, expected revision/request key. One transition:

1. validates lifecycle, authority, revision and duplicate-request identity;
2. reads required committed state;
3. constructs a bounded typed batch;
4. validates cross-record invariants against committed state plus earlier mutations in that batch;
5. commits the batch;
6. publishes committed observations;
7. dispatches effects outside mutation authority.

Task settlement, successor creation and required ownership transfer commit together. Scheduling/observers never see a false idle gap.

SQLite may block a dedicated storage worker; the storage worker is persistence, not another semantic owner.

## 8. Durable tasks

The generic lifecycle stays small:

```text
pending/ready -> running/inflight -> waiting or terminal
```

Terminal outcomes distinguish at least completed, failed, cancelled and indeterminate. Domain phases live inside typed task-kind state; the scheduler does not understand generation/tool/compaction semantics.

A task records kind/schema revision, immutable input, owning conversation, optional provenance task, dependencies, invocation generation, typed checkpoint/state, output refs, terminal outcome and timing/cancellation metadata where needed.

P1 selected a typed re-entrant step/checkpoint boundary as the durable task authoring model. A step inspects committed state and returns a bounded transition plan such as effects to admit, durable waits/dependencies or terminal/successor work.

Async Rust remains inside model/tool/process/timer adapters. An async stack frame is never the durable continuation.

Dependencies mean named tasks must become terminal before a dependent becomes eligible; the dependent interprets their outcomes. Reject self-dependency and cycles.

Waiting for a child is durable dependency/continuation state, not a future that consumes execution capacity for the child's lifetime.

## 9. Turns and tool exchanges

The default turn remains:

```text
input
  -> generation
      -> zero or more tools
          -> join/post-tools
              -> next generation or final answer
```

When generation returns `[A, B, C]`, one atomic settlement creates all tool tasks plus their join before the generation becomes terminal.

```text
generation G
     |
     +--> tool A --+
     +--> tool B --+--> join P --> generation G2
     +--> tool C --+
```

Tools settle independently in completion order. Model projection restores source call order without rewriting transcript history.

Independent read-only tools may run concurrently. Mutating calls against the same workspace serialize by default unless environment policy proves a stronger safe scheme.

A tool settles only itself. The join owns exchange continuation.

## 10. Effects and uncertainty

Repeat-sensitive external work requires durable intent before dispatch.

Effect identity and attempt identity are separate. Recovery class is explicit:

| Class | Recovery |
|---|---|
| Retry-safe | A new attempt may execute; prior possible usage/billing remains recorded. |
| Reconcile | Query/adopt the external operation using durable authenticated identity. |
| No safe retry | Mark indeterminate and stop dependent automatic action until explicitly resolved/abandoned. |

An interrupted mutating shell command is not retry-safe merely because no result persisted.

A running task found after process loss invokes recovery. Opening/inspection starts no work; drive/resume is explicit.

Ordinary provider/tool failures become task outcomes. Persistence uncertainty, corrupted canonical state or invariant violation fences the session.

## 11. Inputs and idempotency

Input admission is separate from eventual execution/completion.

An input records identity, sender, target conversation, mode, payload, optional request key and disposition.

Initial modes:

| Mode | Active conversation | Idle conversation |
|---|---|---|
| Submit | reject busy unless caller selects another mode | start turn |
| Steer | place at next safe model boundary | start turn unless paused |
| Follow-up | queue successor input | start turn unless paused |
| Queue-only | remain queued | remain queued |
| Notice | retain attributed notification; model visibility explicit | no automatic wake |

Repeating the same request key with equivalent target/content/mode returns the original durable receipt. Reusing a key with different semantics rejects `IdempotencyConflict`.

Caller disappearance before admission creates no work. Once admission begins, dropping the response future cannot abandon the commit. Cancelling a caller wait does not cancel accepted work.

Acceptance, placement, model consumption and final answer are distinct facts.

Inter-agent communication should use this same input/message substrate rather than a second mailbox truth model. Sender and target conversation identities are explicit.

## 12. Conversation history and model context

Canonical history is append-only semantic entries, not a mutable provider-message vector.

Model context is a derived/explicitly controlled projection of that history. Context controls remain constrained so they cannot create impossible provider exchanges: summary/head/handoff/reset and targeted omission/replacement where justified.

Compaction appends durable summary/handoff information and advances context against a captured boundary. It never mutates old entries.

Old transcript entries and terminal tasks remain queryable but do not stay resident merely because they were once observed. P2 measures long histories, context edits and deep forks before complexity claims become contractual.

### Historical forks

A historical fork creates another conversation whose logical transcript shares a source prefix through a stable cutoff; new entries append locally. Later source changes are invisible and source tasks are never inherited.

Fork provenance is history only. A user/API-created alternate branch can therefore have a history parent and no execution owner.

For worker creation, inherited context should resolve to a safe complete model exchange boundary. Do not launch a child with half of an active tool exchange merely because storage can name such a cutoff.

Configuration/authority/workspace inheritance is independent from history inheritance.

## 13. Worker creation: context and lifetime are orthogonal

A worker is an owned conversation. Two independent choices matter.

### Context seed

**Fresh**: no history parent. The child receives its explicit task prompt plus selected configuration/authority/workspace initialization.

Prefer fresh when the work is self-contained, exploratory, an independent review, a parallel module with a clear interface, or when inherited reasoning would mostly add stale/noisy context.

**Inherited**: the child records a history parent/cutoff and begins from the parent's effective historical context at a safe boundary.

Prefer inherited when the task depends on nuanced prior requirements/decisions, is a direct continuation/debug thread, or needs an exact common base for alternative solutions.

Context inheritance is not automatically better. Full history costs tokens and can carry stale observations and correlated assumptions. The initial baseline should therefore favor clean context for independent delegation and make inheritance explicit; P4/M6-style effectiveness tests may refine the model-facing default.

Reuse an existing worker conversation for a closely related follow-up when its accumulated context is genuinely useful. Reset/handoff that conversation if it has become noisy rather than creating a second identity merely for context hygiene.

### Lifetime/dependency

**Joined/foreground delegation**: the parent task cannot complete until the child's required result is terminal. Cancellation policy may include the child.

**Retained/background spawn**: creation returns the child ID and the creating task may settle immediately; the child remains addressable and continues under session serving. Parent-turn completion does not retire it.

A later wait uses durable dependency/continuation state and does not hold the capacity the child needs.

These are not separate worker types. The same conversation/task schema represents both.

### Small control surface

The model/human control surface should remain small:

```text
run/spawn
send/follow-up
inspect/status
wait
interrupt/cancel
retire
```

Exact tool schemas are evaluated later. They are commands over the same conversation/task runtime, not a swarm subsystem.

## 14. Delegation policy

The root/primary conversation is the default synchronizer and validation bottleneck. Multi-agent execution is optional and should be conservative.

Delegate concrete bounded work when decomposition or parallelism is useful. Prefer direct work for tightly sequential reasoning, trivial tool calls, single-file edits or tasks where coordination overhead is likely to dominate.

Initial useful patterns:

| Worker purpose | Preferred context | Workspace |
|---|---|---|
| repo explorer/researcher | fresh | shared read-only |
| independent reviewer | fresh | shared read-only or immutable diff |
| test/failure investigator | fresh unless prior diagnostics are essential | shared read access |
| independent implementation slice | usually fresh | isolated worktree/snapshot |
| continuation/debug specialist | inherit or reuse | policy-dependent |
| alternate approach from same decision point | inherited/forked | isolated if mutating |

Do not automatically expose every worker transcript to every model. Shared persistence is not shared model context. Workers communicate through explicit inputs/results; supervisors may inspect their subtree through bounded queries.

## 15. Authority and workspace

Authority is structured runtime state, not prose.

Child effective authority is:

```text
requested capability
INTERSECT owner/supervisor ceiling
INTERSECT host policy
```

History forks, model changes, plugin reload or identifier reuse cannot widen it.

Approvals bind exact action/effect identity and relevant arguments/revision.

Workspace binding is independent of context history. Multiple read-only workers may share one workspace. Parallel mutating workers normally use isolated workspaces/worktrees unless a stronger safe conflict policy is proven.

Integrating a worker result is a separate admitted effect with current base/dirty-state validation and post-apply verification.

## 16. Cancellation, pause and shutdown

Semantic cancellation is durable.

The writer records exact scope, revokes the current invocation generation's normal write authority, commits, then signals local execution. Late normal completions are fenced by task identity + invocation generation.

```text
settlement committed before cancel mark -> settlement wins
otherwise -> normal completion rejected; cancellation cleanup owns outcome
```

Cancellation is not rollback. External uncertainty remains explicit.

Subtree/group cancellation establishes an admission barrier before traversing descendants so concurrent child creation cannot escape the scope.

Pause prevents new effect dispatch in scope and lets already-dispatched work reach defined safe boundaries.

Host close stops admission, drains admitted writes, preserves recoverable unfinished tasks, joins owned resources and releases the session lock last.

## 17. Observations and frontends

Frontends consume one command/observation contract. Focus is presentation state, not execution identity.

A subscription captures a bounded atomic snapshot and then committed events/live-output frames from that point. Durable ordering derives from `CommitSeq`, not wall time.

Live output is provisional presentation with channel/invocation identity and offsets; overflow requires resnapshot rather than silently pretending completeness. Final durable output replaces matching provisional presentation once.

Per-conversation drafts and delayed replies are keyed by captured target IDs. Changing UI focus cannot reroute an already submitted command.

Session summaries must be obtainable without loading every worker transcript.

## 18. Persistence

One session has one canonical crash-atomic store boundary. Every record that can participate in one session command belongs together semantically: conversations/history, inputs, tasks/dependencies, effects, authority/approvals, usage/budgets, workspace correctness metadata and artifact references.

Leading physical topology:

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/

  # optional rebuildable discovery cache if scale justifies it
  catalog.sqlite
```

Independent sessions then have independent writer/WAL/checkpoint/failure domains. All conversations inside one session share the database so coordination stays transactional.

Plain SQLite is the default engine candidate. Its one-writer-per-database rule matches Ion's intentional one semantic writer per session, provided transactions stay short. Turso remains a P2 comparison candidate if measurements or a real sync requirement justify it; concurrent database writers do not replace Ion-level ordering semantics.

Do not split one session transaction across category-specific WAL databases.

Small semantic payloads required for exact replay may stay inline. Large opaque output may spill to files with durable references/integrity metadata. Prefer reclaimable orphan files after a crash over committed references to missing required content.

Separate semantic facts, recovery checkpoints/output and provisional live display deltas. Do not fsync every token or rewrite an ever-growing full message per streaming frame; P2 chooses checkpoint/spill policy from measurement.

No knowledge/memory/vector/task-board store is part of this core topology.

## 19. Public contract

Expose session/conversation(agent-thread) commands and bounded queries with typed IDs, receipts, typed errors and explicit cancellation scopes.

The public UX may say agent/worker while the durable identity remains `ConversationId`. Avoid exposing the physical schema or a second identity simply to satisfy naming.

Keep database rows/codecs, transaction builders, invocation tokens and raw registries private unless a demonstrated embedding need requires them.

A frontend attaches without becoming execution owner. Acceptance and eventual completion are separately observable.

Default local access uses OS-local trust and restrictive file/socket permissions. Remote authentication, multi-user authorization and distributed writable sessions require separate designs.

## 20. Validation gates

No design becomes optimal merely because it is written here.

| Gate | Decision/evidence |
|---|---|
| P1 | Promote typed durable tasks into production; validate idempotent input, concurrent tool tasks, ownership/waits, cancellation races, recovery and missing-kind behavior. |
| P2 | Validate per-session physical topology, SQLite behavior, output/checkpoint policy, indexed history/forks, active-memory bounds, backup/repair and optionally Turso under the same workload. |
| P3 | Validate extension/tool/behavior contribution boundaries, authority/reload/failure semantics and sandbox assumptions. |
| P4 | Validate TUI/group interaction, target-safe input, fresh/inherited worker flows, approvals, narrow layouts, overload, reconnect and terminal restoration. |

After a working core exists, effectiveness evaluation must compare at least single-agent direct work, fresh delegated workers, inherited/reused workers and bounded multi-worker strategies under equal model/token/time budgets.

Higher-level knowledge/memory/task-board systems remain outside these gates. Durable shared sessions, worker transcripts/results, context reset/handoff and repository state are the baseline they would have to beat.

If evidence shows a core decision is wrong, change the design and implementation. Do not preserve it for compatibility with unfinished internal code.