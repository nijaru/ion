# Ion architecture v2

**Status:** normative target for `refactor/architecture-normalization`. It supersedes conflicting assumptions in the current root `DESIGN.md`; that document must be reconciled before this branch merges.

**Target:** an idiomatic Rust implementation of the strongest current Pi 2 harness ideas, improved where independent evidence or Rust ownership/storage semantics justify it.

## 1. Reference weighting

External projects are evidence, not compatibility contracts, and they are not weighted equally.

### Pi 2 — primary architectural and practical alpha

Pi is the primary reference for the harness itself. Its current v2 work provides the leading model for:

- append-only parent-linked conversation history;
- named lanes as active cursors into shared passive history;
- one operation per lane, parallel lanes, one session writer;
- forks for isolation and lanes for shared-history concurrency;
- branch/navigation without copying shared prefixes;
- provisioned identities before repeat-sensitive work;
- deterministic child identity for replay-safe subagent creation;
- prompt-cache-friendly tail growth;
- hooks separated from observational events;
- explicit durable recovery semantics.

Pi's currently implemented v2 reconstructs some operation state from execution records and stores model/thinking/tool activation changes in the conversation tree. Pi's newer explicit-state redesign proposes a stronger representation: immutable operation acceptance, total operation/lane state, and separate total lane configuration. Ion adopts that **logical** direction because it makes recovery direct and keeps conversation topology independent from execution configuration.

Ion does not copy Pi's TypeScript APIs, JSONL-driven physical representation, or compatibility requirements.

### DeepSeek Harness / Cordis — strongest independent architecture cross-check

DSH/Cordis independently demonstrates that many agents can share infrastructure while registrations remain agent-scoped and lifecycle-owned. The transferable idea is:

- shared provider/store/tool infrastructure;
- per-agent visibility for tools, prompt contributions, restrictions, listeners, and related model-facing state;
- teardown structurally reverses registrations owned by that scope;
- concurrent agents do not redefine one ambient "current agent" context.

Ion should borrow scoped ownership where concrete features need it, without adopting a universal service locator or an "everything is a plugin" architecture.

### Codex — high-value independent production reference

Codex provides strong evidence for:

- family/root-scoped agent control shared by descendants;
- stable agent registry/path/status concepts;
- concurrency and rollout budgets;
- direct inter-agent messaging/control;
- explicit history-fork choices;
- distinct history lineage and control/spawn lineage;
- centralized sandbox/policy/execution boundaries;
- practical Rust APIs and ownership choices.

Codex is not ranked above Pi as the practical reference; it is valuable because it is an independent production implementation with different trade-offs.

### Grok Build — useful Rust and multi-agent implementation evidence

Grok Build is useful for concrete mechanics:

- independent child sessions with their own context;
- foreground/background as a wait/observation choice;
- resumable completed children;
- optional git-worktree isolation independent of conversation isolation;
- typed agent/persona/capability configuration;
- bounded concurrency and event-driven waits;
- background completion/status plumbing;
- substantial Rust module/API organization.

Its specific depth/recursion policy is a product choice, not an Ion core invariant.

### Prime Agent — secondary pressure test

Prime Agent is useful mainly because it pushes a Pi-derived system toward recursive and long-running behavior: recursive children, daemon continuity, direct messaging, goals, heartbeats, schedules, autonomous continuation, and a persistent programmatic control environment.

These are useful pressure tests for Ion's primitives. Prime's benchmark/product claims and higher-level orchestration decisions are not strong evidence by themselves.

Gemini CLI is not an architectural reference for Ion. OpenCode is low-weight evidence because repeated large rewrites make architectural convergence difficult to infer.

## 2. North star

Ion is a local-first, provider-neutral coding harness whose ordinary model loop remains deliberately small while the durable substrate supports richer execution topology.

The substrate should support without another persistence/runtime rewrite:

- durable interactive sessions;
- branch/navigation/rewind;
- concurrent lanes over shared history;
- isolated or fresh subagents;
- recursive and background agents;
- direct agent-to-agent messaging;
- worktree-isolated coding agents;
- bounded multi-agent/supervisor/swarm strategies;
- detached/daemon execution and scheduled wakeups;
- local, ACP, or other external-agent backends;
- measured harness-policy experimentation.

Higher-level orchestration is composition over these primitives, not privileged state in the normal agent loop.

## 3. Durable session model

```text
Session
├── conversation Tree
├── Lanes
│   ├── leaf
│   ├── LaneState
│   ├── LaneConfig
│   └── at most one open Operation
├── Operations
├── Effects
├── usage ledger
└── global/session facts
```

One loaded session has one mutation authority. Provider, tool, process, and agent effects run outside that mutation line and may run concurrently.

### 3.1 Conversation tree

Conversation entries are passive semantic history. Every entry has:

- a stable application-visible ID provisioned before persistence;
- an immutable parent ID or the virtual root;
- semantic payload;
- storage-assigned ordering/timestamp metadata.

The tree only grows. Branches share prefixes by reference; they do not copy history inside one session.

The tree must not contain lane pointers, queues, operation state, effect bookkeeping, usage accounting, agent-control state, or execution configuration merely because those values are durable.

Storage sequence numbers order commits and support queries. They are not semantic entry identity.

### 3.2 Lanes

A lane is a durable named active cursor into the conversation tree plus work serialized at that cursor.

Every session has `main`. Initial terminal UX may expose only `main`; subagent or branch features may create more lanes without changing normal CLI behavior.

A lane owns:

- its leaf;
- total lane state;
- total lane configuration;
- at most one open operation;
- lane-owned queued input that exists independently of an operation.

Different lanes may execute concurrently. They may point at the same leaf and diverge on their next append.

### 3.3 Lane state

`LaneState` is a **total latest-value** durable value. It must not be reconstructed by folding queue/history records.

The initial shape needs at least:

```text
leaf
current operation id
pending next-run input
```

Additional lane-owned state enters this value only when it truly exists independently of one operation.

### 3.4 Lane configuration

`LaneConfig` is separate from conversation history and is also total latest-value state.

Initial configuration should cover the model-facing selections needed for future generations, such as:

```text
model/provider selection
reasoning/thinking selection
active capability/tool identities
```

A branch/navigation operation moves conversation position; it does not implicitly restore historical execution configuration. If callers want configuration copied/restored, that is an explicit operation.

Each generation snapshots the exact effective configuration it actually uses. Later lane configuration changes affect later generations, not an in-flight or retrying generation whose configuration was already captured.

This follows the stronger direction in Pi's explicit-state redesign and aligns with the broader separation of semantic history from agent-scoped execution configuration.

## 4. Operations

A lane has at most one open operation.

Each operation consists of:

1. immutable acceptance data;
2. one latest **total** mutable operation state, optionally with prior revisions retained for audit/debugging.

Conceptually:

```text
Operation
  id
  lane
  source leaf
  intent

OperationState
  control
  phase
  operation-owned inbox/queued writes
  pending/settled effect plan
  complete continuation state
```

Total means one latest state value plus immutable acceptance is sufficient to determine what may happen next. Recovery must not collect partial task records or infer a hidden program counter from absence/presence combinations.

Historical revisions may be retained for audit/debugging, but no previous state revision may be required to fill in current state.

Cancellation is orthogonal control over active workflow state, not a fake workflow phase.

## 5. Atomic transition rule

Every durable boundary obeys one rule:

> Compute the next total authoritative state, then atomically commit every durable mutation required to make that state true.

A transaction may include:

- conversation entries;
- lane leaf/state/config changes;
- operation state;
- effect intents or settlements;
- durable input consumption/admission;
- usage;
- facts/labels;
- reconciliation evidence.

Either all logical mutations commit or none do. Failed persistence never installs live success.

Slow provider/tool/process/agent work never runs on the session mutation line.

## 6. Storage representation

Logical architecture must not inherit a physical format merely because Pi supports JSONL.

Ion uses SQLite today. It may represent total current state using append-only revisions plus current projections/indexes, or another indexed form, provided:

- one read yields the complete current lane/operation state;
- immutable acceptance/history remains immutable;
- transitions are atomic;
- crash recovery does not fold a delta chain;
- ownership/fencing remains explicit.

Measure storage cost before weakening total-state semantics. Physical compression or structural sharing may be added later without changing the logical contract.

Ion is v0 and may continue archiving incompatible older development schemas rather than carrying speculative migrations.

## 7. Effects and recovery

Repeat-sensitive external work is admitted durably before it starts.

External effects include model generations, tools, remote/local child creation, mutable hooks/interceptors, and any scheduled/retry action where duplicate firing matters.

There is no general exactly-once claim. An admitted invocation declares/owns appropriate recovery semantics:

- safe replay;
- reconciliation/idempotent reattachment;
- never replay / visible indeterminate outcome.

Typed runtime code operates on typed effect/invocation values. A storage codec may still encode a discriminant plus JSON.

### 7.1 Child creation

Child/agent identity is preallocated or deterministically derived from the durable parent invocation before the external creation effect starts. Recovery reattaches/reconciles instead of spawning a twin.

### 7.2 Model steps

Each generation persists/snapshots the exact effective input needed for reproducibility and recovery:

- model/provider identity and relevant capabilities;
- projected conversation context;
- effective capability/tool identities;
- trusted/model-facing context contributions;
- harness/profile identity;
- relevant hook/interceptor output;
- retry/cache expectations when they affect behavior.

The current small `ion/default@1` profile identity is sufficient until structured profile configuration actually exists. Do not invent a synthetic fingerprint that merely hashes a hard-coded label.

## 8. Agent topology

A subagent is an addressable agent using common session primitives, not a second runtime architecture.

History choices are orthogonal:

- **lane** — another lane in the same session, anchored at a committed entry;
- **fork** — another durable session seeded from a source branch/tree boundary;
- **fresh** — another durable session without inherited conversation history;
- **external** — an ACP/other backend with its own history semantics.

Workspace choices are separate:

- shared working directory;
- isolated git worktree;
- future sandbox/remote workspace.

Foreground/background is a wait/observation policy, not an agent kind.

Control/spawn lineage and history lineage are distinct. A fresh child can have a control parent without a history source; a user fork can have a history source without a control parent; a fork-context child can have both.

## 9. Family-scoped agent control

A root agent/session family needs a control plane above individual lane/session execution.

Responsibilities include:

- stable agent identity/path/name;
- live/retained registry;
- lane/fork/fresh/external spawn admission;
- direct messaging;
- observe/wait/status;
- cancel/interrupt/resume;
- descendant accounting;
- concurrency/depth/token/time budgets;
- routing background completions;
- deterministic recovery of admitted children.

This control plane is family/root-scoped rather than one process-global mutable agent registry.

The existing special-purpose `ChildManager` and test-only synchronous delegation path are migration scaffolding, not target architecture.

A swarm, reviewer, supervisor/worker, manager/executor/auditor, or other strategy is ordinary code using this control API.

## 10. Durable inputs and background wakeups

User prompts, steering/follow-ups, agent messages, background completions, schedules, heartbeats, and external events should enter through one durable input mechanism with explicit delivery semantics.

Useful delivery modes include:

- steer current work;
- follow up after current continuation;
- enqueue next run;
- notify without implicitly starting model work.

Background tasks never mutate a parent's model context directly from arbitrary Tokio tasks. They submit through the session writer.

## 11. Model-facing composition and scopes

Shared infrastructure may expose model-facing contributions whose visibility/lifetime belongs to one agent or lane scope.

Concrete scoped contribution types may include:

- tools/capabilities;
- prompt/context sections;
- commands/skills;
- behavior-changing hooks/interceptors;
- observers/events;
- owned tasks/resources.

Dropping a scope reverses its registrations and drains owned work.

Use DSH/Cordis as evidence for the ownership property, not for a universal service graph. Ion should keep typed, explicit Rust boundaries.

Behavior-changing hooks and observation/events are separate APIs.

## 12. Tool boundary

Session code must not know that a tool named `write`, `edit`, `bash`, `spawn_agent`, or another public name has special canonicalization/recovery semantics.

The tool boundary should resolve an exact call into the information required for admission:

```text
model call
  -> resolve tool
  -> validate/prepare exact invocation
  -> canonical policy target
  -> recovery/reconciliation semantics
  -> persist typed effect intent
  -> execute admitted invocation
  -> persist settlement/evidence
```

Recovery behavior belongs to invocation semantics, not whether a tool is native, MCP, extension-provided, or dynamically scoped.

## 13. Workspace and verification

Conversation topology and workspace topology are independent.

Git worktrees are a strong option for concurrent mutating agents and unnecessary overhead for many read-only/shared-work agents.

A global `WorkspaceGeneration` cannot prove workspace freshness because editors, users, git, hooks, build tools, and other processes can mutate the workspace outside Ion.

Correctness-sensitive verification binds to concrete observations such as:

- relevant file/content hashes;
- git HEAD/index/worktree state;
- command/test identity and exit status;
- output/artifacts;
- relevant paths/environment inputs.

An Ion-owned generation may exist only as cache/optimization metadata.

## 14. Long-running and autonomous features

Goals, schedules, heartbeats, daemon retention, autonomous continuation, reviewers, and swarm/supervisor policies should primarily compose over durable inputs and agent control.

Do not add a new ordinary operation phase merely because a long-running policy exists outside the operation.

Prime Agent is useful as a pressure test that the primitives remain composable under long-running/recursive use, not as the source of the durable model.

A persistent programmatic RLM/REPL environment is an evaluation target, not a required architecture primitive.

## 15. Runtime/process topology

The durable model is independent of process placement.

A practical local shape is:

```text
Host / Runtime
├── provider/model infrastructure
├── durable store
├── workspace/process infrastructure
├── extension/MCP infrastructure
└── loaded sessions
    ├── SessionTask A
    └── SessionTask B
```

Each `SessionTask` owns one loaded session's mutation authority. Lanes inside it share that authority while slow lane effects run concurrently. Forked/fresh agent sessions may use another session task.

A later daemon may place root/session families in worker processes without changing the durable schema/API.

## 16. Rust API and module discipline

Ion should improve on TypeScript reference systems rather than transliterate their framework vocabulary.

Rules:

- modules provide namespace/context so type names stay short;
- store rows, codecs, task internals, and transition machinery remain `pub(crate)` unless callers truly require them;
- enums represent closed states rather than loose boolean combinations;
- one authoritative session owner, bounded channels, structured task ownership;
- prefer ordinary `new`, `open`, `create`, `spawn`, `fork`, `send`, `wait`, `cancel`, `close` APIs;
- use `Manager`, `Service`, `Controller`, `Factory`, `Handle`, `Config`, and `Record` only when each word expresses a real lifetime/domain distinction;
- builders exist only when staged optional construction materially improves use;
- split modules/crates at ownership/cohesion boundaries, never line-count thresholds;
- do not flatten implementation types into `ion_core::*` re-exports;
- do not land unused future types merely to sketch the architecture.

Current domain direction:

```text
ion-core/src/
  runtime/      # host/session-task composition while migration proceeds
  session/      # tree + lane topology
  operation/    # pure operation reducer/state
  effect/       # typed durable external-effect vocabulary
  agent/        # family control once it owns real behavior
  model/        # provider/model/context-step vocabulary as it is extracted
  tool/         # tool contract/catalog/preparation/native tools
  workspace/    # execution/worktree abstraction when introduced
  store/        # SQLite persistence and codecs
  extension/
  mcp/
  policy/
  process/
```

The move of the old `session.rs` reducer to `operation/mod.rs` is correct: that code describes operation execution, not session topology.

## 17. Implementation sequence

1. Keep the current green architecture-normalization checkpoint.
2. Make semantic conversation-entry identity application-visible and provisioned before persistence.
3. Move entries to immutable parent-linked tree semantics and add durable `main` lane state/config in the same schema slice.
4. Bind operations and durable inputs/effects to a lane; migrate today's linear CLI behavior onto `main` without UX change.
5. Generalize from one open operation per session to one per lane and allow parallel lane effects under the one session writer.
6. Finish typed effect **writes** and remove raw model-step JSON field knowledge from normal storage/runtime code.
7. Replace special-purpose/test-only delegation with family-scoped agent control over lane/fork/fresh agents.
8. Add lifecycle-owned contribution scopes from concrete tool/context/extension use cases.
9. Establish/expand `ion-eval` before speculative orchestration policy.
10. Add long-running/swarm/supervisor strategies only as compositions over the proven substrate.

## 18. Research posture

Another broad framework survey is not a prerequisite to implementation. Research is now question-driven at the slice being built.

High-value unresolved questions:

- exact idiomatic Rust decomposition of total lane/operation state without type soup;
- branch-only versus full-tree fork semantics;
- append-only operation-state revisions versus a current-row SQLite representation plus optional audit history;
- durable agent-message delivery/receipt semantics;
- worktree apply/merge ownership;
- ACP/external-agent adapter boundaries;
- whether a persistent programmatic RLM/REPL capability improves measured Ion evals.

When implementation or evaluation evidence contradicts this document, update the design in the same change. Do not preserve a weaker idea for consistency.
