# Ion roadmap

This roadmap delivers [DESIGN.md](DESIGN.md) and [TERMINAL.md](TERMINAL.md). It deliberately prioritizes a correct, cohesive core over preserving the current implementation.

Long-term/project memory, knowledge stores, shared task boards, vector stores and planner layers are outside the core roadmap. They may be evaluated later against a stable baseline and must not shape the core prematurely.

## Current position

| Deliverable | State | Evidence |
|---|---|---|
| Core architecture | R0 kernel contracts accepted; clean rewrite active | DESIGN.md; docs/r0-kernel-gates-2026-09-12.md |
| Worker/fork/session topology | Accepted target | docs/research/agent-topology-context-2026-09-12.md; R0.2 |
| Task execution/recovery | Accepted target contract; production implementation open | R0.1/R0.3; docs/r0-kernel-gates-2026-09-12.md |
| IDs/order | Session-local unified sequence selected; production schema open | R0.4 |
| AI boundary | Independent provider-neutral `ion-ai` contract selected; production crate open | R0.5 |
| Storage topology/engine | Per-session SQLite leading physical candidate; comparative P2 work open | docs/research/storage-engines-2026-09-12.md; docs/p2-storage-topology-prototype.md |
| Fresh production core | Not implemented | current `ion-core` still contains lane/agent/operation/effect-era machinery |
| TUI/group target | Interaction requirements drafted; old application not target-validated | TERMINAL.md |

R0 was validated at `81c344d713f13b73e19232b4ad36dfe40ba663b9` by CI run `34718467220`: fmt, strict Clippy and the full workspace tests passed, including real subprocess crash/recovery. The design remains reopenable if production evidence disproves a contract, but the old runtime is no longer a reason to delay cutover.

## 1. Immediate work: clean rewrite

Follow [docs/core-runtime-migration.md](docs/core-runtime-migration.md) and [docs/source-layout.md](docs/source-layout.md). The five pre-rewrite gates are closed; their evidence and decisions are recorded in [docs/r0-kernel-gates-2026-09-12.md](docs/r0-kernel-gates-2026-09-12.md).

Accepted R0 decisions:

1. one typed async `TaskKind` contract with durable complete checkpoints, invocation-generation fencing, durable cancellation mark + local signal, and fresh abort invocation;
2. append-only immutable entries with derived head/edit context and stable fork cutoffs;
3. no generic durable `Effect` entity in the fresh core;
4. one session-local monotonic sequence privately mints typed local IDs and commit cursors;
5. a small independent provider-neutral `ion-ai` crate owns model request/stream/result/error types and a scripted service.

Do not add another prototype round before K0/K1 unless a production implementation exposes a concrete contradiction.

## 2. Clean production core

Do not maintain old/new production runtimes in parallel. Git history preserves the old implementation.

### K0 — `ion-ai` contract crate

Promote only the accepted R0.5 provider-neutral model contract plus scripted service. No production HTTP/auth/catalog implementation yet.

### K1 — storage-independent domain

Build only target nouns:

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

No durable Agent row, lane, Operation, or generic Effect.

A `Conversation` may have a history parent/cutoff and/or owning task. Those are different edges.

### K2 — session writer + deterministic in-memory store

Implement one serialized mutation line and atomic batch model first against an in-memory store.

First capabilities:

- create session/root conversation;
- create independent/forked/owned conversations;
- append immutable entries/context controls;
- accept/dedupe/place inputs;
- create tasks and acyclic dependencies;
- reserve invocation;
- checkpoint task;
- mark cancellation;
- apply terminal/abort closure;
- produce bounded committed observations.

### K3 — task driver

Implement claim/drive, execute/recover/abort ownership, invocation fencing, local cancellation signalling, waits, dependencies, close and fail-stop behavior using deterministic task kinds.

Opening/inspection starts no task effects.

### K4 — fresh SQLite session store

Implement a fresh schema for one session database. Do not migrate old lane/operation tables in place while designing the core.

Development-era old databases may be archived/refused under the pre-1.0 policy. A later migration is written only if preserving old sessions is actually worth the complexity.

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