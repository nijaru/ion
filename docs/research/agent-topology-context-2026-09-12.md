# Agent topology and worker-context review — 2026-09-12

Scope: Ion core agent/session topology only. This review deliberately excludes long-term knowledge/memory stores and shared task-board systems.

## Conclusion

The leading Ion design should treat a **session as the durable team/consistency domain** and a **conversation as the durable agent thread**. Root and worker conversations use the same durable schema and task system. A worker normally remains inside its parent's top-level session even when it starts with completely fresh model context.

Fresh versus inherited context is therefore **not** a storage/session-isolation choice. It is a conversation-history choice.

The separate durable `Agent` + `Conversation` split should be removed unless implementation uncovers a concrete requirement for one persistent agent identity to own several simultaneously distinct conversations. No such requirement is currently established: follow-ups can continue the same conversation, reset/handoff can start a clean model context without losing durable history, and a true branch is naturally another conversation.

Do not claim this general session-with-many-conversations model as novel. Current Pico is explicitly converging on it, and current Codex multi-agent work also identifies spawned agents by thread. Ion can still differ materially in Rust APIs, per-session persistence, typed task execution, recovery/effect uncertainty, authority and TUI control.

## Current Pico direction

Primary source at review time:

- `earendil-works/pi` `pico` branch commit `7a2647f32a11864d0c2f98bd2278d18fdf524f9a`.
- `packages/agent/docs/pico/pico-simple-handoff.md` states that it is the sole normative clean-room Pico implementation specification; older Pico documents are historical inputs.

The current Pico core is intentionally small:

```text
Session
  +-- Conversations
  |     +-- immutable Entries
  |     +-- durable Tasks
  |     +-- scoped State
  +-- one serialized commit line
```

Execution is concurrent; mutations serialize through one session owner. There is no lane abstraction in the normative core.

### Fork provenance and execution ownership are different edges

Pico stores a conversation roughly as:

```text
Conversation
  id
  parent? = { conversationId, at }  # historical source/fork cutoff
  owner?  = taskId                  # execution ownership
```

A fork sets `parent`; it does not inherit source tasks and it does not create execution ownership. Later changes in the source are invisible.

An owned child sets `owner`; the creating task and child ownership are committed together. An owned child may also have a `parent`, so a child can inherit history while remaining an independently owned execution thread.

This separation is important. History ancestry must not decide cancellation, serving or permissions.

### A subagent is an owned conversation

The normative Pico specification says a subagent is an owned conversation, never a separate subagent task. Its creation is an atomic parent-tool transaction that can:

- create the child conversation;
- optionally choose a history parent/cutoff;
- copy explicitly selected initial values;
- accept the child's first input;
- create the child's first generation;
- store the child/input identity in the parent tool's checkpoint.

The parent tool may wait for the child or may complete while the child continues. That difference is lifecycle/dependency policy, not a different durable agent type.

Older Pico v3/usage material makes the product shape explicit with `run`, `spawn`, `send`, `status`, `wait` and `stop`. The current normative spec gates the exact ordinary-tool API but retains the same ownership/admission/wait foundation.

## Current Codex evidence

Current multi-agent V2 source inspected around commit `8d3c6cc13d41127faa25eebeac00c48410dfe5c5` (2026-09-12) provides useful production evidence:

- spawned agent identity is a `ThreadId`;
- root and child agents use the same underlying thread/runtime machinery;
- children may recursively spawn children;
- the root has explicit `spawn_agent`, `followup_task`, `send_message`, `wait_agent`, `interrupt_agent` and `list_agents` controls;
- spawn supports `fork_turns = none | all | N`, showing that context inheritance is independently selectable from child identity;
- the tool guidance says to delegate concrete bounded subtasks that can run independently, otherwise continue locally;
- current V2 workers share a filesystem/container, while context can remain separate.

Codex defaults full-history fork in its current tool surface. That is production behavior, not evidence that full inheritance is the optimal Ion default.

## External effectiveness evidence

### Multi-agent only helps when work decomposes

Google Research, *Towards a science of scaling agent systems: When and why agent systems work* (2026-01-28):

- evaluated 180 configurations across single, independent, centralized, decentralized and hybrid architectures;
- centralized orchestration improved a parallelizable task substantially (+80.9% on the reported Finance-Agent comparison);
- every multi-agent variant degraded a sequential planning benchmark by 39–70%;
- independent agents amplified errors much more strongly than centralized orchestration;
- tool-heavy workloads incur a growing coordination tax.

Source: https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/

Implication: Ion should favor one root/synchronizer with bounded delegated work, not peer swarms or automatic fan-out.

### Coding-agent cooperation is currently fragile

CooperBench (2026) reports that, at equal combined workload, two cooperating coding agents often underperform one agent. Its public results report roughly 25% success for GPT-5 and Claude Sonnet 4.5 in the tested two-agent cooperative setting, about 50% below their solo result, with failures attributed to expectation, communication and commitment gaps.

Sources:

- https://cooperbench.com/
- https://github.com/cooperbench/CooperBench

Implication: multi-agent should be an optional decomposition mechanism with explicit ownership/result flow. More agents are not an objective.

### Isolated context is often a benefit, not a deficiency

Anthropic's current prompting guidance recommends subagents for parallel work, isolated context and independent workstreams, and recommends direct work for simple/sequential tasks or tasks that need context maintained across steps. It also warns that newer models can overuse subagents.

Source: https://docs.anthropic.com/en/docs/build-with-claude/prompt-engineering/prompt-templates-and-variables

Microsoft's 2026 *Less Context, Better Agents* experiment is not a coding-subagent benchmark, but it supplies useful context evidence: selective pruning plus compact summarization outperformed full-history retention in its long-horizon tool-use task. Full context was not automatically better context.

Source: https://arxiv.org/abs/2606.10209

Implication: do not equate history inheritance with intelligence. Inherited history can carry useful constraints, but it can also carry stale tool output, anchoring, token cost and correlated mistakes.

## Ion topology recommendation

### One durable entity for an agent thread

Prefer:

```text
Session
  primaryConversationId

Conversation  # also the durable identity presented as an agent/worker
  id
  historyParent?       # source conversation + cutoff
  ownerTask?           # execution/control provenance
  lifecycle metadata   # only if needed
```

with:

```text
Entry    -> conversationId
Input    -> conversationId
Task     -> conversationId
Effect   -> taskId
```

Do not add separate durable `AgentId -> current ConversationId` state unless a real use case requires one stable participant to own multiple concurrent conversations.

The user-facing API may still expose an `Agent`/`Worker` handle for clarity. It should wrap the same durable conversation/thread identity rather than introduce a second canonical object.

Root and workers are the same type. The root is distinguished because `session.primaryConversationId` points to it and it has no owner. Researcher/reviewer/implementer are configurations/instructions, not subclasses or scheduler kinds.

### Keep independent relationships independent

There is no single authoritative "agent tree". The core has several graphs:

```text
history ancestry:     Conversation --parent/cutoff--> Conversation
execution ownership: Task --owns--> Conversation
execution ordering:  Task --depends-on--> Task      (DAG)
workspace relation:  Conversation/Task --binds--> Workspace
message flow:         Input(sender,target)
```

A child may have both a history parent and an owner, but those edges mean different things.

Lanes should disappear. A UI that wants a lane-shaped view derives it from one conversation's current tasks/output; it is not durable execution topology.

## Fresh versus inherited workers

### Fresh child inside the same session

```text
parent conversation
       |
       | owner/control edge
       v
child conversation          historyParent = none
```

The child receives:

- its explicit task prompt;
- selected configuration (model, tool loadout, permissions ceiling, etc.);
- its workspace binding;
- no parent transcript unless explicitly provided in the task input.

Prefer fresh context when:

- the subtask can be described self-contained;
- the worker is researching/exploring a repo;
- an independent reviewer/tester perspective is valuable;
- parallel modules have clear boundaries;
- inherited reasoning would mostly add stale tool output or anchoring;
- the filesystem/source of truth contains what the worker needs.

Benefits:

- lower context/token cost;
- cleaner attention;
- less correlated reasoning/anchoring;
- stronger independence for review;
- parent can send only the constraints that matter.

Costs:

- may repeat repository exploration;
- can miss tacit requirements that were never made explicit;
- parent task prompt/result handoff quality matters more.

### Inherited/forked child inside the same session

```text
parent conversation --history at P--> child conversation
        |
        +-------- owner/control -------->
```

Prefer inherited context when:

- the child must understand nuanced user requirements or prior decisions that are expensive/risky to restate;
- it is continuing the same investigation/debugging thread;
- two alternatives should start from exactly the same historical model state;
- a follow-up genuinely depends on the preceding conversation rather than only repo state.

Benefits:

- less handoff loss;
- exact historical provenance;
- cheap logical branching because immutable source entries can be referenced rather than copied;
- good for alternate solutions or continuation from a known point.

Costs:

- token cost and context pollution;
- stale tool/file state can mislead;
- correlated assumptions reduce independent review value;
- long/full inheritance can be worse than a concise fresh handoff.

For worker spawning, inherited context should resolve to a safe complete exchange boundary. Do not launch a child with half of a live tool exchange merely because the storage model can represent the historical cutoff.

### Reuse an existing worker

Reuse the same worker conversation when the new task strongly depends on what that worker already learned. This avoids repeatedly forking or reconstructing the specialist's context.

Use a context reset/handoff on that same conversation if its history has become noisy but retaining the stable worker identity/control relationship is useful.

### New top-level session

Create a separate session only when the consistency/lifecycle boundary is actually independent, for example:

- a separate user goal that should be independently archived/deleted/backed up;
- a different security/credential/host ownership domain;
- a truly independent workspace/project with no need for atomic group coordination;
- a future remote/multi-host ownership boundary.

Do **not** create a separate top-level session merely to give a worker clean model context. A fresh child conversation already provides that isolation without losing transactional coordination.

## Worker scheduling policy

The root should be the default synchronizer and validation bottleneck.

Delegate when a concrete subtask is both useful and sufficiently decomposable. Avoid delegation for tightly sequential reasoning, trivial tool calls, single-file changes that one agent can complete directly, or tasks where coordination overhead exceeds parallel value.

Useful initial worker patterns:

| Pattern | Context | Lifetime/workspace |
|---|---|---|
| Repository explorer | fresh | read-only/shared workspace |
| Independent reviewer | fresh | read-only or immutable diff/snapshot |
| Test/failure investigator | fresh unless prior diagnostic context is essential | usually shared read access |
| Independent implementation slice | usually fresh | isolated worktree/snapshot |
| Continuation/debug specialist | inherit or reuse existing worker | depends on mutation policy |
| Alternative solution from same decision point | fork at explicit boundary | isolated worktree if mutating |

Foreground versus background is separate from context:

- **joined/foreground delegation:** the parent task cannot finish until the child's result is terminal;
- **retained/background spawn:** creation returns the child ID and the parent continues; the child remains addressable;
- a later `wait` creates durable dependency/continuation state rather than consuming an execution slot while waiting.

The exact model-facing tool surface can remain small: spawn/run, send/follow-up, inspect/status, wait, interrupt/cancel, retire. These are commands over the same conversation/task runtime, not a separate swarm subsystem.

## Shared session is not shared model context

This is the central design point.

All cooperating threads can share one canonical session database while retaining completely independent model contexts:

```text
                       one session.sqlite
                             |
         +-------------------+-------------------+
         |                   |                   |
     root thread         worker A            worker B
     own context         own context          own context
         |                   |                   |
         +------ explicit messages/results ------+
```

Do not automatically expose every worker transcript to every other model. The session stores truth and provenance; model context remains deliberately selected.

This gives Ion many of the benefits people reach for a "knowledge store" to obtain inside one collaborative run:

- durable worker findings/results;
- inspectable provenance;
- addressable prior workers;
- explicit messages and follow-ups;
- searchable/queryable transcripts if later needed;
- one consistent task/effect/resource graph.

It does so without extracting possibly stale facts into a second semantic authority.

This does not prove that cross-session knowledge is never useful. It changes the burden of proof: first test whether durable resumable sessions, context reset/handoff, worker result summaries and repository state already solve the continuity problem. Only add a separate knowledge system if a measured cross-session task still benefits enough to justify freshness/trust/retrieval complexity.

## Initial default recommendation

For the first Ion multi-agent baseline:

1. One top-level session owns the root and all cooperating worker conversations.
2. Root and workers share one durable conversation schema and task engine.
3. No separate durable Agent row initially.
4. Fresh child context should be the conservative default for independent delegation; inheritance is explicit when the task is context-dependent.
5. Reuse an existing worker for closely related follow-ups rather than spawning repeatedly.
6. The root integrates/validates worker results; peer coordination is not the default architecture.
7. Parallel mutating workers use isolated workspaces/worktrees; read-only workers may share the source workspace.
8. No knowledge/memory/task-board store is part of the core.
9. Evaluate fresh versus inherited context, worker count and delegation policy on coding tasks before freezing model-facing defaults.

The design remains reopenable. If implementation reveals a real need for stable agent identity separate from conversation identity, add it with that use case and invariant spelled out rather than preserving the distinction preemptively.