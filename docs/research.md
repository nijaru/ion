# Ion architecture research

Evidence reviewed for the proposed target on 2026-09-11, America/Los_Angeles. [DESIGN.md](../DESIGN.md) contains recommendations; [ROADMAP.md](../ROADMAP.md) identifies the work needed to validate them. This is a decision-oriented source review, not a runtime benchmark or a complete audit of the referenced projects.

## 1. Evidence rules

Source code establishes what a specific revision implements. Tests establish the cases asserted, not that those tests passed in this review. Specifications establish intended contracts. Product documentation establishes the public behavior claimed by its publisher. Performance claims require reproducible workloads and measurements; none of the referenced harnesses was benchmarked in this documentation change.

Record both the reference revision and the exact inspected path. A moving branch is a discovery surface, not a compatibility target. An upstream change can reopen a decision when it supplies new evidence; it does not automatically change Ion's requirements.

The existing Ion implementation is not scored against these references. It will be inspected for reuse only when implementing an accepted slice. Its historical design and recent fixes remain available in Git and the historical document index.

## 2. Pi's three relevant layers

At the inspected revisions, the ordinary coding-agent SDK still constructs Agent and AgentSession. The earlier durable AgentHarness exists separately and is exercised by the experimental mini client. Mini is explicitly an incomplete application built to investigate the harness and presentation boundary. Code being present on main therefore does not mean the regular CLI uses it. [R2](#references)

Pico is a clean-room harness design and initial implementation under packages/agent/src/harness/pico/. Its current authority is pico-simple-handoff.md, not the older pico-v3.md or preserved prototypes. The status file records compile-time declarations and tests as WP1, with mutation algebra and MemoryStorage next. Provider, tool, and client integration still contain gated interfaces. This supports studying Pico as a candidate future foundation; it does not establish that it is the shipped Pi runtime or a final release commitment. [R1](#references)

Use the ordinary Pi application for practical workflow evidence, the earlier AgentHarness for durable-operation contracts and failure cases, and Pico for the proposed redesign. Do not flatten them into a single 'Pi 2' baseline.

## 3. Findings and Ion decisions

### Durable lifecycle versus agent behavior

Pico places immutable inputs, complete checkpoints, task ownership, and execution/recovery/abort methods behind a common lifecycle. It makes settlement and successor work atomic. Its scheduler is not supposed to interpret generation or compaction phases. [R1](#references)

Recommendation: a small execution lifecycle with typed Rust behavior state is the leading design. Keep model/tool/compaction details out of the generic scheduler. The task implementation still needs exact effect intent when it performs repeat-sensitive work; a generic running marker is not a universal recovery strategy.

Competing option: Goose's state-machine module describes an ordered re-entrant pipeline over persisted conversation state, with concrete operations separate from the protocol. Its new path is enabled through GOOSE_STATE_MACHINE; the migration is not evidence that every user runs the new path. [R5](#references)

P1 compares the API clarity and failure behavior of these approaches on the same small workload. It does not build two entire harnesses or preserve both public APIs indefinitely.

### History and context

Pico materializes model projections and context controls on immutable entries. Forks share a fixed prefix, and later source changes do not affect them. Its SQLite direction avoids loading all terminal history into execution residency. [R1](#references)

Recommendation: keep historical meaning locally reconstructable, support indexed range reads, and separate history from live work. Measure deep forks and dense context edits; a small final prompt does not imply cheap cold reconstruction. Exact caches and physical indexes are P2 decisions.

Do not adopt Pico's ID allocation merely for resemblance. Ion's proposal separates object identity from commit sequence while retaining writer-owned allocation and durable-before-visible identity. P1 must settle the representation.

### Input and cancellation

Pico distinguishes input acceptance, placement, and final result. It serializes cancellation marks, invocation authority, terminal commits, and watcher capture. Its request-key rule returns the first receipt even when the retry changes the payload. [R1](#references)

Recommendation: retain durable input dispositions and scoped cancellation, but reject conflicting key reuse in Ion. A caller timeout is not cancellation of durable work. Terminal outcomes must preserve uncertain external effects.

Ion also proposes retaining cancelled-turn queued inputs in a paused state for explicit withdrawal/resubmission. This is an intentional product choice, not a claim of matching Pico's conversation-abort queue policy.

### Composition and extensions

DSH's Cordis architecture makes the model adapter, tool registry, persistence, and loop replaceable plugins. Registrations unwind with plugin lifetime. Its application profiles distinguish live-reload applications from one-shot and stdio applications that compose once at startup. [R3](#references)

Recommendation: make meaningful behavior and environment boundaries replaceable, with explicit lifetimes and atomic publication. Do not reproduce a universal dynamic context graph merely because the reference uses one. Runtime invariants must remain enforced when behavior changes.

A subprocess RPC boundary is an API and lifecycle boundary, not automatically a security sandbox. Code running with the host user's privileges can access resources outside the protocol unless an OS sandbox actually constrains it. P3 must define trusted local plugins versus restricted extensions, credential/environment filtering, and the effective OS authority of every execution path.

### Agent identity and cooperation

Codex's inspected spawn implementation separates persisted identity restoration from runtime reopening and excludes parent cumulative token usage from child model context. These are narrow observations from the named functions, not a blanket claim about all its scheduling or accounting. [R4](#references)

DSH's team documentation independently represents durable membership, attributed messages with target-side deduplication, and revisioned assignments with dependency edges. Its advisory path scopes are not locks. [R3](#references)

Recommendation: retained agents outlive turns; supervision, messaging, assignment dependencies, and workspace sharing are independent. A spawn tool finishing must not implicitly kill a retained worker. Messaging peers does not grant access to their tools or cancellation authority. Prefer one session writer for an initial local group so internal message admission can be atomic; design a real outbox only when cross-session delivery is needed.

Child authority must be the intersection of the request, the parent's current ceiling, and host policy. It can narrow but cannot expand through a model override, history fork, configuration reload, or name reuse. This is an Ion requirement to test, not an assumption inferred from a role called 'read-only'.

### Workspaces and integration

Grok Build documents background children, explicit worktree isolation, root-to-child messaging, and continuation from completed child history. On continuation it rebuilds prompt/tools from current definitions. Worktree changes remain separate until an apply operation integrates them. [R6](#references)

Recommendation: make environment binding independent of agent type and context origin. Record exactly what is retained versus re-resolved. Review and apply worker changes as a separate effect with a known base and fresh verification. Worktrees do not remove logical integration conflicts or automatically include uncommitted parent changes.

### TUI as an agent-group client

Oh My Pi's Agent Hub documents a flat/tree roster, per-agent inspection, narrow-terminal switching, transcript access, and direct steering. It also revives parked agents when focused. [R7](#references)

Recommendation: borrow the visibility and responsive layout, not focus-triggered execution. Ion inspection is read-only. Input routing, approvals, and delayed replies must retain stable targets across focus changes. Session summaries and selected transcript pages should replace directory scans or full-history loading as the authoritative observation path.

Pico's whole-commit watch delivery and reconnect-on-overflow provide a useful correctness model. Ion's proposal deliberately distinguishes durable commits from provisional output frames, so a consumer must use channel offsets and an observation epoch rather than treat all frames as durable commit events. P2 and P4 test this trade-off. [R1](#references)

### Managed-agent APIs and model-facing effectiveness

OpenAI's September 10 Agents API announcement describes managed Codex harness execution, compaction, deferred tool discovery, programmatic tool calling, and optional subagents. The architecture documentation separates harness, environment, and application server. Self-hosting the environment is not self-hosting that managed harness. [R8](#references)

Recommendation: keep Ion's behavior, environment, and clients separable, and evaluate deferred discovery and programmatic calling as optional model-facing capabilities. No API dependency is required. Customer testimonials are not comparative evidence that those capabilities improve Ion's coding tasks.

### Rust and storage boundaries

Tokio documents that already-running spawn_blocking work cannot generally be aborted, and a shutdown timeout stops waiting rather than stopping that work. This constrains database and process cleanup design. [R9](#references)

SQLite documents WAL's synchronization and backup implications. FULL adds commit synchronization; WAL can still require checkpoint maintenance, and the application must not discard uncheckpointed WAL data. SQLite guarantees are conditional on the filesystem/VFS honoring them. [R10](#references)

Recommendation: use bounded blocking ownership and local SQLite first, explicitly configure and verify durability, and test storage failure rather than assuming an async wrapper makes I/O cancellable. Do not add storage backends before a concrete requirement.

## 4. Decision register

These are recommended choices for validation, not claims of completed implementation.

| Decision | Proposed choice | Reopen when |
|---|---|---|
| Runtime composition | Shared durable task lifecycle; typed behavior internals | P1 exposes awkward APIs, duplicated mechanisms, or harder recovery than a re-entrant pipeline |
| Group boundary | One writer per local session containing the root and workers | Measured contention or a real independent-session deployment requires another boundary |
| Collaboration | Explicit messages and optional assignment board; supervision remains separate | An evaluated policy requires additional primitives |
| Context | Immutable entries, stored projections/controls, bounded indexed reads | P2 identifies unsound projection or unacceptable fork/edit costs |
| IDs | Typed session-local identities; commit sequence separate | P1 selects the physical schema and allocation method |
| Plugins | Trusted compiled interfaces plus supervised versioned subprocess contributions | A concrete dynamic task-authoring or stronger isolation requirement justifies a new boundary |
| Output | Durable facts/checkpoints plus provisional incremental presentation | P2 cannot provide coherent recovery/reconnect within acceptable cost |
| TUI | Inline-first conversation, responsive group/worker views, explicit control | P4 usability and PTY evidence favors a different renderer/layout |
| Swarm policy | Optional, bounded, measurable; no required coordinator workflow | Controlled tasks show a better default at the same budget or deadline |

## 5. Remaining focused research

The core draft does not depend on an exhaustive survey. Before a corresponding implementation boundary is stabilized, inspect its specific competing implementation and tests:

- P1: Goose's underlying goose-agent machine and Codex's capacity/residency, wait, and cancellation implementations beyond the spawn excerpt.
- P2: Pico's output/watch/storage specification and relevant legacy regression tests; current SQLite query plans and fork representations.
- P3: DSH's scoped resource disposal and failure paths, Codex's permission intersection, and concrete OS sandbox boundaries.
- P4: Actual Pi/Oh My Pi/Grok event and rendering code, not just their product guides.
- Provider/environment milestones: Kimi's Wire/KAOS and OpenHands' conversation/workspace boundaries; exact ACP schema and capability negotiation.
- Effectiveness milestones: editing representations, tool-result shaping, deferred discovery, and programmatic execution, including Oh My Pi and Prime Agent where reproducible evidence is available.

These are narrow follow-through tasks attached to gates, not authorization for an indefinite new framework survey. Additional agents earn inclusion by answering an unresolved question. Neither rewrite history nor popularity is sufficient to dismiss or select a design.

## References

### R1 — Pi/Pico foundation

Publisher: earendil-works/pi. Revision: `7a2647f32a11864d0c2f98bd2278d18fdf524f9a`, pico, inspected September 11, 2026.

- [Current implementation specification](https://github.com/earendil-works/pi/blob/7a2647f32a11864d0c2f98bd2278d18fdf524f9a/packages/agent/docs/pico/pico-simple-handoff.md): entries/context, scoped state, tasks, transactions, admission, cancellation, outputs, watches, storage, and race requirements.
- [Implementation status and authority](https://github.com/earendil-works/pi/blob/7a2647f32a11864d0c2f98bd2278d18fdf524f9a/packages/agent/docs/pico/pico-simple-blockers.md): WP1 and gated interfaces.
- [Task declarations](https://github.com/earendil-works/pi/blob/7a2647f32a11864d0c2f98bd2278d18fdf524f9a/packages/agent/src/harness/pico/tasks.ts): typed input/checkpoint/outcome contract. Most runtime claims in this review are specifications, not tested implementations.

### R2 — Ordinary Pi and earlier AgentHarness

Publisher: earendil-works/pi. Revision: `71dca871bc80b6bc97be37f0ca3189399d651fff`, inspected September 11, 2026.

- [Coding-agent SDK construction](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/coding-agent/src/core/sdk.ts): Agent and AgentSession path.
- [Experimental mini](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/coding-agent/src/experimental/mini/README.md): experimental client/host/worker topology and declared omissions.
- [Earlier harness specification](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/agent/docs/harness.md): lanes, operations, durability, recovery, and invariant catalog. Some implementation-status text is historical; verify a specific claim against source before porting it.

### R3 — DSH/Cordis and teams

Publisher: deepseek-ai/deepseek-harness. Revision: `c291e7961a515f6d7af9304e7fd1d257929aef26`, inspected September 11, 2026.

- [Architecture](https://github.com/deepseek-ai/deepseek-harness/blob/c291e7961a515f6d7af9304e7fd1d257929aef26/docs/architecture.md): composition, profiles, events, and ownership.
- [Agent teams](https://github.com/deepseek-ai/deepseek-harness/blob/c291e7961a515f6d7af9304e7fd1d257929aef26/docs/subsystems/agent-team.md): experimental identity, mailbox, assignment revisions, and advisory write scopes.

### R4 — Codex agent identity and spawn

Publisher: OpenAI. Inspected source revision: `89c8bcf37d64be69e4c8286f4541c1a84ed312a4`; not claimed to be a release or the latest branch head.

- [Agent spawn and restoration](https://github.com/openai/codex/blob/89c8bcf37d64be69e4c8286f4541c1a84ed312a4/codex-rs/core/src/agent/control/spawn.rs): restore_v2_agent_metadata and keep_forked_rollout_item. The inspected functions separate identity residency and inherited context/accounting; broader lifecycle conclusions require adjacent source/tests.

### R5 — Goose's alternative execution design

Publisher: aaif-goose/goose. Revision: `50666ae0b9a51e260b52b7efbab2e4e020346e94`, inspected September 11, 2026.

- [State-machine module](https://github.com/aaif-goose/goose/blob/50666ae0b9a51e260b52b7efbab2e4e020346e94/crates/goose/src/agents/state_machine/mod.rs): ordered re-entrant pipeline, exported operations, and opt-in switch.
- [Migration instructions](https://github.com/aaif-goose/goose/blob/50666ae0b9a51e260b52b7efbab2e4e020346e94/AGENTS.md): old/new path coexistence. This is a candidate to test, not a claim that its recovery semantics match Pico.

### R6 — Grok Build subagents and workspaces

Publisher: xai-org/grok-build. Revision: `37949780c144e37df692e3d669051a21fec24f20`, inspected September 11, 2026.

- [Subagents and personas](https://github.com/xai-org/grok-build/blob/37949780c144e37df692e3d669051a21fec24f20/crates/codegen/xai-grok-pager/docs/user-guide/16-subagents.md): background, resume, messaging, inherited MCP, and worktree application. These are documented product contracts; no Grok runtime was executed in this review.

### R7 — Oh My Pi Agent Hub

Publisher: can1357/oh-my-pi. Revision: `f97fa5c95010b62ac34c7357f9a1cae6975e12d6`, inspected September 11, 2026.

- [Agent Hub](https://github.com/can1357/oh-my-pi/blob/f97fa5c95010b62ac34c7357f9a1cae6975e12d6/docs/agent-hub.md): responsive roster/inspector, steering, persisted workers, and focus-triggered revival. Product guide, not a TUI benchmark.

### R8 — OpenAI Agents API

Publisher: OpenAI. Announcement dated September 10, 2026; official documentation inspected September 11, 2026. Web documentation is not commit-pinned.

- [Introducing the Agents API](https://openai.com/index/introducing-the-agents-api/): managed Codex harness, context management, tool discovery, programmatic calling, and optional multi-agent operation.
- [Architecture](https://developers.openai.com/api/docs/guides/agents-api/architecture): harness, environment, and application-server boundaries. Architectural evidence, not a dependency proposal or an independent performance comparison.

### R9 — Tokio cancellation and blocking work

Publisher: Tokio maintainers. Documentation for Tokio 1.53.1 inspected September 11, 2026; this does not select Ion's dependency version.

- [spawn_blocking](https://docs.rs/tokio/1.53.1/tokio/task/fn.spawn_blocking.html): running blocking tasks are not generally abortable; timeout limits waiting, not execution.

### R10 — SQLite durability and WAL

Publisher: SQLite project. Official living documentation inspected September 11, 2026; exact SQLite version must be selected and verified with the implementation.

- [PRAGMA synchronous](https://www.sqlite.org/pragma.html#pragma_synchronous): synchronization modes and WAL durability.
- [Write-ahead logging](https://www.sqlite.org/wal.html): WAL lifecycle, checkpoints, file handling, and local-host constraints. Read the current release/advisory information when selecting the library; this review does not certify a version.
