# Ion architecture v2 — research-backed target

**Status:** Target direction for `refactor/architecture-normalization`; `DESIGN.md` must be reconciled before this branch merges.

**Intent:** Build an idiomatic Rust harness that captures the strongest current ideas from Pi 2 and the broader 2026 coding-agent ecosystem without treating any one implementation as canonical.

## 1. North star

Ion should be a fast, local-first, provider-neutral coding harness with a deliberately small ordinary model loop and a substantially richer substrate underneath it.

The substrate should make the following natural rather than requiring another architectural rewrite:

- durable interactive sessions;
- branching and rewind;
- multiple concurrent lines of work over shared history;
- isolated/forked subagents;
- background agents and direct agent-to-agent messaging;
- worktree-isolated coding agents;
- recursive or multi-agent orchestration under explicit budgets;
- future supervisor/worker, review, swarm, or long-horizon strategies;
- local, remote, ACP, A2A, or other agent backends;
- detached/daemon execution, schedules, heartbeats, goals, and autonomous continuation policies;
- experimentally varied model-facing harness policy without weakening durable correctness.

Higher-level strategies are compositions over the substrate. They are not privileged states in the normal model loop.

## 2. External evidence

Pi 2 remains the strongest architectural reference for durable session semantics:

- append-only parent-linked conversation tree;
- named lanes pointing at tree leaves;
- one open operation per lane, with lanes executing concurrently;
- one writer for the durable session;
- immutable operation acceptance plus explicit total operation state;
- intent before repeat-sensitive effects;
- forks for isolation and lanes for shared-history concurrency;
- deterministic child identity for replay-safe subagent creation;
- model/provider context that grows at the tail to preserve prompt-cache reuse.

Prime Agent is especially useful because it is a real evolution of Pi rather than an unrelated design. It demonstrates that a Pi-derived substrate can grow into:

- recursive subagents;
- retained/background agents;
- direct agent messaging;
- persistent goals, heartbeats, schedules, and autonomous continuation;
- daemon-backed root workers with descendant lifecycles;
- a persistent programmatic control environment;
- continual harness state and evaluation-oriented long-running work.

Grok Build provides strong practical evidence around:

- independent child sessions with their own context windows;
- background subagents and auto-wake/result delivery;
- resumable completed children;
- agent definitions, roles/personas, capability restrictions, and per-child MCP inheritance;
- optional git-worktree isolation as a separate execution concern;
- dedicated subagent admission/coordinator logic, bounded concurrency, and worker isolation;
- schedules/goals and independent evidence review;
- a Rust implementation that separates agent runtime, tools, workspace, and TUI concerns.

Codex contributes useful multi-agent control-plane ideas:

- one root-scoped agent control object shared across a family;
- an agent registry and stable agent paths;
- execution/concurrency limits and rollout budgets;
- explicit spawn/fork history choices;
- direct inter-agent messaging, steering, interrupt, status, and subtree operations.

DeepSeek Harness/Cordis contributes a useful composition lesson:

- shared infrastructure with per-agent registration/lifecycle scopes;
- tools/prompt contributions/background resources owned by the agent scope that exposed them;
- lifecycle cleanup following ownership rather than global mutable activation.

Gemini CLI reinforces:

- separate context loops for subagents;
- per-agent tool/MCP isolation and policy;
- parallel subagents;
- remote-agent interoperability through A2A;
- worktree support for parallel work.

OpenCode reinforces:

- child sessions as ordinary sessions;
- foreground/background promotion as a wait policy rather than a different agent kind;
- resumable task sessions;
- derived child permissions;
- asynchronous completion injected back through the parent's ordinary session path.

None of these systems is the specification. The convergence is the evidence.

## 3. Core durable model

### 3.1 Session

A durable session owns passive shared history plus active lane state:

```text
Session
├── conversation tree
├── lanes
├── global/session facts
├── usage ledger
└── durable operation/effect state
```

Conversation entries are append-only and parent-linked. History never needs to be copied merely to branch inside the same session.

The tree contains semantic/model-visible history only. Execution bookkeeping does not become conversation history.

### 3.2 Lane

A lane is a durable named cursor into the conversation tree plus the work serialized at that cursor.

A lane owns:

- its current leaf;
- at most one open operation;
- queued steer/follow-up/next-run inputs;
- lane-local model-facing configuration;
- lane-local agent/contribution scope when applicable.

Different lanes may execute concurrently. They may share an arbitrary history prefix and diverge without copying that prefix.

`main` is simply the default lane. Interactive Ion may initially hide lanes unless a user or agent feature creates another one.

### 3.3 One writer, parallel effects

One loaded session has one mutation authority. This is the durable ordering boundary, not a requirement that provider/tool work run serially.

```text
provider/tool/agent effect A ─┐
provider/tool/agent effect B ─┼─> Session task -> atomic durable transition
provider/tool/agent effect C ─┘
```

The session task must never await slow provider/tool/subprocess work on its mutation line. It may await the bounded local transaction required to make an accepted transition durable.

Do not introduce one actor per module. Lane executors may become separate tasks if measurement or implementation clarity justifies it, but the durable data model must not depend on that choice.

## 4. Operations and total state

Each lane has at most one open operation.

An operation has immutable acceptance data and an append-only sequence of **total** state revisions. The latest revision is sufficient to determine the legal next transition without folding a collection of partial orchestration records.

Conceptually:

```rust
operation::Operation {
    id,
    lane,
    source_leaf,
    intent,
}

operation::State {
    control,
    phase,
    inbox,
    pending_effects,
    // complete current continuation state
}
```

Exact Rust field names should be chosen during implementation; the important contract is totality.

Cancellation is orthogonal control, not a fake workflow phase. A cancellation request prevents new effects from starting after it wins the lane ordering, while already-started effects still settle durably.

A state revision may reference immutable conversation entries, exact effect records, usage records, and configuration snapshots by ID. It must not require older operation-state revisions to fill in missing current state.

## 5. Effects and recovery

Ion keeps the existing strong rule: repeat-sensitive external work is admitted durably before it starts.

External effects include at least:

- model generations;
- tool invocations;
- agent/session creation or remote-agent requests;
- mutable hooks/interceptors;
- scheduled/retry waits whose duplicate firing matters.

There is no general exactly-once claim. Each admitted invocation carries a recovery policy appropriate to the operation:

- safe replay;
- reconciliation/idempotent reattachment;
- never replay / indeterminate.

Child-agent creation should normally be reconcilable rather than `NeverReplay`. Preallocate or deterministically derive the child identity from the durable parent invocation (for example parent operation + tool call + child index), persist it before spawn, and reattach on recovery.

Typed runtime code must operate on typed effect/invocation values. String `kind` + JSON may remain a storage codec, not an architectural interface.

## 6. Agents are topology over the session primitives

A subagent should not be synonymous with one specific storage or process strategy.

An Ion agent is an addressable execution identity whose local implementation targets either a lane or a session, or whose backend is external.

Useful spawn dimensions are orthogonal:

### History

- **shared lane** — new lane anchored at a committed entry in the current session;
- **fork** — new durable session seeded from a source branch/tree boundary;
- **fresh** — new durable session without inherited conversation history;
- **external** — remote/other-agent backend with its own history semantics.

### Workspace

- shared working directory;
- isolated git worktree;
- future sandbox/remote workspace.

Workspace isolation is not the same thing as conversational isolation.

### Capabilities/configuration

- agent definition/profile;
- model and reasoning settings;
- tool/capability scope;
- MCP/extension inheritance;
- policy/sandbox restrictions;
- budget/depth/concurrency limits.

### Waiting

Foreground/background is a caller wait policy, not an agent type. A running agent may be detached, observed, messaged, awaited, cancelled, or promoted between foreground/background UX states without changing its durable identity.

This model supports both cheap shared-history helpers and strongly isolated coding workers without choosing one globally.

## 7. Agent family control

A root agent/session family needs a control plane above individual lane/session execution.

Responsibilities:

- stable agent identities and paths/names;
- registry of live/retained descendants;
- spawn/fork/fresh/external admission;
- concurrency/depth/token/time budgets;
- direct messaging and receipts;
- status/observe/wait/cancel/interrupt/resume;
- ownership/cancellation tree;
- routing background completions;
- deterministic recovery of admitted children.

This should be root/family-scoped, not a process-global mutable registry.

Do not preserve the existing `ChildManager` abstraction merely because it exists. Rebuild child tools as adapters over the general agent-control substrate.

A future swarm/supervisor is then ordinary code using this API rather than a new runtime.

## 8. Messaging, background work, and wakeups

Agent-to-agent communication should use the same durable input machinery as user/host input.

A message may request semantics such as:

- steer active work;
- follow up after current work;
- enqueue next run;
- deliver informational completion without starting a new run.

Background completion should not mutate a parent's model context from an arbitrary task. It should enqueue a durable parent input/notification through the session writer. Whether that wakes the parent immediately is an explicit policy.

This same mechanism can support:

- subagent completions;
- command/test completion;
- schedules/heartbeats;
- external events;
- future supervisor messages.

## 9. Workspaces and verification

Session/history topology and workspace topology are separate.

A workspace abstraction should own the execution root and, where applicable, VCS/worktree identity. It may later represent a sandbox or remote workspace without changing conversation semantics.

Do not use a single `WorkspaceGeneration` as correctness evidence. Other editors, git, hooks, and processes can mutate the workspace outside Ion.

Verification should bind to concrete observations when they matter:

- file/content hashes;
- relevant paths;
- git HEAD/index/worktree state;
- command identity, exit status, and output/artifacts;
- test/verifier evidence.

An Ion-owned generation counter may still be useful for cache invalidation, but it is only an optimization token.

Worktree isolation should be available to agent spawning, but not mandatory. Independent read-only agents often benefit from sharing the working tree; concurrent mutating agents often benefit from separate worktrees.

## 10. Model-facing composition and scopes

Ion should keep a small ordinary tool/model loop, but model-visible configuration must be agent/lane-scoped rather than globally mutable.

The effective model step should snapshot at least:

- model/provider identity and relevant capabilities;
- projected conversation context;
- effective tool definitions and stable tool identities;
- trusted instruction/context contributions;
- model-facing harness/profile identity;
- any mutable hook result that affects the request.

Once a model request starts, later capability/config changes affect only future requests.

Dynamic contributions should be lifecycle-owned. A scope may contribute narrowly typed things such as:

- tools;
- prompt/context sections;
- commands/skills;
- control hooks/interceptors;
- observers;
- owned tasks/resources.

Dropping/closing a scope reverses its registrations and drains its work. Do not introduce an ambient service locator or a universal untyped plugin context.

Hooks that can change behavior and observers/events that cannot must be separate APIs.

## 11. Tool boundary

The session runtime must not know that a tool named `write`, `edit`, `bash`, `spawn_agent`, or anything else has special semantics.

A resolved tool owns or supplies the information needed to admit and execute an exact invocation:

```text
model call
  -> resolve tool
  -> validate/prepare exact invocation
  -> policy over canonical action
  -> persist typed effect intent
  -> execute exact admitted invocation
  -> persist settlement/reconciliation evidence
```

Use module context to keep Rust type names short. For example, an internal `tool::Invocation` is preferable to a cross-crate `PreparedCanonicalizedDurableToolCall`.

Recovery policy belongs to the tool/invocation semantics, not to whether the tool happened to come from the core set, MCP, an extension, or an agent scope.

## 12. Long-running and autonomous features

The substrate should support Prime/Grok-style long-running features without putting all of them into `operation::State`.

Schedules, heartbeats, persistent goals, autonomous continuation, independent review, and supervisor loops should primarily be policies/services that submit durable inputs and inspect durable evidence.

Examples:

- a schedule claims a due tick and submits a prompt/input;
- a heartbeat periodically submits a steer or follow-up;
- a persistent goal supplies continuation policy across operations;
- an autonomous mode decides whether to enqueue another run under a budget;
- a reviewer/auditor is another agent plus an evidence contract;
- a swarm is a controller that creates/messages/waits on agents.

The normal agent loop remains simple while the runtime substrate is not artificially limited.

A persistent programmatic control environment similar to Prime Agent's RLM/IPython model is worth evaluating as an optional model-facing capability. It is not required to define the durable session model.

## 13. Runtime/process topology

With multiple lanes and forked/fresh agents, a real process-level runtime becomes useful.

Target responsibilities:

```text
Runtime
├── shared provider/model infrastructure
├── durable store
├── workspace/process infrastructure
├── extension/MCP infrastructure
└── loaded sessions
    ├── SessionTask A
    └── SessionTask B
```

A `SessionTask` owns one loaded session's mutation authority. Forked/fresh agent sessions get their own session task. Lanes inside one session share that session writer but run slow effects concurrently.

A future daemon may move loaded sessions into worker processes without changing the durable API. Prime Agent provides evidence that root-session worker isolation is useful for long-running/detached work; Grok provides evidence that subagent worker/runtime isolation is useful for parallelism and fault containment. Neither process layout should be baked into the durable schema.

## 14. Rust API and module discipline

The implementation should be idiomatic Rust rather than a transliteration of TypeScript frameworks.

Rules:

- let modules provide namespace/context instead of repeating prefixes in every type;
- keep persistence rows and task internals `pub(crate)` unless callers truly need them;
- prefer `new`, `open`, `create`, `spawn`, `fork`, `close`, `send`, `wait`, `cancel` over factory/manager/builder vocabulary;
- use a builder only when staged optional construction materially improves the API;
- use `Manager`, `Service`, `Controller`, `Factory`, `Handle`, `Config`, and `Record` only when the word describes a real domain/lifetime distinction;
- encode closed state machines with enums rather than boolean/state-field combinations;
- keep the single-writer authority explicit; do not spread session truth across `Arc<Mutex<_>>` graphs;
- use bounded channels and structured task ownership;
- do not split files or crates by line count alone; split at ownership/cohesion boundaries;
- do not flatten every internal type into `ion_core::*` re-exports.

Likely module domains, subject to implementation feedback:

```text
ion-core/src/
  runtime/
  session/
    tree.rs
    lane.rs
    task.rs
  operation/
  effect/
  agent/
  model/
  tool/
    catalog.rs
    native/
  workspace/
  store/
  extension/
  mcp/
  policy/
  process/
```

Avoid crate proliferation until compile/dependency boundaries justify it. The existing `ion-core`, `ion`, and `ion-terminal` split is sufficient for the next architecture pass.

## 15. What changes in the current Ion design

The current `DESIGN.md` is too restrictive in several places and must be reconciled before merge.

In particular:

- replace the assumption of one active operation per session with one active operation per lane;
- replace linear canonical history with a parent-linked append-only tree;
- make lanes first-class durable state;
- allow both lane-based and isolated/forked agent execution;
- replace special-purpose child management with a general agent-control substrate;
- separate conversational history choice from workspace isolation;
- allow recursive/multi-agent topologies under explicit budgets instead of banning them architecturally;
- remove bans on swarm/supervisor concepts as *compositions* while keeping them out of the core model loop;
- allow model-created topology only through typed, budgeted, policy-controlled agent APIs;
- generalize scoped capability ownership beyond the current tool-only catalog where concrete use cases require it;
- revise workspace-verification language away from an authoritative generation counter;
- keep `HarnessProfile` small until there is structured policy worth hashing/versioning;
- shrink the public Rust API and reorganize modules around the new durable domains.

## 16. Implementation order

Do not continue polishing the current single-operation session shape before introducing the new durable substrate.

Recommended sequence:

1. Reconcile this direction into `DESIGN.md` and remove contradictory non-goals/invariants.
2. Define the new durable schema/types for tree entries, lanes, immutable operations, total operation state, and typed effects.
3. Write migration/round-trip/crash-boundary tests first.
4. Move existing linear-session behavior onto `main` lane so ordinary CLI behavior remains unchanged.
5. Migrate model/tool effect creation and recovery to the new total-state/effect boundary.
6. Fix the tool invocation boundary so the session runtime contains no tool-name semantics.
7. Add lane creation/navigation and parallel-lane execution tests.
8. Replace `ChildManager` with root-scoped agent control over lane/fork/fresh targets; derive child identity before execution.
9. Add agent messaging, background completion, configurable budgets/depth, and worktree isolation.
10. Generalize contribution scopes only as needed by agent/MCP/extension isolation.
11. Build `ion-eval` baselines and use measurements to tune context projection, compaction, tool presentation, retrieval, delegation, and optional orchestration.
12. Add daemon/background/schedule/goal/swarm-style layers only on top of the stable primitives.
13. Perform a separate Rust code-hygiene/API pass after the architecture has stopped moving.

## 17. Remaining research questions

The design is broad enough to proceed, but these should be answered while implementing the relevant slice rather than through another open-ended research phase:

- whether lane configuration should be explicit lane records (Pi v2 direction) or derived from semantic configuration entries on the lane path;
- whether forks copy a branch only or may copy the full tree, and how much session-global metadata follows each mode;
- the exact total `operation::State` decomposition that is easiest to make exhaustive in Rust;
- whether immutable effect records remain separately normalized in SQLite or are embedded in operation-state revisions while keeping the same typed runtime contract;
- whether programmatic RLM/REPL control materially improves Ion's benchmark performance versus direct tools;
- which agent messaging receipt/delivery semantics are worth making durable in v0;
- when worktree creation/apply should be core workspace functionality versus a host/tool capability;
- which remote-agent protocol(s) ACP/A2A/custom should be first-class adapters.

These are implementation choices, not reasons to narrow the architecture prematurely.
