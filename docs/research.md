# Ion architecture research

Evidence reviewed for the proposed target through 2026-09-12, America/Los_Angeles. [DESIGN.md](../DESIGN.md) contains recommendations; [ROADMAP.md](../ROADMAP.md) identifies the work needed to validate them. This is a decision-oriented source review, not a runtime benchmark or a ranking of agent products.

## 1. Evidence rules

Source code establishes what a specific revision implements. Tests establish the cases asserted, not that those tests passed in this review. Specifications establish intended contracts. Product documentation establishes the public behavior claimed by its publisher. Performance and effectiveness claims require reproducible workloads and measurements.

Record both the reference revision and the exact inspected path. A moving branch is a discovery surface, not a compatibility target. An upstream change can reopen a decision when it supplies new evidence; it does not automatically change Ion's requirements.

Do not flatten projects into a generic "SOTA agents" set. Current Pico/Pi 2 work is the primary minimal-harness design reference because it directly explores many of Ion's core lifecycle questions. Codex is a primary production-engineering reference when its public Rust implementation answers a concrete runtime, storage, client, or multi-agent question. Ordinary shipping Pi is practical product/workflow evidence, not the architectural baseline for future Ion. Goose, OpenHands, DSH, Grok Build, Oh My Pi, and other systems are consulted only when a specific mechanism is relevant; age, popularity, or feature count is neither positive nor negative architectural evidence.

The existing Ion implementation is not scored against references. Reuse is decided against the accepted target contract for each slice. Historical design and fixes remain available in Git.

## 2. Pi/Pico evidence layers

At revision `71dca871bc80b6bc97be37f0ca3189399d651fff`, ordinary Pi still supplies useful product/workflow evidence while `packages/agent/docs/pico2.md` is the current forward design under review. Pico2 explicitly says it is a design, not an implementation claim. It replaces older resident-tree/fold experiments with separately-lived transcript, tasks, typed values/lists, and an explicit context list. [R1, R2](#references)

Pico2's important architectural properties for Ion are:

- a session-global serialized command line and committed sequence;
- immutable transcript entries distinct from mutable durable tasks;
- explicit model context rather than equating transcript with prompt state;
- tasks with start/inflight/waiting/terminal roles and explicit recovery/cancellation stories;
- one atomic command for settlement plus required successors;
- working scopes/scratch that are durable state but not conversation history;
- bounded read models rather than reconstructing all historical state into residency;
- private physical storage representation, with SQLite/JSONL described as alternative backends rather than part of the public agent model.

Ion should use those as design evidence, not compatibility requirements. In particular, Pico2 identifies entries/tasks/conversations by journal sequence; Ion currently prefers distinct typed identities plus a separate CommitSeq. The P1 prototype supports that separation, and production schema work must still validate its physical cost. [R1](#references)

Older AgentHarness and Pico documents remain useful for failure-case inventories and rationale, but they do not override the current Pico2 design when the two differ. Do not call ordinary Pi, the old AgentHarness, and Pico2 one implementation named "Pi 2".

## 3. Findings and Ion decisions

### Durable lifecycle versus agent behavior

Pico2 keeps task lifecycle generic while kind-specific state/code owns generation, tools, jobs, subagents, collapse, and approvals. Settlement and successors belong in one command; an unowned inflight task recovers rather than blindly rerunning. [R1](#references)

Ion P1 independently compared an async task-authoring candidate against an explicit typed re-entrant step/checkpoint boundary. The prototype selected the re-entrant boundary: durable continuation is explicit typed state; async Rust remains inside providers, tools, processes, timers, and other effect adapters. Keeping both as public task frameworks would duplicate lifecycle state. See [P1 evidence](p1-execution-prototype.md).

Goose's re-entrant state-machine work was useful corroborating evidence during that comparison, but it is no longer an unresolved peer architecture that Ion needs to follow. Its broader product architecture is not treated as a baseline. [R5](#references)

### History and context

Pico2 sharply separates transcript, context, task state, and versioned values/lists. That is stronger evidence for Ion's existing direction than the older "conversation as provider-message vector" model. [R1](#references)

Recommendation: keep canonical historical facts append-only, model context explicit and reconstructable, and execution state separate from both. Support indexed range/point reads and keep cold historical state out of live residency. Measure deep forks, dense context edits, large task histories, and context rebuild costs before selecting physical indexes or caches.

Do not adopt Pico's sequence-as-identity allocation merely for resemblance. Ion's P1 direction keeps object identity and commit order distinct unless production measurements show a concrete cost that outweighs the semantic separation.

### Input, cancellation, and uncertain effects

Pico's designs distinguish admission from placement/execution, serialize cancellation with task authority, and make caller disappearance distinct from cancellation. Ion retains those principles but intentionally rejects request-key reuse when the same key is rebound to different target/content/mode.

Ion also makes external-effect uncertainty an explicit first-class contract: retry-safe, reconcilable, or no-safe-retry. A durable `running` marker alone is not sufficient evidence that an interrupted shell/process/provider effect can be repeated. Cancellation is not rollback, and late results are fenced by task identity plus invocation generation. P1 validated the basic race/recovery shape; production promotion remains open.

### Agent identity and cooperation

Codex's inspected spawn/restoration code separates persistent identity restoration from runtime residency and provides useful production evidence for retained-agent semantics. DSH's team subsystem independently distinguishes membership, attributed messaging, revisioned assignments, dependency edges, and advisory path scopes. [R3, R4](#references)

Recommendation: retained agents outlive turns; supervision, messaging, assignment dependencies, task dependencies, conversation ancestry, and workspace sharing remain different relationships. Waiting must not consume the execution capacity needed by the dependency. A spawn tool finishing does not kill a retained worker.

Child authority is request intersect parent ceiling intersect host policy. History forks, model/config changes, plugin reload, or name reuse cannot widen it.

### Optional coordination and knowledge

A task/assignment board can improve multi-agent synchronization, but it is an optional coordination mechanism rather than part of the minimal harness contract. Same-session assignment mutations that influence ownership/messaging belong inside that session's transaction boundary. Whether model-facing assignment tools improve task success enough to justify their context/coordination cost is an effectiveness experiment, not an architectural assumption. DSH is evidence for revisioned assignment semantics, not a mandate to copy its team implementation. [R3](#references)

Longer-lived knowledge/memory is a separate concern from session history. Codex now has dedicated goal, queue, and memory stores and an experimental memory pipeline; that establishes that a production agent can benefit from independently-lived state categories, but it does not establish the right extraction/retrieval policy for Ion. [R11](#references)

Ion should experiment later with project/workspace-scoped knowledge that carries provenance, revisions/freshness, supersession/invalidation, and explicit bounded retrieval. Candidate agent-written knowledge should not silently become trusted system state. Exact lexical/semantic retrieval, consolidation, contradiction resolution, and prompt/tool presentation require controlled effectiveness evaluation. The feature remains disabled by default until it earns its cost.

### Workspaces and integration

Grok Build documents background children, explicit worktree isolation, root-to-child messaging, continuation, and apply-style integration. These are useful product contracts for workspace semantics, not proof of its runtime architecture. [R6](#references)

Recommendation: environment binding remains independent of agent type and context origin. Parallel mutating workers should normally use isolated workspaces/worktrees from explicit bases. Applying a worker result is a separate admitted effect with fresh base/dirty-state checks and post-apply verification.

### TUI as an agent-group client

Oh My Pi's Agent Hub provides useful interaction evidence for roster/inspection/narrow-terminal behavior. Current Codex client/command-center work is also relevant when it exposes concrete production behavior. Neither UI defines Ion's execution semantics. [R7, R11](#references)

Recommendation: inspection is read-only; focus does not implicitly wake an agent. Inputs, approvals, and delayed replies carry stable target identities independent of current focus. Frontends consume bounded runtime observations rather than loading every transcript.

### Managed-agent APIs and model-facing effectiveness

OpenAI's Agents API separates managed harness, environment, and application-server concerns and exposes compaction, deferred tool discovery, programmatic calling, and optional subagents. This is useful product/architecture evidence without requiring Ion to depend on the managed service. [R8](#references)

Recommendation: keep behavior, environment, and clients separable. Evaluate tool-result shaping, editing representation, deferred discovery, programmatic tool use, context strategy, and single/group policies through controlled task outcomes. Runtime engineering can improve reliability, control, recovery, and coordination; it does not by itself prove the model solves more coding tasks.

### Storage ownership and database boundaries

Tokio documents that already-running blocking work cannot generally be aborted, which supports keeping SQLite behind bounded blocking ownership rather than pretending an async wrapper makes database mutation cancellable. [R9](#references)

SQLite WAL is a good local substrate, but database partitioning must follow atomicity and lifecycle. SQLite documents that multi-file transactions using attached databases are crash-atomic only under conditions that exclude WAL; in WAL mode individual files remain atomic but a host crash can leave an attached multi-file transaction partially committed across files. Therefore Ion must not split one session's core transaction across several WAL databases. [R10](#references)

The leading topology is one authoritative SQLite database per session/group, plus its artifact directory. A small global catalog may index/discover sessions but must be rebuildable and cannot be required for session correctness. Optional project knowledge, global/search indexes, caches, or similar independently-lived state can use separate stores and synchronize through explicit idempotent/outbox protocols when authoritative.

This is a change from the current implementation, which uses one SQLite database per Ion data root. The target is not yet validated. P2 must measure independent concurrent sessions, lock/checkpoint behavior, backup/archive/delete, large histories, output spooling, restart, and catalog repair before the topology is treated as stable.

Current Codex provides supporting but non-prescriptive evidence: its Rust state runtime opens separate SQLite stores for state, logs/history, goals, memories, and queue, and explicitly separates logs/history to reduce lock contention. Those categories have independent lifecycles; Ion should adopt the principle, not the schema. [R11](#references)

## 4. Decision register

These are recommended choices for validation, not immutable architecture promises.

| Decision | Proposed choice | Reopen when |
|---|---|---|
| Runtime composition | Shared durable task lifecycle; typed re-entrant behavior state | Production P1 exposes duplicated state, awkward APIs, or worse recovery |
| Group boundary | One writer and one authoritative core database per local session/group | Measured contention or a real independent-session deployment requires another ownership boundary |
| Collaboration | Explicit messages; optional revisioned assignment board; supervision separate | Controlled tasks justify different primitives |
| Knowledge | Optional project/workspace service with provenance and bounded retrieval | E2 evidence favors another scope/model or shows no benefit |
| Context | Immutable historical facts plus explicit context controls and bounded indexed reads | P2 identifies unsound projection or unacceptable fork/edit costs |
| IDs | Typed identities; commit sequence separate | Production P1/P2 shows a concrete representation cost that outweighs separation |
| Storage partition | Core session state together; independent-lifecycle stores separate | Atomicity, contention, backup, or measurement evidence contradicts the boundary |
| Plugins | Trusted compiled interfaces plus supervised versioned subprocess contributions | A concrete dynamic task-authoring or stronger isolation requirement justifies a new boundary |
| Output | Durable facts/checkpoints plus provisional incremental presentation | P2 cannot provide coherent recovery/reconnect within acceptable cost |
| TUI | Inline-first conversation, responsive group/worker views, explicit control | P4 usability and PTY evidence favors another renderer/layout |
| Agent policy | Single agent default; optional bounded delegation | Controlled tasks show another default wins at equal resource/deadline budgets |

## 5. Remaining focused research

The core draft does not depend on an exhaustive agent survey. Before stabilizing a boundary, inspect only references that answer its unresolved question:

- P1 production promotion: current Pico2 task/line semantics and Codex capacity/residency/cancellation only where they expose a concrete unresolved race or API question.
- P2: Pico2 storage/read-model/output rules, SQLite WAL/backup/ATTACH behavior, current Codex storage partitioning, and Ion query plans/benchmarks. Validate per-session databases rather than assuming them.
- P3: scoped contribution disposal/failure paths, Codex permission/authority handling, and concrete OS sandbox boundaries.
- P4: current Pi/Pico, Codex, Oh My Pi, and other terminal clients only for specific interaction/event questions.
- Provider/environment milestones: inspect Kimi/Wire/KAOS, ACP, or another implementation only when an exact provider/environment/protocol question requires it.
- Effectiveness: editing representations, tool-result shaping, context/compaction, deferred discovery, programmatic execution, delegation policy, assignment tools, and knowledge retrieval using reproducible task evidence.

Goose and OpenHands are not default comparison targets. They remain available if a concrete mechanism becomes relevant. Additional projects earn inclusion by answering an unresolved question, not by being established or feature-rich. Neither rewrite history nor popularity is sufficient to dismiss or select a design.

## References

### R1 — Current Pico2 design

Publisher: earendil-works/pi. Revision: `71dca871bc80b6bc97be37f0ca3189399d651fff`, inspected September 12, 2026.

- [pico2.md](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/agent/docs/pico2.md): current design under review; transcript/tasks/values/context, task roles and recovery, single session line, working scopes, storage/read models, watches, and open decisions. This is a specification, not an implementation claim.

### R2 — Ordinary Pi and earlier harness evidence

Publisher: earendil-works/pi. Revision: `71dca871bc80b6bc97be37f0ca3189399d651fff`, inspected September 12, 2026.

- [Coding-agent SDK construction](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/coding-agent/src/core/sdk.ts): current ordinary coding-agent path at the inspected revision.
- [Experimental mini](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/coding-agent/src/experimental/mini/README.md): experimental client/host/worker topology and declared omissions.
- [Earlier harness specification](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/agent/docs/harness.md): historical durable-operation contracts and failure cases. Use for rationale/regressions when compatible with Pico2; it is not the current future design authority.

### R3 — DSH/Cordis and teams

Publisher: deepseek-ai/deepseek-harness. Revision: `c291e7961a515f6d7af9304e7fd1d257929aef26`, inspected September 11, 2026.

- [Architecture](https://github.com/deepseek-ai/deepseek-harness/blob/c291e7961a515f6d7af9304e7fd1d257929aef26/docs/architecture.md): composition, profiles, events, and ownership.
- [Agent teams](https://github.com/deepseek-ai/deepseek-harness/blob/c291e7961a515f6d7af9304e7fd1d257929aef26/docs/subsystems/agent-team.md): experimental identity, mailbox, assignment revisions, and advisory write scopes.

### R4 — Codex agent identity and spawn

Publisher: OpenAI. Inspected source revision: `89c8bcf37d64be69e4c8286f4541c1a84ed312a4`.

- [Agent spawn and restoration](https://github.com/openai/codex/blob/89c8bcf37d64be69e4c8286f4541c1a84ed312a4/codex-rs/core/src/agent/control/spawn.rs): narrow evidence for identity restoration, residency, and inherited context/accounting. Broader conclusions require adjacent source/tests.

### R5 — Goose re-entrant execution evidence

Publisher: aaif-goose/goose. Revision: `50666ae0b9a51e260b52b7efbab2e4e020346e94`, inspected September 11, 2026.

- [State-machine module](https://github.com/aaif-goose/goose/blob/50666ae0b9a51e260b52b7efbab2e4e020346e94/crates/goose/src/agents/state_machine/mod.rs): historical corroborating evidence for an ordered re-entrant pipeline. Not an Ion baseline or claim that Goose has equivalent durability/recovery semantics.

### R6 — Grok Build subagents and workspaces

Publisher: xai-org/grok-build. Revision: `37949780c144e37df692e3d669051a21fec24f20`, inspected September 11, 2026.

- [Subagents and personas](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/16-subagents.md): documented background/resume/messaging/worktree behavior. Product contract evidence, not a runtime benchmark.

### R7 — Oh My Pi Agent Hub

Publisher: can1357/oh-my-pi. Revision: `f97fa5c95010b62ac34c7357f9a1cae6975e12d6`, inspected September 11, 2026.

- [Agent Hub](https://github.com/can1357/oh-my-pi/blob/f97fa5c95010b62ac34c7357f9a1cae6975e12d6/docs/agent-hub.md): responsive roster/inspector, steering, persisted workers, and focus-triggered revival. Useful UI evidence; Ion intentionally keeps inspection read-only.

### R8 — OpenAI Agents API

Publisher: OpenAI. Announcement dated September 10, 2026; official documentation inspected September 11, 2026. Web documentation is not commit-pinned.

- [Introducing the Agents API](https://openai.com/index/introducing-the-agents-api/): managed Codex harness, context management, tool discovery, programmatic calling, and optional multi-agent operation.
- [Architecture](https://developers.openai.com/api/docs/guides/agents-api/architecture): harness, environment, and application-server boundaries. Architectural/product evidence, not a dependency proposal.

### R9 — Tokio cancellation and blocking work

Publisher: Tokio maintainers. Documentation for Tokio 1.53.1 inspected September 11, 2026; this does not select Ion's dependency version.

- [spawn_blocking](https://docs.rs/tokio/1.53.1/tokio/task/fn.spawn_blocking.html): running blocking tasks are not generally abortable; timeout limits waiting, not execution.

### R10 — SQLite durability, WAL, and multi-database transactions

Publisher: SQLite project. Official living documentation inspected September 12, 2026; exact SQLite version must be selected and verified with the implementation.

- [PRAGMA synchronous](https://www.sqlite.org/pragma.html#pragma_synchronous): synchronization modes and WAL durability.
- [Write-ahead logging](https://www.sqlite.org/wal.html): WAL lifecycle, checkpoints, file handling, and local-host constraints.
- [ATTACH DATABASE](https://www.sqlite.org/lang_attach.html): multi-database transaction guarantee; crash-atomicity across attached files excludes WAL-mode main databases. This is why one session transaction must not be split across multiple WAL files.

### R11 — Current Codex state/storage subsystems

Publisher: OpenAI. Revision: `ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8`, inspected September 12, 2026.

- [State runtime](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/state/src/runtime.rs): separate SQLite pools/stores for state, logs/thread history, goals, memories, and queue; source comment explicitly cites lock-contention reduction for logs/history separation.
- Adjacent goals/memories/queue modules are evidence of independently-lived state surfaces, not a schema template for Ion. Their effectiveness is not inferred from their existence.