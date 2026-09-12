# Ion roadmap

This roadmap delivers the core target in [DESIGN.md](DESIGN.md) and [TERMINAL.md](TERMINAL.md). Milestones are end-to-end capabilities, not a request to build every planned abstraction first.

The roadmap intentionally excludes long-term/project knowledge, memory systems, shared assignment/task boards, vector stores and similar higher-level coordination features. Those may be researched after the core agent/session runtime provides a clean baseline; they are not dependencies of this roadmap.

## Current position

| Deliverable | State | Evidence |
|---|---|---|
| Core product/architecture contract | Drafted and actively revised | DESIGN.md revision 3; TERMINAL.md |
| Reference/evidence record | Current Pico/Pi2/Pico v3 primary minimal-harness reference; Codex production reference | docs/research.md |
| P1 durable task/transaction model | In progress; isolated prototype validated, production promotion open | docs/p1-execution-prototype.md; CI at `78148e84` |
| P2 storage/output topology | In progress; initial per-session structural prototype validated, comparative/performance work open | docs/p2-storage-topology-prototype.md |
| P3 extension/authority boundary | Not started in this redesign workstream | existing implementation is not target validation |
| P4 group/TUI interaction | Early target/draft reducer evidence only | P1 target-safe draft/command tests; no PTY/human acceptance yet |
| Target architecture implemented | No | current production runtime still contains lane/operation-era machinery |

The architecture remains deliberately reopenable. A failed prototype or production slice changes the design rather than becoming a hidden compatibility requirement.

## 1. Immediate implementation: replace the old execution core

Follow [docs/core-runtime-migration.md](docs/core-runtime-migration.md). Do not deepen the legacy `lane -> operation -> one open effect` model.

### K0 — Target nouns and store boundary

Establish the new semantic identities and crate-private session-store boundary without yet duplicating the runtime.

Required concepts:

- session;
- retained agent;
- conversation;
- input/receipt;
- task/dependency;
- effect/attempt;
- immutable entry/context;
- artifact reference.

`CommitSeq` remains separate from semantic identity. Exact local-ID representation is decided with P1/P2 evidence rather than by legacy schema compatibility.

### K1 — Session command kernel

Introduce one crate-private serialized session mutation owner. Typed commands read committed state, validate, build one bounded commit plan, persist it atomically, publish committed observations and return post-commit effect dispatch.

First command surface:

1. admit input with request-key equivalence/idempotency;
2. create/settle/cancel task;
3. add/remove dependency with cycle checks;
4. open/settle/recover effect attempt;
5. append entry/update explicit context;
6. create retained agent/conversation.

No provider/tool/process I/O occurs while mutation authority or a database write transaction is held.

### K2 — One real turn using generic tasks

Replace one production agent turn with:

```text
input
  -> generation
      -> tool A ---+
      -> tool B ---+-> join/post-tools -> next generation/final
```

Generation settlement creates all tool children plus their join atomically. Tools settle independently in completion order. Model projection restores source call order.

This slice must use real `SessionStore` transactions plus scripted providers/tools. It is not another isolated runtime.

### K3 — Recovery/cancellation

Promote P1 fault semantics into production:

- durable effect intent before dispatch;
- stable effect identity plus attempt identity;
- retry-safe/reconcile/no-safe-retry recovery;
- open/inspect starts no work;
- explicit drive/resume;
- invocation generation fencing;
- settlement/cancel race in both orders;
- caller disappearance before/after admission;
- persistence uncertainty fences the session;
- host close preserves recoverable unfinished work;
- missing task kind remains inspectable and blocked.

### K4 — Retained workers on the same kernel

Move retained-agent behavior onto the same session/task kernel.

- spawn identity + conversation + authority/workspace request + initial input/task atomically when required;
- spawn task/tool can finish while worker remains retained;
- waits park as dependencies/continuations instead of consuming execution capacity;
- inspection is read-only;
- delayed replies/drafts retain captured target identity;
- subtree cancellation has a durable admission barrier.

Delete displaced lane-based orchestration once equivalent coverage exists.

### K5 — Remove legacy concepts

Delete or convert:

- `OperationMachine` as the main turn scheduler;
- `OperationId` where the semantic object is a task/turn root;
- lane identity where the semantic object is an agent/conversation;
- singular `open_effect` checkpoint state;
- `submit_if_idle_on_lane` as the input-admission contract.

Do not preserve compatibility with unfinished internal APIs.

## 2. P1 — production durable execution gate

P1 closes only when production components demonstrate the intended semantics.

Required deterministic cases:

1. durable input acceptance; lost reply; retry same key returns original receipt; changed target/content/mode conflicts;
2. two tool calls complete out of order while provider projection remains call ordered;
3. retained worker outlives spawning task/tool;
4. waiting does not hold the execution capacity needed by the dependency;
5. settlement/cancel race both ways including late output;
6. process loss after provider/tool intent distinguishes retry-safe, reconcile and uncertain effects;
7. atomic task settlement + successor creation/ownership transfer;
8. pending reopen does no external work until explicit drive;
9. caller disappearance before/after admission;
10. second writable owner rejected, including abnormal predecessor exit;
11. missing task kind blocks visibly without data loss;
12. dependency/self-wait cycle rejection;
13. implementation panic/failure/host-close joins are surfaced under a tested policy;
14. explicit input disposition remains recoverable.

Exit: one production Rust task API, no permanent second runtime, passing deterministic fault tests, and displaced legacy execution code removed as slices land.

## 3. P2 — storage, output and history gate

The semantic rule is already clear: one session has one canonical crash-atomic store boundary. Physical topology and engine remain evidence-gated.

### Leading candidate

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite
      artifacts/

  # optional rebuildable discovery cache if measurements justify it
  catalog.sqlite
```

Root agent and retained workers inside one session/group share `session.sqlite`. Independent top-level sessions may have separate owners/connections/WAL files and progress independently.

Do not use one database per agent. Do not split one session command across category-specific WAL databases.

### Compare against current root-wide store

Measure at minimum:

- many independent sessions writing concurrently;
- one large active session plus many cold sessions;
- WAL growth/checkpoint pressure and lock wait;
- startup/open/list/resume latency;
- corruption/failure isolation;
- session delete/archive/clone/backup/restore;
- interrupted session creation;
- optional catalog loss/rebuild if a catalog is introduced;
- schema archive/migration/refusal behavior.

### SQLite baseline

SQLite remains the production baseline unless evidence changes the decision. Ion's one-writer-per-session semantic model and short write transactions fit SQLite WAL well; slow model/tool/process work must never run inside those transactions.

### Turso comparison

Turso Database is a valid **benchmark candidate**, not a production dependency.

Evaluate it only after the session workload is representative and only if one of these becomes plausible:

- SQLite commit/lock/checkpoint overhead is material after per-session partitioning;
- native async storage materially helps versus a dedicated blocking worker;
- a concrete accepted local/remote sync requirement appears;
- another Turso feature solves a measured core problem.

Do not add concurrent canonical session writers merely because the engine permits them. Ion still requires one deterministic semantic mutation line for ordering, authority, cancellation and recovery.

libSQL is not a preferred core engine; its replica/remote surface is not currently required and it retains the single-writer model.

### Output and artifact tests

Exercise:

- large model response;
- large shell/tool output;
- tool-to-background-job output ownership transfer;
- slow storage;
- disk exhaustion;
- process loss at each publication boundary;
- watch/subscription overflow;
- orphan artifact cleanup;
- receipt/tombstone retention sufficient to prevent duplicate work.

Separate semantic durable facts, bounded recovery checkpoints and provisional live display output.

Measure checkpoint cadence, bytes written including WAL/artifacts, restart loss bound and UI reconstruction cost. Do not fsync every token.

### History/context tests

Benchmark cold/warm queries across:

- short and long transcripts;
- dense context edits;
- shallow/deep historical forks;
- a fixture with 100,000 terminal tasks and a small active set.

Opening/resuming execution must not decode all historical task state. Record RSS, retained objects, query count and time to interactive view.

Exit: justified physical store boundary, tested backup/repair behavior, bounded active residency, coherent snapshot/output cursors and recorded performance envelopes.

## 4. P3 — contributions, authority and reload

Implement the minimum extensibility boundaries required by a real coding agent without turning the core into a framework for its own sake.

Validate:

- one tool contribution;
- one observational contribution;
- one typed behavior contribution if dynamic behavior remains justified;
- preparation/required-hook failure;
- cancellation and oversized response handling;
- explicit replacement/removal;
- missing implementation on recovery;
- stale approval and authority revocation races;
- child authority = request ∩ parent ceiling ∩ host policy;
- trusted local subprocess versus actual OS sandbox boundaries.

Exit: one public contribution mechanism per demonstrated purpose, tested failure/disposal semantics and no promise of rolling back already-performed external effects.

## 5. P4 — TUI/group interaction

Build the TUI as a client of runtime truth rather than a second state machine.

Required behavior includes:

- main conversation plus group summary;
- focused-worker detail;
- read-only inspection;
- independent per-agent drafts;
- stable target IDs when focus changes during command completion;
- hidden-worker approvals surfaced correctly;
- narrow terminals;
- Unicode/multiline editing;
- output floods/slow rendering;
- reconnect from a fresh bounded snapshot;
- terminal restoration on failure/exit.

Measure input-to-frame and cancellation-command latency under load separately from provider latency.

Exit: agreed layout/input model, reducer/PTy coverage, and recorded human terminal acceptance for behavior automation cannot establish.

## 6. Delivery milestones

| Milestone | End-to-end result |
|---|---|
| M1 — one runtime, two agents | root + retained worker, provider-neutral generation, native read/edit/shell slice, durable input/recovery, visible/control-capable TUI and headless trace client |
| M2 — daily coding | model/provider/auth configuration, images where supported, skills/prompts/resources, compaction, history/forks, queues, search/completion, external editor, export/settings |
| M3 — controlled group work | nested retained workers, peer messages/results, background jobs, safe pause/cancel scopes, budgets/limits, explicit workspace policy |
| M4 — parallel implementation | worktree-backed mutating workers, retained patches/evidence, review/apply/reverify and explicit cleanup/conflict handling |
| M5 — interoperable clients/extensibility | useful Rust API, bounded JSON control/events, negotiated ACP, supervised plugins/MCP/scoped hooks where justified |
| M6 — measured agent effectiveness | controlled evaluation of tool interfaces, context/compaction, editing representation, single versus bounded delegation, and any later proposed higher-level systems |

No milestone is "Pi parity". Pico/Pi2/Pico v3 supply architecture/failure-case evidence; ordinary Pi remains useful product/workflow evidence. Ion may differ deliberately, but differences must be reasoned and tested.

## 7. Correctness and evaluation rules

Use fake providers/environments, controllable clocks and storage barriers for deterministic races. Test both orderings of settlement/cancel, spawn/group-stop, approval/revocation, message/dedup, observation-capture/commit and output publication.

Crash injection covers before/after:

- input acceptance;
- task/effect intent;
- external completion;
- settlement/successor creation;
- artifact publication;
- session-store creation/repair.

Provider conformance tests cover semantic completion versus transport EOF, fragmented tool arguments, cancellation, malformed frames, usage uncertainty, unsupported capabilities and model identity mismatch.

System benchmarks separate runtime overhead from provider/tool cost and record source revision, model/provider/configuration, workload, memory, storage bytes, query counts, lock/checkpoint behavior and restart/backup time.

Agent-effectiveness evaluation starts only after a stable M1 baseline. Improvements ship because controlled tasks show benefit, not because another harness implements them.

## Evidence log

2026-09-12: Isolated P1 execution prototype validated at `78148e84d6120d5670a784ec3ecb07684577db1`. Demonstrated durable idempotent admission/reopen, out-of-order effect settlement with call-order projection, retained worker lifetime, capacity-safe waiting, cancellation/invocation fencing, stable early-P4 command targets/drafts, and abruptly killed subprocess recovery distinguishing retry-safe from indeterminate effects. Rust 1.98.0 repository gates passed. P1 remains open pending production promotion.

2026-09-12: DESIGN revision 2 introduced a per-session storage hypothesis. P2 structural prototype at `0c45553538e0244e27cc13ac0719f8afb7d73cbb` validated same-file atomic rollback, independent writer-lock domains for separate session files, rebuildable catalog metadata and session creation before catalog publication. This is structural evidence, not a performance result.

2026-09-12: DESIGN revision 3 narrowed the architecture to the core agent/session runtime. Knowledge/memory/task-board systems were removed from the core target. SQLite remains the baseline engine; Turso is retained only as a possible P2 comparison if representative measurements justify it. The leading physical boundary remains one canonical database per top-level session/group, subject to comparative P2 evidence.
