# Ion roadmap

This roadmap delivers the target in [DESIGN.md](DESIGN.md) and [TERMINAL.md](TERMINAL.md). It replaces old Pi-parity work ordering, not the evidence contained in existing source and tests. Milestones are end-to-end capabilities, not a request to build every planned abstraction first.

## Current position

The product goal is established: an idiomatic Rust coding agent, single-agent by default, with optional cooperating workers and full TUI control. The architecture is a proposal supported by the [research record](docs/research.md). Runtime authoring now has initial prototype evidence; production execution integration, physical storage/output details, extension boundaries, and renderer choices remain prototype-gated.

| Deliverable | State | Evidence |
|---|---|---|
| Product contract, architecture, interaction specification | Drafted and actively revised | DESIGN.md revision 2 and TERMINAL.md |
| Primary-source review and decision register | Recorded and refreshed around current Pico2/Codex/storage evidence | docs/research.md |
| Rust task/transaction prototype P1 | In progress; isolated proof validated, production promotion open | [docs/p1-execution-prototype.md](docs/p1-execution-prototype.md); CI at `78148e84` |
| Output/history/storage prototype P2 | Not started; per-session topology is a hypothesis to measure | DESIGN.md §13; no benchmark claimed |
| Extension/authority prototype P3 | Not started in this workstream | No new runtime tests claimed |
| Multi-agent TUI prototype P4 | In progress only at early target/draft reducer level | P1 prototype checks stable command targets and per-agent drafts; no PTY or human acceptance claimed |
| Optional assignment coordination E1 | Designed only as an experimental layer | No effectiveness evidence claimed |
| Optional project knowledge E2 | Designed only as an experimental layer | No retrieval/effectiveness evidence claimed |
| Target architecture implemented | Not established | Current production runtime predates this redesign; reuse is assessed per slice |

The documentation baseline is Ion commit `fca3346d0fa7aae82ffb77d02234b2f9b0d2975e`. That commit already addresses revision-witnessed peer authority and reload fencing; do not reopen it as a missing feature merely because earlier reviews described it as unfinished. Its commit-recorded test results are historical evidence, not tests rerun for this documentation change.

The design remains deliberately reopenable. A prototype gate can change an architecture decision; implementation that merely happens to exist does not turn that decision into a compatibility requirement.

## 1. Immediate work: P1 production promotion with an early P4 trace

The first isolated P1 proof is validated and recorded in [docs/p1-execution-prototype.md](docs/p1-execution-prototype.md). It selects a typed re-entrant checkpoint/step boundary for durable task authoring, while retaining async Rust inside effect adapters. This is evidence for the production direction, not permission to maintain a second runtime.

Next task: promote those semantics into the existing production ownership boundaries and complete the remaining P1 fault cases. Read DESIGN.md sections 3–6 and the current Pico2 evidence attached to P1. Delete the isolated storage fixture as equivalent invariants become covered by `SessionRuntime`/`SessionStore` tests.

The production slice must continue to demonstrate:

1. Accept input durably; lose the reply; retry its key and receive the original receipt. Changed content, target, or mode with the same key rejects.
2. Run a turn with two tool calls whose completion order differs from source order while model projection retains call order.
3. Spawn one retained worker, finish the spawning tool, and keep the worker addressable.
4. Wait for that worker without retaining the only execution permit it needs.
5. Race task settlement against cancellation in both orders, including late output and generation fencing.
6. Reopen after provider/tool intent, distinguish retry-safe, reconcilable, and uncertain effects, and preserve input disposition.
7. Feed the same runtime observations to a minimal group/focused-agent view, demonstrating stable target IDs and separate drafts.
8. Cover caller disappearance, atomic successor/ownership transfer, pending reopen without automatic work, missing task kind, second-writer ownership, failure/panic/host-close joins, and dependency/self-wait cycles.

The prototype comparison favors the re-entrant typed checkpoint/step API because durable continuation and recovery remain explicit and runtime-owned. Do not keep the async candidate as a second task framework. Resolve the still-open production ID allocation, immutable input encoding, invocation fencing, commit/storage-thread boundary, and schema migration behavior before promoting the prototype.

Do not deepen dependencies on the current one-database-per-data-root physical layout while doing P1. P1 needs a narrow session-store contract and correct transactions; P2 is responsible for testing and, if validated, migrating the physical topology.

Exit: one production Rust task API, an explicit schema/transaction model for this slice, passing deterministic core P1 tests against production components, no second runtime, and an updated evidence entry below. A failed hypothesis changes the design rather than becoming a hidden exception in the implementation.

## 2. Remaining core architecture gates

### P2 — Output, history, and storage topology

Validate the physical store boundary instead of assuming either the current root-wide database or the proposed per-session layout is optimal.

Prototype the leading topology:

```text
Ion data root/
  catalog.sqlite
  sessions/<SessionId>/session.sqlite
  sessions/<SessionId>/artifacts/
```

`session.sqlite` must contain every fact that needs the session writer's atomic commit: conversation/context state, tasks/effects, agents, inputs/messages/receipts, authority/approvals, usage/reservations, and any enabled same-session assignment board. Do not distribute one such transaction over several WAL databases. The catalog is discovery metadata and must be reconstructable from valid session stores.

Compare the candidate against the current one-database-per-data-root implementation under:

- many independent sessions writing concurrently;
- one large active session plus many cold sessions;
- WAL growth/checkpoint pressure and lock contention;
- open/list/resume latency;
- session delete/archive/clone/fork and backup/restore;
- catalog loss, stale catalog rows, interrupted session creation, and repair by scanning stores;
- corruption/failure isolation and second-writer ownership;
- schema migration or archive/refusal behavior.

In the same gate, implement the chosen output owner and snapshot/stream boundary against SQLite and an instrumented test store. Exercise a large model response, large shell output, tool-to-job output handoff, slow storage, disk exhaustion, process loss, and watch overflow.

Measure cold/warm context queries with short and long histories, dense context edits, and shallow/deep forks. Include a fixture with 100,000 terminal tasks and a small live set; opening execution must not decode every historical task payload. Record RSS, retained objects, bytes written including WAL/spool/artifact files, query counts, lock wait, checkpoint behavior, restart time, backup size/time, and time to interactive view.

Choose output checkpoint cadence, spill thresholds, page sizes, retention rules, and the physical session/catalog boundary from measurements. Document exactly how much provisional output can disappear on process loss. Preserve final durable results and control decisions regardless of provisional loss. Test artifact publication before reference, orphan cleanup, and receipt/tombstone retention so cleanup cannot re-enable duplicate work.

Exit: a justified storage topology, coherent snapshot/output cursors, tested durable/provisional separation, bounded active residency, repairable discovery metadata, and recorded performance envelopes. No blanket O(context), constant-memory, or "per-session DB is faster" claim without its measured workload.

### P3 — Extensions, authority, and reload

Implement one tool contribution, one observational contribution, and one typed behavior hook through the intended boundaries. Define trusted local subprocess plugins separately from OS-sandboxed extensions; RPC alone does not isolate host files, processes, or credentials.

Test preparation failure, required-hook failure, cancellation, oversized responses, peer outage, explicit removal/replacement, stale approval, and recovery with a missing implementation. Test revocation versus effect dispatch in both orders and name reuse after removal. Recheck child authority as request intersect parent ceiling intersect host policy; model/configuration changes cannot expand it. Preserve authority independent of discovery availability.

Exit: exact capability and lifecycle rules with tests, one public contribution mechanism per purpose, and explicit behavior for already-dispatched irreversible actions. Do not promise transactional rollback of external process effects.

### P4 — TUI and interaction

Prototype the conversation, group, and focused-worker views using runtime traces, then real runtime commands. Compare renderer choices against interaction requirements, not existing library allegiance. Run the U1–U12 scenarios in TERMINAL.md.

Include a narrow terminal, Unicode/multiline editing, hidden-worker approvals, focus changes during command completion, output floods, reconnection, and terminal restoration. Inspection must remain read-only. Measure input-to-frame and cancellation-command latency under load separately from provider latency.

Exit: an agreed layout/input model, PTY/reducer coverage, and recorded human terminal acceptance for behaviors automation cannot establish. P4 begins with P1; it is not postponed until a complete runtime exists.

## 3. Optional experimental systems after the core path is credible

These gates are intentionally separate from P1–P4. They must not delay a coherent single-agent runtime, and disabled features must contribute no hidden prompt/tool/context tax.

### E1 — Assignment/task coordination

After the core group primitives work, evaluate a revisioned shared assignment board as an optional synchronization aid. The board is not the runtime task graph and does not replace agent supervision, direct messages, or workspace policy.

A candidate should support explicit owner/claim state, revisions, acyclic dependencies, evidence/result references, human visibility, and optional advisory path scopes. Same-session mutations live in the session's atomic database because assignment state may need to commit with agent/message ownership decisions. Model-facing tools are separately toggleable.

Evaluate on multi-agent repository investigations, implementation/review splits, and parallel changes with equal model/token/wall-clock budgets. Measure duplicated work, missed dependencies, coordination/tool tokens, stale claims, human intervention, and verified task outcome. Compare against the simpler baseline of spawn/send/wait/result without a board.

Exit: either evidence that the assignment surface improves useful coordination enough to justify its cost, or a recorded decision to keep it human-only/remove it. It is not enabled by default merely because it is implemented.

### E2 — Project/workspace knowledge

After core history/context and storage behavior are measured, prototype optional cross-session project knowledge. This is not conversation history and must not be silently appended to every model request.

Each retained item needs scope, source/provenance, creation/revision/freshness information, and supersession/invalidation. Agent-written material starts with an explicit trust/status classification. Retrieval must be bounded and inspectable. Start with simple exact/lexical retrieval; add embeddings or derived semantic indexes only if they improve measured retrieval/task outcomes enough to justify another index lifecycle.

Use a separate project/knowledge store because its lifetime can span many sessions. Session-to-knowledge publication uses an explicit idempotent/outbox protocol rather than a cross-database WAL transaction. The core session must remain recoverable and inspectable if knowledge storage is unavailable.

Evaluate stale facts, contradictions, renamed/deleted code, repeated facts, adversarial/low-quality agent notes, repository changes between sessions, and clean disable/reset. Measure retrieval precision/usefulness, context tokens, latency, task success, and harmful stale-memory rate against no-memory and explicit-file baselines.

Exit: a demonstrated retrieval/knowledge policy with provenance and invalidation behavior, or removal. No autonomous consolidation loop becomes mandatory runtime machinery without evidence.

## 4. Delivery milestones

| Milestone | End-to-end result | Dependencies and exit evidence |
|---|---|---|
| M1 — One runtime, two agents | One real provider, root plus one worker, native read/edit/shell slice, durable input and recovery, visible/control-capable TUI, and a headless trace adapter. Single-agent mode adds no swarm instructions/tools. | P1 chosen/promoted; P2 minimum persistence/output path; P4 thin interaction. Real provider credentials remain local. Prove cancellation, uncertain effect, worker lifetime, and target-safe TUI flows. |
| M2 — Daily coding | Model changes, authenticated providers, images where supported, configurable tools, prompt/context resources, skills, compaction, history/forks, queues, search/completion, external editor, export, and settings. | M1; P2 query/output evidence. Capabilities work through runtime APIs and TUI, not UI-only stubs. Establish the explicit coverage matrix below. |
| M3 — Controlled group work | Nested workers, peer messages, group limits, background jobs, safe pause/cancel scopes, and retained results. Optional shared assignments may graduate from E1 if they earn inclusion. | M1 ownership; P2 output; P3 authority where dynamic contributions are involved. No lost messages, duplicate charging, permit starvation, or child grant widening under fault injection. E1 is not required for basic group operation. |
| M4 — Parallel implementation and integration | Worktree-backed mutating workers, deliberate dirty-checkout policy, retained patches/results, review/apply/reverify, and explicit cleanup. | M3 plus concrete workspace identity. Conflicting and nonconflicting worker changes tested with concurrent user edits. TUI exposes provenance and integration state. |
| M5 — Extensibility and interoperable clients | Stable useful Rust API, bounded JSON event/control interface, negotiated ACP, supervised plugins/MCP, scoped hooks, and reload. | P3; exact protocol schemas read and pinned. TUI/print/ACP consume one command/observation contract. API stability promises follow implementation evidence, not precede it. |
| M6 — Agent effectiveness and measured optimization | Controlled comparison of tool interfaces, context management, single/group strategies, optional programmatic/deferred tool use, E1 coordination, and E2 knowledge if promising. | Begins with M1 baseline; improvements ship individually after measured benefit. No strategy or memory system is mandatory solely because a reference implements it. |

M2 and M3 can advance as independent slices after M1; their relevant safety and observation requirements cannot be deferred. Core model/provider/tool/environment boundaries exist from M1. M5 stabilizes and broadens them rather than retrofitting extensibility into a closed design.

No milestone is called "Pi 2 parity". Current Pico/Pi2 work supplies architecture/failure-case evidence; ordinary Pi supplies workflow/product evidence. Ion can intentionally differ, but an omission or behavioral difference must be recorded rather than silently relabelled complete.

## 5. Capability coverage and release evidence

Maintain this matrix as implementation progresses. "Existing code" is not target validation; attach a test or live acceptance record before changing a row to verified. These entries are not a finding that the current binary lacks the capability.

| Capability family | Target gate | Current target evidence |
|---|---|---|
| Prompt, stream, tools, cancel, resume | M1 | Prototype evidence only; production path not yet validated against P1 |
| Root/worker identity and human control | M1, U2–U4, U11 | Prototype retained-worker and target/draft evidence only |
| Storage topology, history/output scaling | P2 | Proposed per-session boundary only; no comparative benchmark yet |
| Model/auth/input modality and context | M2 | Not assessed |
| Skills, prompts, completion, editing, shell UX | M2 | Not assessed |
| Queues, branch/fork, compaction, export/import policy | M2 | Not assessed |
| Group supervision, messages, budgets/jobs | M3 | Not assessed |
| Optional assignment coordination | E1, optionally M3 | Design only; disabled by default |
| Optional project knowledge | E2, optionally M6 | Design only; disabled by default |
| Workspace isolation, integration, verification, cleanup | M4, U9 | Not assessed |
| Plugins, hooks, MCP, scoped authority/reload | P3, M5 | Existing implementations available; new contract not validated |
| Rust/JSON/ACP frontend equivalence | M5 | Not assessed |
| Load, reconnect, startup/exit, terminal lifecycle | P2/P4, U6–U10 | Prior renderer evidence historical; new design not validated |

For each completed slice, record commit, invariant, automated tests, live checks where required, known limitations, and any intentional target change. Do not duplicate detailed test logs here; link the smallest durable evidence artifact.

## 6. Correctness suite

Use fake providers/environments, controllable clocks, and storage barriers for deterministic races. Run both orderings of settlement/cancel, spawn/group-stop, approval/revocation, message/dedup, watch-capture/commit, and terminal/output publication. Randomized state-machine traces supplement explicit cases; they do not replace them.

Crash injection covers before/after acceptance, external intent, external completion, settlement, artifact publication, successor creation, session-store creation/catalog publication, and catalog repair. Test that the UI's closed/disconnected state cannot make a task disappear from the store. Test a second writer against the actual OS ownership mechanism, including abnormal process exit.

Provider conformance tests cover semantic completion versus transport EOF, fragmented tool arguments, cancellation, malformed frames, usage uncertainty, unsupported capabilities, and model identity mismatch. No test should require real paid inference unless explicitly classified as a live provider check.

Use fixture files/temporary workspaces for shell and integration tests. Never run destructive tests against a developer checkout or credentials. Group stress tests include resource limits and malicious or faulty contribution behavior, not just happy-path fan-out.

## 7. Effectiveness and performance evaluation

Separate runtime overhead from model inference and tool cost. Record source revision, model/provider/version, prompt/tool configuration, enabled experimental capabilities, environment, seed where supported, limits, and raw outcomes. Keep an untouched evaluation set when iterating on tools/prompts/coordination/knowledge.

Compare at least single agent, bounded root/worker delegation, and any proposed richer strategy on the same task families. Evaluate both equal resource budgets and equal wall-clock constraints. Useful task families include a narrow bug fix, a cross-module change, repository investigation, independent review, a change whose outputs must be integrated, and a repeated project task where prior-session knowledge could plausibly matter.

Report task success and external verification, human intervention, elapsed time, tokens/cache usage, actual or estimated cost with uncertainty, edit failures, duplicated work, coordination overhead, integration conflicts, stale knowledge usage, and unresolved outcomes. Use repeated trials and uncertainty intervals appropriate to sample size. Do not cherry-pick the best run or score correctness solely through the producing model's self-report.

System benchmarks record startup/reopen, input/observer latency under load, memory versus historical size, active concurrency, storage/WAL/artifact bytes, lock/checkpoint behavior, query counts, backup/repair time, and cancellation/shutdown behavior. Select numerical budgets after baseline measurement; do not invent performance guarantees in architecture prose.

## 8. Reuse, migration, and scope

For an implementation slice, inspect existing Ion components only after its target contract is settled. Reuse a component when it satisfies that contract and is economical to adapt. Replace it when ownership or semantics conflict. Preserve useful regression tests even when their implementation is replaced.

Do not maintain permanent old/new production runtimes or duplicate transcript authorities. A temporary prototype must have a promotion/deletion decision. Before changing persisted formats or moving from the current root-wide database to per-session stores, explicitly implement migration or preserve an archive/refusal path; no silent data loss for architectural cleanliness.

Do not proliferate storage engines or databases by category. Separate stores are appropriate only for genuinely independent ownership/lifecycle boundaries such as session versus rebuildable catalog versus optional project knowledge/indexes. The core session transaction remains one authoritative store unless measurements force a redesigned ownership boundary.

Deferred until a concrete use case requires them: distributed writable sessions, multi-user remote hosting, arbitrary dynamic Rust ABI plugins, mandatory planner/task-board workflows, mandatory long-term memory, and storage backends beyond the local SQLite/artifact design. These do not block local group operation or a clean future transport/environment boundary.

The next work item remains production promotion of P1 semantics with a thin P4 trace. P2 should begin early enough that production P1 does not accidentally freeze the current root-wide physical database. E1/E2 wait until the corresponding core capabilities can serve as a clean baseline.

## Evidence log

2026-09-11: Target architecture, terminal interaction contract, reference ledger, and staged roadmap drafted. Documentation only. No runtime prototype, compiler gate, fault-injection run, performance result, or new terminal acceptance is claimed by this entry.

2026-09-12: Initial isolated P1 execution prototype validated at `78148e84d6120d5670a784ec3ecb07684577db1d`. It demonstrates durable idempotent admission/reopen, out-of-order effect settlement with call-order projection, retained worker lifetime, capacity-safe waiting, cancellation/invocation fencing, stable early-P4 command targets/drafts, and abruptly killed subprocess recovery that distinguishes retry-safe from indeterminate effects. The repository Rust 1.98.0 gates passed (`fmt`, strict workspace `clippy`, locked workspace tests). The evidence favors a typed re-entrant checkpoint/step task boundary with async confined to effect execution. See [docs/p1-execution-prototype.md](docs/p1-execution-prototype.md). P1 remains open until equivalent semantics and the remaining core fault cases are validated against production runtime/storage; no PTY or human terminal acceptance is claimed.

2026-09-12: Architecture research/design refreshed after inspecting current Pico2 and current Codex state/storage code plus SQLite WAL/ATTACH guarantees. Target storage now proposes one authoritative SQLite database per session/group, a rebuildable catalog, external artifacts, and separate optional project knowledge/index stores only across independent lifecycles. This is a P2 hypothesis, not a benchmarked result. Assignment coordination (E1) and project knowledge (E2) are explicitly experimental, disabled-by-default post-core systems.