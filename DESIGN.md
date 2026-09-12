# Ion design

Status: proposed target architecture, revision 3, 2026-09-12 (America/Los_Angeles).

This document specifies the core agent/runtime Ion should become. It is not a description of the current implementation or a claim of demonstrated state-of-the-art performance. Architecture remains reopenable when implementation evidence disproves a choice.

Current Pico/Pi 2/Pico v3 work is the primary minimal-harness design reference. Codex is a primary production-engineering reference where its public implementation answers a concrete runtime, storage, multi-agent, or client question. Other agents are consulted only for specific unresolved mechanisms. Existing Ion code does not constrain the target.

[TERMINAL.md](TERMINAL.md) owns interaction and presentation. [Research](docs/research.md) records sources and evidence limits. [ROADMAP.md](ROADMAP.md) defines validation gates. [Core runtime migration](docs/core-runtime-migration.md) translates this design into implementation slices.

The scope here is deliberately narrow: **agent execution, sessions, conversations, tools, durable state, retained workers, recovery, and clients**. Long-term/project knowledge, memory systems, shared task boards, semantic/vector stores, autonomous planning layers, and similar higher-level systems are not part of the core design. They may be evaluated later against a working baseline and must remain removable without changing core session correctness.

## 1. Product contract

Ion is a user-owned, provider-neutral Rust coding agent with a first-class terminal interface. It runs one agent by default. When multi-agent operation is enabled, the root agent and user may create, observe, steer, and supervise cooperating retained workers through the same runtime.

A worker is an ordinary agent with its own identity, conversation, configuration and authority. Researcher, implementer, reviewer, and similar roles are behavior/configuration choices rather than scheduler types.

Single-agent use must not pay a hidden multi-agent prompt/tool tax. Multi-agent capabilities are exposed only when enabled.

The core must support:

- complete coding turns with streaming, provider-neutral messages, files, shell/process work, images where supported, approvals, model changes, context management, durable history and crash recovery;
- retained workers with fresh or forked context, nested delegation under explicit limits, direct messages/results and explicit workspace choices;
- human control of every retained agent through the same runtime semantics used by library/headless clients;
- local operation on macOS and Linux without a required cloud service, account, daemon or telemetry backend;
- local models as ordinary providers;
- deterministic, inspectable handling of acceptance, cancellation, uncertain external effects and process loss.

Runtime capability and model strategy are separate. The core does not mandate planning, reflection, voting, a task board, memory retrieval, role taxonomies, or a swarm policy. Those may be evaluated later without changing the execution kernel.

## 2. Architecture

Use a small durable execution runtime with one semantic mutation owner per session, typed behavior implementations, provider-neutral conversation state, explicit effect adapters and bounded client projections.

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
| entries / context / agents    |
| inputs / tasks / effects      |
| grants / usage / references   |
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

Layer responsibilities:

| Layer | Owns | Does not own |
|---|---|---|
| Host | loaded-session residency, cross-process ownership locks, provider/plugin/process lifetimes, credentials, client attachments | model decisions, canonical transcript |
| Session owner | serialized semantic mutation, durable acceptance, task lifecycle, dependency scheduling, invocation fencing, authority checks, observations | provider I/O, terminal layout |
| Behavior | typed task-specific state and transitions such as generation, tool join, compaction | raw database mutation, a second scheduler |
| Environment/effects | files, commands, jobs, sandbox/workspace enforcement, external APIs | agent supervision or conversation truth |
| Frontends | drafts, focus, rendering, inspection and explicit commands | providers, persistence, hidden execution state |

Logical separation does not require one process or crate per layer.

### Core principle: concurrency outside mutation

Slow work must never execute while holding session mutation authority or a database write transaction. Provider requests, tools, subprocesses, timers, network calls and user interaction run concurrently outside the writer.

The writer performs short transitions:

```text
read committed state
    -> validate command
    -> build bounded mutation batch
    -> commit atomically
    -> publish observation
    -> dispatch admitted effects outside the writer
```

This lets Ion support many concurrent effects and workers without creating multiple competing authorities for canonical session state.

## 3. Domain model

| Concept | Meaning |
|---|---|
| Session | Durable coordination, ownership and transaction boundary containing one root agent and its cooperating retained workers. |
| Agent | Addressable retained participant with configuration, authority, workspace binding and current conversation. |
| Conversation | Immutable transcript plus explicit model-context state and optional historical fork provenance. |
| Turn | One foreground input-to-answer lifecycle represented by a root task and its dependent tasks. |
| Input | Admitted user/agent input with sender, target, delivery mode, request identity and disposition. |
| Task | Recoverable unit of execution with typed kind state, dependencies, invocation generation and terminal outcome. A durable task is not a Tokio task. |
| Effect | Durable intent for external work plus attempt/recovery classification and settlement. |
| Job | Environment-backed work that may outlive the initiating tool/turn and is represented through durable tasks/effects. |
| Entry | Immutable semantic transcript fact. |
| Artifact | Retained content/evidence stored inline or by durable external reference. |

Keep these relationships separate:

```text
conversation ancestry
agent supervision
task dependencies
workspace sharing
```

A history fork grants neither supervision nor authority. A task dependency does not imply conversation ancestry. Sharing a workspace does not make two agents one execution identity.

### Session versus agent

A session is intentionally larger than one agent. Root and retained workers that cooperate closely belong to one session because ordinary operations may need to commit atomically across them:

- spawn identity + conversation + initial input/task;
- message admission;
- task dependencies and successor creation;
- supervision/cancellation barriers;
- authority narrowing;
- resource/budget accounting;
- workspace ownership/conflict metadata.

Do not create one canonical database per agent. That would turn common same-group transitions into cross-database coordination problems.

Independent top-level sessions need no such atomicity and may run concurrently under separate owners.

## 4. Identity and ordering

Use distinct Rust newtypes for semantic identities. `SessionId` is globally unique. Agent, conversation, entry, input, task, effect and artifact identities are scoped by the session unless a concrete external boundary requires global identity.

`CommitSeq` orders successful atomic session mutations. It is not an object's semantic identity.

The physical representation of local IDs remains a P1/P2 implementation decision. Compact session-local integers are attractive once the per-session store is established; UUID-style identities avoid central allocation. Do not expose representation-dependent behavior in public APIs.

IDs created by a command must not escape to callers or external effects before the creating transaction commits. A persistence outcome that is uncertain fences the current session handle; reopen storage and recover the last durable state rather than guessing which IDs or effects committed.

## 5. One writer per session

One loaded session has one authoritative semantic writer. Concurrent independent writers to the same session are not supported.

The host acquires an exclusive cross-process session ownership lock before writable open and holds it through shutdown. A PID file, heartbeat or stale timestamp is not ownership. Never perform a timed takeover while the prior writer may still execute.

A command carries its target identities, authority and, where relevant, expected revision/request key. One transition:

1. validates lifecycle, authority, revision and duplicate-request identity;
2. reads the required committed state;
3. constructs a bounded typed mutation batch;
4. validates cross-record invariants against committed state plus earlier mutations in the batch;
5. persists the complete batch;
6. applies/publishes committed observations;
7. dispatches effects outside mutation authority.

Task settlement, successor creation and required ownership transfer commit together. Observers and scheduling must not see a false idle gap between them.

The storage worker may block on SQLite. It is a persistence mechanism, not a second semantic owner. Reads that inform a later write carry revisions or are repeated/revalidated on the writer.

## 6. Durable tasks

The generic lifecycle is deliberately small:

```text
pending/ready -> running/inflight -> waiting or terminal
```

Terminal outcomes distinguish at least completed, failed, cancelled and indeterminate. Domain-specific phases live inside the task kind's typed state; the scheduler does not know generation/tool/compaction semantics.

A task records:

- kind and schema revision;
- immutable input;
- owning agent/conversation;
- optional parent/provenance task;
- dependencies;
- invocation generation;
- typed durable checkpoint/state;
- output references;
- terminal outcome;
- timing/permit/cancellation metadata where needed.

P1 selected an explicit typed re-entrant step/checkpoint boundary as the durable task authoring model. A behavior step inspects committed state and returns a bounded transition plan such as:

```text
create/admit effects
persist wait/dependencies + checkpoint
complete with outcome + successor plan
```

Async Rust remains appropriate inside model/tool/process/timer adapters. An async stack frame is not the durable continuation. Do not maintain a second public async-task framework.

### Dependency semantics

Dependencies mean that named tasks must reach terminal state before a dependent becomes eligible. The dependent interprets whether those terminal outcomes represent success/failure acceptable for its behavior.

Dependency updates must reject self-dependency and cycles.

Waiting for a child is therefore durable state, not a Tokio future held for the child's entire lifetime.

## 7. Turns and tool exchanges

The default coding turn remains conceptually simple:

```text
input
  -> generation
      -> zero or more tools
          -> join/post-tools
              -> next generation or final answer
```

When one generation returns calls `[A, B, C]`, one atomic settlement creates the tool tasks and their join task before the generation becomes terminal.

```text
generation G
     |
     +--> tool A --+
     +--> tool B --+--> join P --> generation G2
     +--> tool C --+
```

Tools may complete in any order. Each result becomes durable immediately when settled. Model projection restores source call order rather than rewriting transcript history to completion order.

Independent read-only tools may execute concurrently within resource limits. Mutating calls against a shared workspace serialize by default unless the environment can prove a stronger safe policy. Tool-name labels alone are not isolation guarantees.

A tool settles only its own task/effect. The join task owns exchange continuation; tools do not scan siblings or manufacture the next generation after noticing they happened to be last.

## 8. Effects and uncertainty

External work requires durable intent before dispatch whenever repeating it may matter.

Effect identity and attempt identity are separate. Recovery class is explicit:

| Class | Recovery |
|---|---|
| Retry-safe | A new attempt may execute. Prior possible usage/billing is retained. |
| Reconcile | Query/adopt the external operation using durable authenticated identity. |
| No safe retry | Mark indeterminate and stop dependent automatic action until explicitly resolved/abandoned. |

An interrupted mutating shell command is not retry-safe merely because no result was persisted. A spawn effect succeeding does not prove the spawned job later completed.

A running task found after process loss invokes its recovery behavior. Opening or inspecting a session dispatches no work; `drive`/resume is explicit.

Ordinary provider/tool failures become task outcomes. Persistence uncertainty, corrupted canonical state or invariant violation fences the session against further effects.

## 9. Input admission and idempotency

Admission is separate from eventual execution/completion.

An input records identity, sender, target agent/conversation, mode, payload reference/content, request key when supplied and disposition.

Core modes initially cover:

| Mode | Active conversation | Idle conversation |
|---|---|---|
| Submit | Reject busy unless another mode is selected | start turn |
| Steer | place at next safe model boundary | start turn unless paused |
| Follow-up | queue successor input | start turn unless paused |
| Queue-only | remain queued | remain queued |
| Notice | retain attributed notification; model visibility explicit | no automatic wake |

Repeating the same request key with equivalent target/content/mode returns the original durable receipt. Reusing the key for different semantics rejects with `IdempotencyConflict`.

A caller disappearing before admission creates no work. Once admission has begun, dropping its response future cannot abandon the in-progress commit. Cancellation of a caller's wait is not cancellation of accepted work.

Acceptance, placement, model consumption and final answer are distinct facts. Every placed input eventually references a terminal answer or an explicit unanswered disposition.

## 10. Conversations, history and context

Canonical history is append-only semantic entries, not a mutable vector of provider messages.

A conversation owns an immutable transcript. Model context is explicit state selecting/projecting transcript material. Projection is provider-neutral and side-effect free.

Context operations should remain small and constrained:

```text
append selected new entries
replace a context prefix with a summary/handoff
reset to an empty or bootstrap context
```

Do not expose arbitrary history reordering that can split tool exchanges or produce impossible provider state.

Compaction appends a summary/handoff entry and updates context atomically against a captured context/revision boundary. Tail entries appended after summary preparation remain visible; competing context rewrites invalidate stale preparation when required.

### Forks

A historical fork shares immutable source history through a committed boundary, then appends locally. Later source changes do not affect it.

Forking history does not copy live tasks, pending approvals, cancellation state, credentials or execution authority.

A fork target resolves to a complete committed historical boundary. Do not expose an impossible partial transaction as a historical snapshot. A fork used as model context must not split an unresolved tool-call/result exchange.

Configuration/authority inheritance and history inheritance are separate choices.

### Residency

Old transcript entries and terminal tasks remain queryable but do not stay resident merely because they were once observed. Live execution may retain its active state/context; cold history uses indexed point/range reads.

P2 must measure large histories, dense context edits and deep forks before claims about memory or context-build complexity become contractual.

## 11. Retained agents

Multi-agent mode uses the same runtime and task system, not a separate swarm engine.

Agent creation atomically records the retained identity and the state needed to make that identity meaningful: supervisor/root authority, conversation seed, effective configuration/authority ceiling, workspace request and initial input/task where applicable.

A spawn task/tool may finish while the retained worker remains alive and addressable.

The minimal control surface is:

```text
spawn
inspect
send
wait
cancel
get result/status
```

Inspection is read-only and never resumes work.

Nested spawning is constrained by explicit depth, retained-agent, active-execution, budget and permission limits. Peer communication does not grant supervision authority merely because an agent knows another ID.

Waiting on another agent/task parks as durable dependency/continuation state and must not retain the execution capacity required by the dependency.

A supervisor turn finishing does not automatically destroy retained workers. Retirement/cancellation is explicit.

## 12. Authority, approvals and workspace

Authority is structured runtime state, not prose inferred from a transcript.

Child effective authority is bounded by:

```text
requested capability
INTERSECT parent ceiling
INTERSECT host policy
```

Model/configuration changes, history forks, plugin reload or identifier/name reuse cannot widen authority.

Approvals bind the exact action/effect identity and relevant arguments/revision. A stale approval cannot authorize a materially changed action.

Workspace binding is independent of agent role and history source. Multiple read-only agents may share one workspace. Parallel mutating agents normally use explicitly isolated workspaces/worktrees unless a stronger conflict policy is selected.

Applying or integrating a worker result is a separate admitted effect with current base/dirty-state validation and post-apply verification.

## 13. Cancellation, pause and shutdown

Semantic cancellation is durable.

The writer records the exact cancellation scope, revokes the current invocation generation's normal write authority, commits, then signals local execution. Late normal completions are fenced by task identity + invocation generation.

Race rule:

```text
settlement committed before cancel mark -> settlement wins
otherwise -> normal completion rejected; cancellation cleanup owns outcome
```

Cancellation is not rollback. If an external effect may already have happened, preserve uncertainty.

Group/subtree cancellation must establish its admission barrier before traversing descendants so a concurrent spawn cannot escape the selected scope.

Pause prevents new effect dispatch in scope and lets already-dispatched work reach defined safe boundaries. It is not equivalent to killing a process.

Tokio cancellation tokens are process-local signals, not durable facts. Dropping/aborting a future does not prove a subprocess or blocking action stopped.

Host close stops new admission, drains/settles owned writes as defined by adapter policy, preserves recoverable unfinished tasks, joins owned resources and releases the session ownership lock last.

## 14. Observations and frontends

Frontends consume the same command/observation contract. TUI focus is presentation state, not execution identity.

A watch/subscription captures an atomic bounded snapshot and then committed events/live-output frames from that point. Durable event ordering derives from `CommitSeq`, not wall-clock time.

Live streaming output is provisional presentation. It carries invocation/channel identity and offsets. It may be coalesced. Overflow closes/resets the affected subscription and requires a fresh snapshot rather than silently dropping correctness-visible state.

Final durable output replaces the matching provisional presentation once.

Per-agent drafts and delayed replies are keyed by stable captured target IDs. Changing UI focus cannot reroute an already submitted command.

Session/group summaries must be obtainable without loading every worker transcript.

## 15. Persistence

### Semantic boundary

One session has one canonical crash-atomic store boundary. All records that may participate in one session command transaction belong together semantically.

Logical records include:

- session metadata;
- retained agents and conversations;
- immutable entries and context controls;
- inputs/messages/request receipts;
- tasks and dependencies;
- effect intents/attempts/settlements;
- authority/grants/approvals;
- reservations/usage/budgets;
- workspace/resource metadata required for correctness;
- artifact references.

This does not require one table for every bullet. The schema should follow query/constraint evidence rather than object-oriented table proliferation.

### Leading physical topology

The strongest current candidate is one database file per top-level session/group:

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/

  # optional, rebuildable discovery cache if scale justifies it
  catalog.sqlite
```

Independent sessions then have independent writer/WAL/checkpoint/failure domains. Root agent and workers inside one session share the same database so their coordination remains locally transactional.

The current production implementation uses one database per Ion data root. P2 must compare it against the per-session candidate before physical migration is declared final.

Do not split one core session transaction into category-specific WAL databases. SQLite does not provide host-crash atomicity across multiple attached WAL databases as one set.

### SQLite versus Turso

Plain SQLite is the default core engine candidate. Ion intentionally has one semantic writer per session, and that writer performs short transactions only; therefore SQLite's one-writer-per-database rule is not presently a mismatch.

Turso Database is a valid P2 engine candidate because it offers a Rust implementation, SQLite compatibility, native async capabilities and MVCC/concurrent writes. Those features do not by themselves justify replacing SQLite. Multiple canonical writers inside one session would still require Ion-level ordering, authority, cancellation and recovery semantics and would trade deterministic serialization for conflict/retry handling.

Benchmark Turso only against representative Ion workloads if SQLite storage becomes measurable overhead or a future accepted requirement such as local/remote sync makes Turso materially relevant. Do not add a production Turso/libSQL dependency for optionality alone.

libSQL is not a preferred initial engine because its remote/replica features are not current core requirements and its fundamental single-writer behavior does not solve a present Ion problem.

Client/server databases such as Postgres belong to a future multi-host/multi-user writable-session design, not the local-first core.

Keep storage implementation private enough to change engines, but do not build a generic multi-backend framework before a second engine earns inclusion.

### Artifacts/output

Small semantic payloads required for exact replay may remain inline. Large opaque tool/process output may spill to files with durable reference, byte counts, integrity hash and truncation/quota status.

Publish content safely before committing a required reference. Crash residue may produce an orphan to collect; committed state must not point to missing required content.

Separate three classes:

1. semantic facts/control decisions: durable before acknowledgement/publication;
2. recovery checkpoints/output: bounded and periodically durable, never proof of terminal completion;
3. live display deltas: provisional and replaceable.

Do not fsync every token or rewrite an ever-growing full message for each streaming frame. P2 selects checkpoint cadence and spill thresholds from measurement.

## 16. Public contract

Expose session/agent/conversation commands and bounded queries with stable typed IDs, receipts, typed errors and explicit cancellation scopes.

Keep database rows/codecs, invocation tokens, transaction builders and raw registries private unless a demonstrated embedding use requires them.

A frontend attaches without becoming an execution owner. Acceptance and eventual completion are separately observable.

A compatibility adapter such as ACP may wait for completion if required by that protocol, but it must not change the runtime's underlying admission semantics.

Default local access uses OS-local trust and restrictive file/socket permissions. Remote authentication, multi-user authorization and distributed writable sessions require separate designs before exposure.

## 17. Validation gates

No design section becomes "optimal" merely because it is written here.

| Gate | Decision/evidence |
|---|---|
| P1 | Promote the typed re-entrant task model into production components; validate input idempotency, concurrent tool tasks, retained worker wait, cancellation races, recovery, ownership and missing-kind behavior. |
| P2 | Validate physical store topology, SQLite behavior, output/checkpoint policy, indexed history/forks, active-memory bounds, backup/repair and optionally Turso under the same workload. |
| P3 | Validate extension/tool/behavior contribution boundaries, authority/reload/failure semantics and sandbox assumptions. |
| P4 | Validate TUI/group interaction, target-safe input, approvals, narrow layouts, output overload, reconnect and terminal restoration. |

P1-P4 build the core. Higher-level memory/knowledge/task-board systems are intentionally outside these gates. They may be investigated after the core baseline exists, using controlled effectiveness measurements rather than architecture speculation.

The implementation rule is simple: **if evidence shows a core decision is wrong, change the design and implementation. Do not preserve it for compatibility with unfinished internal code.**
