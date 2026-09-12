# Ion roadmap

This roadmap delivers the target in [DESIGN.md](DESIGN.md) and [TERMINAL.md](TERMINAL.md). It replaces old Pi-parity work ordering, not the evidence contained in existing source and tests. Milestones are end-to-end capabilities, not a request to build every planned abstraction first.

## Current position

The product goal is established: an idiomatic Rust coding agent, single-agent by default, with optional cooperating workers and full TUI control. The architecture is a proposal supported by the [research record](docs/research.md). Runtime authoring, output/storage details, extension boundaries, and renderer choices remain explicitly prototype-gated.

| Deliverable | State | Evidence |
|---|---|---|
| Product contract, architecture, interaction specification | Drafted | DESIGN.md and TERMINAL.md |
| Primary-source review and decision register | Recorded | docs/research.md; pinned references where available |
| Rust task/transaction prototype P1 | Not started in this workstream | No prototype or measurement claimed |
| Output/storage prototype P2 | Not started in this workstream | No benchmark claimed |
| Extension/authority prototype P3 | Not started in this workstream | No new runtime tests claimed |
| Multi-agent TUI prototype P4 | Not started in this workstream | No new PTY or human acceptance claimed |
| Target architecture implemented | Not established | Current source predates this redesign; reuse is assessed per slice |

The documentation baseline is Ion commit `fca3346d0fa7aae82ffb77d02234b2f9b0d2975e`. That commit already addresses revision-witnessed peer authority and reload fencing; do not reopen it as a missing feature merely because earlier reviews described it as unfinished. Its commit-recorded test results are historical evidence, not tests rerun for this documentation change.

## 1. Immediate work: P1 with an early P4 trace

Next task: implement the smallest executable proof of the proposed task/transaction boundary. Read DESIGN.md sections 3–6 and the exact Pico/Goose references attached to P1. A standalone experiment is allowed; it must not become a permanent second production runtime.

Use a scripted provider and an instrumented tool, not a paid provider. Demonstrate:

1. Accept input durably; lose the reply; retry its key and receive the original receipt. Changed content with the same key rejects.
2. Run a turn with two tool calls whose completion order differs from source order.
3. Spawn one retained worker, finish the spawning tool, and keep the worker addressable.
4. Wait for that worker without retaining the only execution permit it needs.
5. Race task settlement against cancellation in both orders, including late output.
6. Reopen after provider/tool intent, distinguish retry-safe and uncertain effects, and preserve input disposition.
7. Feed the same trace to a minimal group/focused-agent view, demonstrating stable target IDs and separate drafts.

Compare async task methods with typed checkpoint/settlement commands against a re-entrant step API only as needed to choose one. Record which method has fewer duplicated states, clearer resource ownership, and simpler recovery tests. Lines of code alone do not decide. Resolve ID allocation, immutable input encoding, invocation fencing, and the commit/storage-thread boundary before promoting the prototype.

Exit: one recommended Rust API, an explicit schema/transaction sketch for this slice, passing deterministic tests, and a short evidence entry below. A failed hypothesis changes the design rather than becoming a hidden exception in the implementation.

## 2. Remaining architecture gates

### P2 — Output, history, and storage

Implement the chosen output owner and snapshot/stream boundary against SQLite and an instrumented test store. Exercise a large model response, large shell output, tool-to-job output handoff, slow storage, disk exhaustion, process loss, and watch overflow.

Measure cold/warm context queries with short and long histories, dense context edits, and shallow/deep forks. Include a fixture with 100,000 terminal tasks and a small live set; opening execution must not decode every historical task payload. Record RSS, retained objects, bytes written including WAL/spool files, query counts, restart time, and time to interactive view.

Choose output checkpoint cadence, spill thresholds, page sizes, and retention rules from the measurements. Document exactly how much provisional output can disappear on process loss. Preserve final durable results and control decisions regardless of provisional loss. Test artifact publication before reference, orphan cleanup, and receipt/tombstone retention so cleanup cannot re-enable duplicate work.

Exit: coherent snapshot/output cursors, tested durable/provisional separation, bounded active residency, and recorded performance envelopes. No blanket O(context) or constant-memory claim without the corresponding fork/edit workload.

### P3 — Extensions, authority, and reload

Implement one tool contribution, one observational contribution, and one typed behavior hook through the intended boundaries. Define trusted local subprocess plugins separately from OS-sandboxed extensions; RPC alone does not isolate host files, processes, or credentials.

Test preparation failure, required-hook failure, cancellation, oversized responses, peer outage, explicit removal/replacement, stale approval, and recovery with a missing implementation. Test revocation versus effect dispatch in both orders and name reuse after removal. Recheck child authority as request intersect parent ceiling intersect host policy; model/configuration changes cannot expand it. Preserve authority independent of discovery availability.

Exit: exact capability and lifecycle rules with tests, one public contribution mechanism per purpose, and explicit behavior for already-dispatched irreversible actions. Do not promise transactional rollback of external process effects.

### P4 — TUI and interaction

Prototype the conversation, group, and focused-worker views using runtime traces, then real runtime commands. Compare renderer choices against interaction requirements, not existing library allegiance. Run the U1–U12 scenarios in TERMINAL.md.

Include a narrow terminal, Unicode/multiline editing, hidden-worker approvals, focus changes during command completion, output floods, reconnection, and terminal restoration. Inspection must remain read-only. Measure input-to-frame and cancellation-command latency under load separately from provider latency.

Exit: an agreed layout/input model, PTY/reducer coverage, and recorded human terminal acceptance for behaviors automation cannot establish. P4 begins with P1; it is not postponed until a complete runtime exists.

## 3. Delivery milestones

| Milestone | End-to-end result | Dependencies and exit evidence |
|---|---|---|
| M1 — One runtime, two agents | One real provider, root plus one worker, native read/edit/shell slice, durable input and recovery, visible/control-capable TUI, and a headless trace adapter. Single-agent mode adds no swarm instructions/tools. | P1 chosen; P2 minimum persistence/output path; P4 thin interaction. Real provider credentials remain local. Prove cancellation, uncertain effect, worker lifetime, and target-safe TUI flows. |
| M2 — Daily coding | Model changes, authenticated providers, images where supported, configurable tools, prompt/context resources, skills, compaction, history/forks, queues, search/completion, external editor, export, and settings. | M1; P2 query/output evidence. Capabilities work through runtime APIs and TUI, not UI-only stubs. Establish the explicit coverage matrix below. |
| M3 — Controlled group work | Nested workers, peer messages, group limits, optional shared assignments, background jobs, safe pause/cancel scopes, and retained results. | M1 ownership; P2 output; P3 authority where dynamic contributions are involved. No lost messages, duplicate charging, permit starvation, or child grant widening under fault injection. |
| M4 — Parallel implementation and integration | Worktree-backed mutating workers, deliberate dirty-checkout policy, retained patches/results, review/apply/reverify, and explicit cleanup. | M3 plus concrete workspace identity. Conflicting and nonconflicting worker changes tested with concurrent user edits. TUI exposes provenance and integration state. |
| M5 — Extensibility and interoperable clients | Stable useful Rust API, bounded JSON event/control interface, negotiated ACP, supervised plugins/MCP, scoped hooks, and reload. | P3; exact protocol schemas read and pinned. TUI/print/ACP consume one command/observation contract. API stability promises follow implementation evidence, not precede it. |
| M6 — Agent effectiveness and measured optimization | Controlled comparison of tool interfaces, context management, single/group strategies, and optional programmatic/deferred tool use. | Begins with M1 baseline; improvements ship individually after measured benefit. No new strategy is mandatory solely because a reference implements it. |

M2 and M3 can advance as independent slices after M1; their relevant safety and observation requirements cannot be deferred. Core model/provider/tool/environment boundaries exist from M1. M5 stabilizes and broadens them rather than retrofitting extensibility into a closed design.

No milestone is called 'Pi 2 parity'. Pi supplies workflow and failure-case evidence. Ion can intentionally differ, but an omission or behavioral difference must be recorded rather than silently relabelled complete.

## 4. Capability coverage and release evidence

Maintain this matrix as implementation progresses. 'Existing code' is not target validation; attach a test or live acceptance record before changing a row to verified. These entries are not a finding that the current binary lacks the capability.

| Capability family | Target gate | Current target evidence |
|---|---|---|
| Prompt, stream, tools, cancel, resume | M1 | Not assessed against the new target |
| Root/worker identity and human control | M1, U2–U4, U11 | Not assessed |
| Model/auth/input modality and context | M2 | Not assessed |
| Skills, prompts, completion, editing, shell UX | M2 | Not assessed |
| Queues, branch/fork, compaction, export/import policy | M2 | Not assessed |
| Group supervision, messages, budgets, assignments/jobs | M3 | Not assessed |
| Workspace isolation, integration, verification, cleanup | M4, U9 | Not assessed |
| Plugins, hooks, MCP, scoped authority/reload | P3, M5 | Existing implementations available; new contract not validated |
| Rust/JSON/ACP frontend equivalence | M5 | Not assessed |
| Load, reconnect, startup/exit, terminal lifecycle | P2/P4, U6–U10 | Prior renderer evidence historical; new design not validated |

For each completed slice, record commit, invariant, automated tests, live checks where required, known limitations, and any intentional target change. Do not duplicate detailed test logs here; link the smallest durable evidence artifact.

## 5. Correctness suite

Use fake providers/environments, controllable clocks, and storage barriers for deterministic races. Run both orderings of settlement/cancel, spawn/group-stop, approval/revocation, message/dedup, watch-capture/commit, and terminal/output publication. Randomized state-machine traces supplement explicit cases; they do not replace them.

Crash injection covers before/after acceptance, external intent, external completion, settlement, artifact publication, and successor creation. Test that the UI's closed/disconnected state cannot make a task disappear from the store. Test a second writer against the actual OS ownership mechanism, including abnormal process exit.

Provider conformance tests cover semantic completion versus transport EOF, fragmented tool arguments, cancellation, malformed frames, usage uncertainty, unsupported capabilities, and model identity mismatch. No test should require real paid inference unless explicitly classified as a live provider check.

Use fixture files/temporary workspaces for shell and integration tests. Never run destructive tests against a developer checkout or credentials. Group stress tests include resource limits and malicious or faulty contribution behavior, not just happy-path fan-out.

## 6. Effectiveness and performance evaluation

Separate runtime overhead from model inference and tool cost. Record source revision, model/provider/version, prompt/tool configuration, environment, seed where supported, limits, and raw outcomes. Keep an untouched evaluation set when iterating on tools/prompts.

Compare at least single agent, bounded root/worker delegation, and any proposed richer strategy on the same task families. Evaluate both equal resource budgets and equal wall-clock constraints. Useful task families include a narrow bug fix, a cross-module change, repository investigation, independent review, and a change whose outputs must be integrated.

Report task success and external verification, human intervention, elapsed time, tokens/cache usage, actual or estimated cost with uncertainty, edit failures, duplicated work, integration conflicts, and unresolved outcomes. Use repeated trials and uncertainty intervals appropriate to sample size. Do not cherry-pick the best run or score correctness solely through the producing model's self-report.

System benchmarks record startup/reopen, input/observer latency under load, memory versus historical size, active concurrency, storage/WAL/artifact bytes, query counts, and cancellation/shutdown behavior. Select numerical budgets after baseline measurement; do not invent performance guarantees in architecture prose.

## 7. Reuse, migration, and scope

For an implementation slice, inspect existing Ion components only after its target contract is settled. Reuse a component when it satisfies that contract and is economical to adapt. Replace it when ownership or semantics conflict. Preserve useful regression tests even when their implementation is replaced.

Do not maintain permanent old/new production runtimes or duplicate transcript authorities. A temporary prototype must have a promotion/deletion decision. Before changing persisted formats, explicitly implement migration or preserve an archive/refusal path; no silent data loss for architectural cleanliness.

Deferred until a concrete use case requires them: distributed writable sessions, multi-user remote hosting, arbitrary dynamic Rust ABI plugins, backend proliferation, and a mandatory workflow/planner framework. These do not block local group operation or a clean future transport/environment boundary.

The next work item is P1 with a thin P4 trace. Do not resume old roadmap tasks by priority alone: reconcile them with this target and preserve already-fixed correctness behavior.

## Evidence log

2026-09-11: Target architecture, terminal interaction contract, reference ledger, and staged roadmap drafted. Documentation only. No runtime prototype, compiler gate, fault-injection run, performance result, or new terminal acceptance is claimed by this entry.
