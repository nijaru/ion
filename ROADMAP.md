# Ion roadmap

This roadmap delivers [DESIGN.md](DESIGN.md) and [TERMINAL.md](TERMINAL.md). It deliberately prioritizes a correct, cohesive core over preserving the current implementation.

Long-term/project memory, knowledge stores, shared task boards, vector stores and planner layers are outside the core roadmap. They may be evaluated later against a stable baseline and must not shape the core prematurely.

## Current position

| Deliverable | State | Evidence |
|---|---|---|
| Core architecture | Clean rewrite active; K0-K2 implemented, K3 core driver validated | DESIGN.md; docs/core-runtime-migration.md; code checkpoint `a33e0fb22073857cc724244b23f99e0c12249147` |
| Worker/fork/session topology | Accepted target; fork/ownership domain + writer primitives implemented, worker runtime open | docs/research/agent-topology-context-2026-09-12.md; K1/K2 |
| Task execution/recovery | K2 lifecycle boundary implemented; K3 async execute/recover/abort driver in progress | R0.1/R0.3; `crates/ion-core/src/task/`; `crates/ion-core/src/session/scheduler.rs` |
| IDs/order | Session-local unified sequence implemented in fresh core; SQLite representation open | R0.4; K1/K2 |
| AI boundary | Independent provider-neutral `ion-ai` contract crate implemented | R0.5; `crates/ion-ai/` |
| Storage topology/engine | Deterministic in-memory writer implemented; fresh per-session SQLite store is K4 | docs/research/storage-engines-2026-09-12.md; docs/p2-storage-topology-prototype.md |
| Fresh production core | Old lane/agent/operation/effect runtime physically removed; K0-K2 complete, K3 in progress | cleanup commit `2d273f78`; code checkpoint `a33e0fb2` |
| TUI/group target | Interaction requirements drafted; fresh application/TUI not yet rebuilt | TERMINAL.md |

R0 was validated at `81c344d713f13b73e19232b4ad36dfe40ba663b9` by CI run `34718467220`. The old core was then physically removed instead of retained as a compatibility runtime. The current clean-core code checkpoint is `a33e0fb22073857cc724244b23f99e0c12249147`, validated by CI run `34725190754`: format, strict Clippy and the full workspace tests all pass.

The design remains reopenable if production evidence disproves a contract. Do not preserve an implementation merely because it has now been rewritten once.

## 1. Immediate work: clean rewrite

Follow [docs/core-runtime-migration.md](docs/core-runtime-migration.md) and [docs/source-layout.md](docs/source-layout.md). The five pre-rewrite gates are closed; their evidence and decisions are recorded in [docs/r0-kernel-gates-2026-09-12.md](docs/r0-kernel-gates-2026-09-12.md).

Accepted R0 decisions:

1. one typed async `TaskKind` contract with durable complete checkpoints, invocation-generation fencing, durable cancellation mark + local signal, and fresh abort invocation;
2. append-only immutable entries with derived head/edit context and stable fork cutoffs;
3. no generic durable `Effect` entity in the fresh core;
4. one session-local monotonic sequence privately mints typed local IDs and commit cursors;
5. a small independent provider-neutral `ion-ai` crate owns model request/stream/result/error types and a scripted service.

The rewrite is now past the pre-rewrite/prototype stage. Continue from the production kernel; add another isolated prototype only when a concrete production design question is cheaper to answer that way.

## 2. Clean production core

Do not maintain old/new production runtimes in parallel. Git history preserves the old implementation.

### K0 — `ion-ai` contract crate

**Status: implemented and validated.**

The accepted R0.5 provider-neutral model contract plus scripted service lives in the independent `ion-ai` crate. No production HTTP/auth/catalog implementation belongs here yet.

### K1 — storage-independent domain

**Status: implemented and validated.**

The fresh core contains only the target durable nouns:

```text
Session
Conversation
Entry
Input
Task
TaskOutput
Artifact
```

Identifiers use distinct Rust wrappers over one session-local ordered namespace; `SessionId` is global.

No durable Agent row, lane, Operation, or generic Effect remains in the production core.

A `Conversation` may have a history parent/cutoff and/or owning task. Those are different edges.

### K2 — session writer + deterministic in-memory store

**Status: implemented and validated.**

The current kernel has one serialized semantic mutation line, typed atomic mutation batches and a deterministic in-memory store. Implemented capabilities include:

- create session/root conversation;
- create independent/forked/owned conversations with reciprocal task ownership;
- append immutable entries/context controls;
- accept/dedupe/place inputs with exact request-key replay/conflict;
- create tasks and dependency edges;
- reserve execute/recover/abort invocations with generation fencing;
- replace durable task checkpoints/output;
- durably mark cancellation and fence stale/normal writes;
- apply terminal/abort closure atomically with successor writes;
- explicit input disposition transitions;
- produce bounded committed observations with resnapshot-on-overflow semantics;
- reject failed transactions without publishing IDs, commits or partial successor state.

K2's lifecycle paths are exercised through the K3 production driver so they are not test-only dead code.

### K3 — task driver

**Status: in progress; core driver slice validated.**

Implemented now:

- explicit `drive_task`; inspection/snapshot does not start work;
- task-kind registry keyed by kind + schema revision;
- execute for pending tasks, recover for already-running durable tasks and abort for cancelled work;
- task futures run outside the session mutation lock;
- invocation-scoped `TaskContext` checkpoint commits re-enter the session writer briefly;
- durable cancellation mark + process-local cancellation signal + fresh abort generation after the old invocation joins;
- local duplicate-drive rejection for one task;
- dependency readiness enforcement at reservation;
- missing task kind settles `Unsupported` without deleting the record;
- task errors and panics settle durable `Failed` outcomes;
- cancellation/settlement ordering is serialized under the writer: settlement wins if it commits first, otherwise abort owns cleanup.

Still open before K3 is considered complete:

- restricted typed task finalization so a generation can atomically settle with its successor entries, tasks and ownership writes;
- typed task authoring adapter over the erased registry;

- writable ownership release after local joins (K4).

The storage-independent wait/capacity/close slice implements client task/dependency waits over committed-state notifications, independent optional model/tool/process limits, driver-owned invocation lifetime across caller disappearance, and graceful/fault close with canonical-write fencing and local joins. Immutable dependencies reject self/forward references by requiring existing tasks; there is no dynamic invocation wait graph. Deterministic tests live in `k3_waits.rs`, `k3_close.rs` and the capacity unit tests. Format, strict workspace Clippy and full workspace tests pass for this slice. Initial cancellation dispatch, cancellation during saturated capacity admission, and interruption/uncertainty handling are covered by outcome-sensitive tests in `k3_abort.rs` and `k3_driver.rs`: exactly one Abort invocation, no normal execution, separate bounded cleanup admission, interrupted invocations that stay running and recoverable, known-failure versus indeterminate settlements, and blocked recovery for a running task whose implementation is unavailable. Persisted reopen coverage remains open.

Persistence-backed evidence remains open:

- persisted reopen/crash recovery and explicit resume/drive, which require K4 storage to test honestly.

### K4 — fresh SQLite session store

**Status: resident-state/persistence separation implemented; SQLite paused for contract work.**

The session owns resident semantic state; a private persistence sink accepts validated batches before prepared state is installed and observations publish. Persistence errors fence the session and fault-stop live async invocations. Fault tests cover atomic rejection of terminal/successor writes, unchanged resident state/observations, and live invocation shutdown. This is in-memory fault evidence, not crash durability evidence.

Before SQL, replace full-history draft/snapshot cloning with bounded typed reads and a transaction overlay (or a justified bounded working set). Semantic ownership does not require full residency. Complete restricted task finalization and typed task adaptation, and specify foreground-turn membership/input disposition before freezing the persistence interface. The current split establishes ordering, not these structural requirements.

Implement a fresh schema for one session database. Do not migrate old lane/operation tables in place while designing the core.

Development-era old databases may be archived/refused under the pre-1.0 policy. A later migration is written only if preserving old sessions is actually worth the complexity.

K4 should preserve the K2 ordering contract: validate/build against resident state, durably commit the complete mutation batch, update resident indexes/state, then publish committed observations. SQLite I/O must not become an alternate semantic writer.

### K5 — one generation/tool chain

Use `ion-ai`'s scripted model service and a narrow tool executor:

```text
input
 -> generation
 -> tool A + tool B
 -> post-tools/join
 -> final generation
```

Generation settlement atomically creates all tool children plus the join. Tool B may settle before A; provider projection still emits A then B.

### K6 — workers as owned conversations

Implement through the same session/task kernel:

- fresh-context worker;
- inherited-context worker at safe cutoff;
- foreground/joined run;
- retained/background spawn;
- follow-up/reuse;
- send/inspect/wait/interrupt/retire;
- nested ownership limits;
- waits that do not monopolize execution capacity;
- subtree cancellation barriers.

There is no separate agent registry.

**Exit for K0–K6:** a new production core replaces the old lane/operation/agent/effect machinery; old core files and temporary R0/P1 prototype implementations are removed rather than left as permanent compatibility paths.

## 3. P1 — durable execution correctness

The fresh production core closes P1 when deterministic tests cover:

1. durable input acceptance and exact request-key replay/conflict;
2. two tool tasks settling out of order with call-order model projection;
3. worker conversation surviving creator task settlement;
4. capacity-safe waits;
5. cancel/settle both orders and late callback fencing;
6. process loss across retry-safe, reconcile/adopt and indeterminate external actions;
7. atomic terminal settlement + successor/ownership writes;
8. reopen without automatic execution before explicit drive;
9. caller disappearance before/after admission;
10. second writable owner rejection including abnormal predecessor exit;
11. missing task-kind behavior without data loss;
12. dependency/self-wait cycle rejection;
13. panic/failure/host-close join policy;
14. explicit durable input disposition;
15. worker creation/message/cancellation races under one session writer.

Fresh-core coverage now includes exact request-key replay/conflict, atomic terminal successor rollback/commit, explicit input disposition, generation/cancellation fencing, missing task kind, panic/error settlement, no-work-on-inspection and both high-level cancel/settle orderings. The remaining items are not implied complete by the historical prototypes.

Historical isolated P1/R0 prototypes remain regression evidence only until the fresh core covers these invariants.

## 4. P2 — storage, output and history

Semantic rule: one top-level session has one crash-atomic canonical store boundary. Root and worker conversations share it. Independent sessions do not.

Leading physical candidate:

```text
Ion data root/
  sessions/<SessionId>/
    session.sqlite
    artifacts/

  catalog.sqlite   # optional rebuildable discovery cache only if useful
```

Compare the current root-wide SQLite layout against per-session stores under many active sessions, large/cold histories, WAL/checkpoint pressure, backup/archive/delete, corruption isolation and restart/repair.

SQLite remains the baseline. Benchmark Turso only if representative measurements show engine-level overhead or a concrete sync requirement makes it relevant. Do not add concurrent canonical writers merely because an engine supports them.

Output tests cover large model/tool/process streams, spill thresholds, slow/dying storage, disk exhaustion, process loss, artifact publication/orphan cleanup and watch overflow.

History tests cover long transcripts, dense heads/edits, deep forks and at least 100k terminal tasks with a small live set. Opening/resuming must not decode all historical task payloads.

**Exit:** justified physical topology, measured query/RSS/WAL/artifact behavior, supported backup/repair path, explicit provisional-output loss bounds and bounded active residency.

## 5. Component pass A — execution environment and tools

After the kernel contract is stable, redesign rather than automatically preserve current tool/process modules.

Target boundary owns:

- workspace identity/binding/isolation;
- files/read/write/edit/search;
- shell/process lifecycle;
- background jobs;
- sandbox/policy enforcement;
- narrow tool execution API;
- MCP adaptation later.

Ordinary tool definitions receive no arbitrary session mutation authority. Built-in trusted task adapters mediate durable worker/job creation and other session-affecting operations.

Port path-safety, process-cleanup, output-bounding and policy algorithms only after they pass the new boundary.

## 6. Component pass B — AI/model/provider/auth subsystem

Extend the accepted `ion-ai` boundary using the useful separation seen in `pi-ai` without copying its TypeScript API:

```text
ModelService / model registry
  Provider
    auth + model catalog + endpoint policy
      API adapter
        provider wire protocol
```

Provider-neutral messages/tools/stream results sit above reusable API adapters. Several providers may share one OpenAI-compatible/Responses/Anthropic/etc. wire implementation.

Provider subsystem owns model discovery/refresh, capabilities/pricing metadata, auth resolution and wire parsing. The application/host owns credential persistence. Dynamic model-catalog cache is independent of session storage.

Ion-specific requirements:

- typed provider error categories;
- provider API contains no session/task IDs;
- durable retry/backoff/usage belongs to the generation task;
- provider/SDK hidden retries are disabled or controlled;
- opaque provider replay metadata may survive on provider-neutral assistant content for same-provider continuity;
- deterministic faux/scripted provider for tests.

Only after the fresh core can run scripted generation should OpenRouter/OpenAI-Codex/current auth code be selectively ported.

## 7. Component pass C — TUI/client architecture

The TUI is a client of runtime truth, not another state owner.

Validate conversation view, session/group summary, worker focus, target-safe per-conversation drafts, approvals, narrow terminals, Unicode/multiline input, resize/output floods, reconnect/resnapshot and terminal restoration.

Review the existing terminal/editor/rendering primitives individually. Do not preserve the current large TUI application structure merely because some widgets are reusable.

## 8. Component pass D — extensions/MCP/authority

Add extensibility only after the core tool/task/client boundaries exist.

Test tool contribution, observation hooks and any justified typed behavior contribution; explicit replace/remove; stale approval/revocation; missing implementation on recovery; cancellation/oversized output; trusted local subprocess versus actual OS sandbox isolation.

MCP is an execution/tool adapter, not a parallel runtime or storage authority.

## 9. Component pass E — external protocols and application shell

Rebuild ACP/JSON/RPC/print/CLI/session-management adapters on the one command/observation contract. Then review settings, export/import, updater/package behavior.

These adapters may wait/reshape responses for protocol compatibility but must not introduce alternate session semantics.

## 10. Delivery milestones

| Milestone | End-to-end result |
|---|---|
| M1 — fresh core | new Session/Conversation/Task runtime, scripted generation/tool chain, SQLite session store, one worker, crash/cancel tests; old core removed |
| M2 — real coding loop | execution environment plus first real provider path, read/edit/shell, compaction/history/forks, usable headless client |
| M3 — controlled workers | retained/nested workers, messages/results, workspace policies, background jobs, group limits and TUI group control |
| M4 — daily coding | provider/auth/model catalog breadth, images, skills/resources, search/completion, editor/settings/export and hardened TUI |
| M5 — parallel implementation | worktree-backed mutating workers, retained patches/evidence, apply/reverify/conflict/cleanup |
| M6 — extensibility/interoperability | scoped extensions/MCP plus stable useful Rust/JSON/ACP client surfaces |
| M7 — effectiveness | controlled evaluation of context, editing/tool interfaces, compaction and delegation; later higher-level systems only if they beat the core baseline |

No milestone is “Pi parity.” Pico supplies minimal-harness design evidence; Codex supplies production-engineering evidence; Ion chooses its own tested contracts.

## 11. Evaluation rules

Use fake model/environment implementations, controllable clocks and storage barriers for deterministic races. Run both orderings of every important race. Crash-inject before/after admission, external dispatch, checkpoint, result, terminal settlement, successor creation and artifact publication.

Keep runtime performance separate from model effectiveness. Record source revision, exact workload/configuration, memory, storage bytes, query counts, lock/checkpoint behavior, restart time and externally verified task outcomes.

Agent-effectiveness optimization starts from a stable M1/M2 baseline. More context, more agents, memory systems and richer coordination surfaces must demonstrate benefit rather than receive architectural preference.

## Evidence log

2026-09-12: isolated historical P1 prototype at `78148e84d6120d5670a784ec3ecb07684577db1d` validated idempotent reopen, out-of-order effect settlement/call-order projection, retained worker lifetime, capacity-safe wait, cancellation fencing and abrupt-process recovery. Its failure/race evidence is retained; its mandatory `step -> plan` authoring API is superseded.

2026-09-12: P2 structural prototype at `0c45553538e0244e27cc13ac0719f8afb7d73cbb` validated atomic rollback within a session DB, independent writer-lock domains for separate session files, and rebuildable discovery metadata. No performance claim is attached.

2026-09-12: architecture revisions 3–5 removed knowledge/task-board systems from core, converged workers onto owned conversations, made fresh versus inherited context independent from session/lifetime, moved toward immutable transcript-derived context controls, reopened task authoring around async execute/recover/abort + durable commits, and made generic Effect removal an explicit pre-rewrite gate.

2026-09-12: R0.1–R0.5 passed together at `81c344d713f13b73e19232b4ad36dfe40ba663b9` in CI run `34718467220`. Accepted: typed async task contract with durable checkpoints/generation fencing; immutable entry/head/edit context + safe fork cutoffs; task-level external recovery without generic Effect; one session-local sequence backing typed local IDs and commit cursors; independent provider-neutral `ion-ai` boundary. See `docs/r0-kernel-gates-2026-09-12.md`.

2026-09-12: legacy production `ion-core` runtime/store/operation/tool code and historical prototype harnesses were physically removed at cleanup commit `2d273f78`; Git history remains the archive. The clean core no longer carries lane/agent/operation/effect compatibility paths.

2026-09-12: clean K0-K2 plus the first K3 task driver are validated at code checkpoint `a33e0fb22073857cc724244b23f99e0c12249147` by CI run `34725190754`. Fresh-core evidence includes exact request-key replay/conflict, immutable fork/context checks, atomic owned-conversation/task links, bounded observations, invocation generation fencing, checkpoints, cancellation + fresh abort generation, terminal successor rollback/commit, dependency readiness, missing-kind preservation, panic/error settlement, no-work-on-inspection, duplicate local-drive rejection and serialized cancellation/settlement ordering.