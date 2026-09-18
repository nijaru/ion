# Ion architecture

Accepted contract, 2026-09-15; materially refined 2026-09-18 after the current
Pi/Pico and Codex review. [README.md](README.md) states what the current source
implements; this file states the contracts the maintained engine must satisfy. Ion is
unreleased v0: replace obsolete abstractions directly rather than preserving them
through compatibility layers.

The coding Turn remains the continuation owner, but the current Rust predates several
accepted 2026-09-18 boundaries: a stable TurnEnvironment, frozen provider/tool bindings,
versioned request manifests, first-class effect admission, logical tool invocations with
immutable physical attempts, external evidence separate from transcript settlement,
typed drive exits and exact-commit observations. Complete that targeted cutover before
real providers or native tools become acceptance dependencies. An opt-in workspace
mutation coordinator exists today; approval, revocation, structured reconciliation and
confinement do not. Context compaction/reset/forks, artifact publication, a client binary
and workers also remain future work.

## Product and boundaries

Ion is a local, provider-neutral Rust terminal coding agent with an equally capable
headless/library interface. One primary conversation is the default; cooperating
workers are optional. macOS and Linux are the initial execution targets. No cloud
account, daemon or telemetry service is required; local models are ordinary providers.

The engine runs a coding turn, not arbitrary workflows. It does not mandate a
planner, memory system, task board, gateway, schedule service or swarm policy.
Other applications may host the same engine without importing the TUI.

- `ion-ai` owns model requests, ordered content, stream protocol, usage and typed
  provider facts. It knows nothing about sessions, storage or execution authority.
- `ion-core` owns turns, frozen semantic environments, context, admission, recovery,
  durable external-effect evidence, storage and the narrow execution interfaces.
  Providers and tools do not receive arbitrary database/scheduler access.
- The application composes provider implementations, credentials, execution backends,
  live policy and clients. Live authority is not frozen into model configuration.
  `ion-terminal` owns reusable terminal mechanics, not agent execution.

Keep modules cohesive around these responsibilities. Runtime-selected providers
and tools justify narrow object-safe async interfaces; built-in control flow uses
ordinary functions and payload-bearing enums, not a generic workflow framework.

## Durable domain

| Record | Owner and meaning |
|---|---|
| Session | One consistency/storage group with one primary conversation. |
| Conversation | Immutable transcript, configuration and optional history parent. |
| Entry | A message, tool result or explicit context-boundary record. |
| Input | Accepted request, attribution, deduplication and placement. |
| Turn | A coding request's continuation, limits, cancellation and immutable terminal outcome. |
| Turn environment | Frozen semantic model/tool/execution bindings for one turn; live authority is separate. |
| Context epoch | One model-facing continuation view: retained host facts, structured checkpoint and lossless recent tail over immutable history. |
| Model step | Versioned request manifest: purpose, environment/epoch, context/input provenance, assembly revision and digest. |
| Model attempt | One physical provider dispatch with immutable failure/result/usage evidence. |
| Tool invocation | One assistant call, frozen binding/prepared action, source index and one exchange result. |
| Tool attempt | One physical execution/replay with intent, executor receipt, optional progress checkpoint and monotonic external outcome evidence. |

A turn replaces the former distributed root-task/membership/closure representation;
it is not an additional layer over it. There is no durable Agent object, generic
Effect object, arbitrary task DAG or public programmable settlement plan.

History ancestry, turn ownership, semantic environment, workspace binding, observation
and live authority are separate relationships. A fork copies no running work or
permission. One conversation has at most one unfinished turn. Inputs can queue without
entering model context. Conversation configuration is the default for a **future** turn;
starting a turn captures its TurnEnvironment, including its allowed generation/
compaction ProviderBinding routes. Each ModelStep binds one exact provider/model from
that captured route; fallback after step creation is another explicit step, never a
mutation of the existing step or environment. Changing the conversation while that turn
runs does not silently change later steps of that turn. Session identity is global;
other identities and commit cursors are distinct Rust newtypes over a private
session-local monotonic sequence. IDs escape only after commit.

## Turn execution

```text
admit input → prepare request → call model → validate response
                   ↑                           ↓
                   └──── record tool results ← execute tools
                                               or finish
```

The turn owns continuation. A logical step/invocation is distinct from each physical
provider/tool attempt so retries never erase earlier uncertainty or spend. One assistant
tool batch may run compatible effects concurrently under bounded resource claims; this is
not a generic workflow DAG. A complete result is durably staged as
`outcome_ready` in effect-completion order, then immutable tool-result entries are
materialized only in assistant source order. Already staged outcomes never replay after a
crash merely because an earlier call was unfinished.

Engine-owned semantic transactions admit input/start a turn and environment, seal
request manifests, record effect intent/evidence, stage/materialize one model-visible
result per invocation, and advance or finish the turn. Every operation has a stable
identity and idempotent settlement.

Placement, inclusion in a request and answer completion are different facts.
Duplicate admission with the same conversation-scoped request key and content returns
the original receipt; conflicting reuse within that conversation rejects. A host needing
a wider principal/global idempotency domain namespaces keys before submission; a key
never grants cross-conversation read authority. Steering enters the next complete-exchange
request boundary, never an already-dispatched request. Follow-ups start later turns.
Withdrawing unplaced input does not erase previously placed transcript entries.

Opening and inspection start no work. Submit and explicit resume authorize driving.
A dropped waiter or disconnected frontend does not cancel accepted work. A host
process that exits cannot promise continued background execution without another host.

## External actions, cancellation and recovery

Every provider or tool effect uses one effect sandwich:

```text
prepare/authorize → commit intent → effect-gate admit → backend start → external effect → commit evidence
```

Each logical ModelStep/ToolInvocation owns a stable EffectKey; every physical execution
owns a distinct AttemptId. EffectKey is derived from the Session-namespaced logical
identity rather than persisted as another entity. A frozen binding may expose it as an
external idempotency key only when that provider/backend explicitly guarantees compatible
semantics.

The durable intent transaction rechecks the owning turn's current cancellation
generation. After it commits, only that turn's process-local effect gate may cross the
external boundary. Session SQLite and an outside provider/tool boundary are not one
atomic transaction. Provider adapters and tool execution backends remain separate narrow
interfaces; Ion does not add a generic durable Effect object/backend just to share this
invariant. Each frozen binding states whether its own boundary supplies an authoritative
durable start receipt discoverable by AttemptId. A boundary with that guarantee records
the receipt/resource claim before its first externally visible effect; after process loss
the recovered receipt can prove the effect may have started, and an authoritative
negative lookup can prove it did not. Absence without that guarantee proves nothing and
stays indeterminate. Recovered receipts are persisted on the attempt before continuation.

Cancellation closes the effect gate to new admission **before** committing its durable
generation, then signals already-admitted effects after that commit. An effect that won
admission is possibly live and must be joined/reconciled; one that lost admission cannot
start after cancellation. Only committed **terminal turn success** that precedes the
cancellation mark wins. Response-ready/model-tool evidence preserves facts but cannot
authorize continuation after cancellation.

Recovery either adopts known evidence, reconciles a durable external receipt, creates a
new physical attempt when frozen/current policy and backend safety all permit replay, or
retains uncertainty. Safe replay never rewrites an earlier indeterminate attempt. A
conflicting prior possibly-live mutation must be reconciled/quiesced, isolated, or
protected by backend-guaranteed idempotency under the stable EffectKey before another
attempt starts. Unknown is not failed, free or proof of non-execution. Missing
implementations/unreadable evidence never mean unstarted.

External execution truth and transcript settlement are independent. A logical tool call
may receive one truthful model-visible "outcome unknown" result so the exchange can
continue while its ToolAttempt remains externally indeterminate. Later reconciliation
may advance that exact attempt and release workspace quarantine, even after the turn is
terminal, but never rewrites the terminal turn or emits a second tool result.

Local work is supervised and joined. Every drive returns a closed typed exit
(settled/parked/stopped/faulted or equivalent); a non-panicking error is never treated as
successful completion and a panic never fabricates a durable phase. Ambiguous
persistence failure fences the whole process-local session against new mutation/effect
admission while preserving inspection and controlled close/reopen. No blind
crash/restart loop. Cleanup has bounded capacity independent of normal execution so
saturated tools cannot prevent it.

Close stops admission and dispatch, signals and joins local work, closes storage, and
releases exclusive ownership last. It does not silently mark suspended work cancelled.
A close timeout is not a successful close: ownership remains held while local work is
unjoined. Non-abortable in-process blocking code cannot receive a hard shutdown guarantee.

## Storage and observations

SQLite is durable truth: one database per session, WAL, `synchronous=FULL`, foreign
keys and an OS-held exclusive writable-owner lock. A dedicated database thread owns
the connection and a bounded command queue. No network, tool, user callback or
provider wait occurs in a storage transaction.

Use indexed queries and bounded active caches, not a full-history resident database
mirrored through an undo journal. Validate durable relationships and versions before
trusting them. Ancestry must be acyclic and cutoffs visible; pagination arithmetic is
checked. Failure must not publish partial semantic state.

Publish committed observations only after commit. Every semantic transaction returns
one exact `CommitReceipt { seq, update }`: an ordered atomic SessionUpdate batch for
that commit. Clients never infer ordering among multiple same-commit events, and
publication uses that receipt directly rather than sampling a later "current commit."

The authoritative watch surface subscribes before taking a complete snapshot plus
coverage sequence, then discards queued batches at or below that sequence and applies
later batches atomically. Lag/overflow requires resnapshot; the observation ring is
bounded by both count and bytes. SessionUpdate is structural notification, not another
semantic database.

Token/tool progress is bounded and provisional outside the durable update stream,
addressed by model/tool attempt identity and attachment epoch. Final committed content
replaces it by identity. Late external evidence is persisted to its exact attempt before
its commit's update is published. Cancellation/control traffic remains serviceable under
output floods.

Large durable content lives in a host-owned immutable BlobStore outside the
agent-writable workspace. A BlobRef is content-addressed and bounded by hard spool/content
quotas. Publication writes/finalizes/verifies the blob under the configured durability
policy before a semantic DB commit may reference it; crashes may leave reclaimable
orphans, but committed state must never knowingly reference missing content.

Canonical tool results keep a bounded model-visible preview plus explicit truncation
metadata and optional BlobRef for complete output. Full blobs never enter model context
implicitly; bounded artifact reads page them explicitly. Running output remains
provisional, with an optional bounded attempt ProgressCheckpoint separate from the final
blob/result. Reachability GC preserves everything referenced by immutable history,
ContextEpochs and active request/attempt state.

Reserve bounded control/settlement capacity at admission and before dispatch; new inputs
and output growth cannot consume it. Managed quota refusal is not disk failure: actual
I/O failure can still fence the session. Enforce limits while reading/writing, not after
unlimited buffering. Durability remains conditional on the filesystem/platform.

## Context and model requests

History is immutable. Context operations are explicit: append, compact an older prefix,
reset and fork at a complete tool-exchange boundary. Old history stays inspectable;
no fabricated successful tool results make an incomplete fork valid. Cancellation or
abandonment records truthful cancelled/indeterminate tool outcomes; if a provider cannot
represent them, a reset excludes the exchange rather than inventing success. Acknowledgment
does not make an unsafe workspace quiescent. User reset/context changes require a quiescent
conversation; automatic compaction and steering occur only at engine-owned request boundaries.
A new answer turn uses current complete history, not a silent rewind. General-purpose
transcript rewrite/document frameworks are not part of the engine.

Model-facing context has four distinct layers:

- **Evidence history:** immutable transcript and execution records; ground truth.
- **Retained facts:** bounded host-owned model-visible facts with source identity and
  scope/lifetime; relevant user/steer instructions, verified answers, intentionally
  model-visible scoped decisions and delegation attribution live here. Bounded loss marks
  the family incomplete; live execution authority remains separate.
- **Continuation checkpoint:** bounded versioned typed model-generated execution frontier
  (goal, progress, blockers, decisions, validated evidence refs, unresolved work, next
  action/terminal condition). Schema validation is required, but content remains advisory
  and never authority.
- **Operational tail:** bounded lossless suffix of recent complete exchange groups.

Compaction creates a new ContextEpoch referencing its source cutoff, retained-facts
revision, typed checkpoint, raw-tail boundary, checkpoint/compactor revision and optional
provider-owned opaque artifact. It does not fork the visible conversation. The renderer
keeps retained facts distinct from checkpoint text and deduplicates exact retained
instructions.
Compact only at safe complete-exchange boundaries. Decide from the estimated **next
assembled request**, including newly placed input and tool results, rather than stale
last-provider usage. Large outputs are bounded/spooled before this path.

Starting a turn captures one bounded immutable TurnEnvironment: conversation
configuration revision, resolved instructions/project context, allowed generation route
(ordered ProviderBindings/fallback policy), optional compaction route, selected tool
declarations and implementation revisions, context policy, canonical workspace/executor
binding and an AuthorityCeiling defining the maximum execution classes/resources that
turn may receive. Each ModelStep selects one exact ProviderBinding from those captured
routes. Persistent configuration updates affect **later turns**, not later request
boundaries of the active turn. A future active-turn semantic/ceiling rebase, if ever
needed, is an explicit durable safe-boundary operation. Credentials and live availability
remain refreshable. Live policy may revoke/narrow immediately; broad widening applies to
later turns by default. Per-action approval can satisfy an `ask` only inside the
captured ceiling.

Each model step persists a versioned request manifest containing the environment/epoch
reference, context cutoff/input provenance, purpose, assembly revision and digest of the
canonical **semantic** provider request. The digest includes model-visible messages,
tools and semantic controls but excludes credentials, auth headers, trace IDs,
timestamps and other intentionally live transport data. Recovery reconstructs and
verifies that manifest; a build/adapter unable to reproduce it blocks rather than
silently sending a different request. Immutable content references avoid copying growing
history into every step.

The provider adapter preserves ordered content and provider-scoped replay information.
The frozen ProviderBinding carries model/provider identity, adapter/request-encoding
revision, relevant capability snapshot, context-window/token-estimator revision and
semantic controls. Credentials and intentionally live endpoints remain host capabilities.
Every ModelStep manifest names the binding/ContextEpoch and normalized request digest.

Every physical retry is a new ModelAttempt; typed failure/usage evidence from earlier
attempts remains inspectable. The engine owns retry, deadline, compaction and budget
policy. Hidden retries cannot bypass durable attempt accounting. Validate complete
responses and calls against the **frozen bindings**, including tool arguments, before
admitting execution; incomplete output cannot authorize tools or masquerade as success.

The validated terminal event ends an attempt's stream; EOF without it is incomplete.
Close the owned stream after that event rather than waiting indefinitely for EOF or
promising to inspect events after closure. Provider neutrality permits model-specific
prompt/tool profiles and does not erase real API differences.

## Execution and authority

The model-facing tool boundary is declaration plus deterministic preparation:
validate/canonicalize arguments into one exact bounded PreparedAction under the frozen
ToolBinding. It performs no external effect. Preparation runs at most once for the logical
invocation under that binding revision and persists the PreparedAction before the first
ToolAttempt; replay reuses it. If the exact preparer is unavailable, Ion does not
reinterpret the call under a newer implementation.

The host execution boundary separately owns live authority, approval, workspace claims,
sandboxing, effect admission, stop/join and reconciliation. Approval binds the exact
invocation/prepared-action digest, binding/executor revision, resources/workspace base
and expiry. Policy never silently rewrites an approved action; a changed action needs a
new digest/decision. Recheck live authority at effect admission; revocation cannot undo an
already-started action. Ordinary text is never approval.

A ToolInvocation owns one assistant call and at most one model-visible result. Every
physical run or replay has a distinct AttemptId and monotonic ToolAttempt evidence with
ordinal, cancellation generation, implementation/executor binding, intent, optional start
receipt/progress checkpoint and outcome/usage. **Only NotStarted proves no effect.** A
settled execution carries a canonical ToolResult plus an EffectSummary; an error result
may still describe known partial/complete mutation. Indeterminate means the effect truth
is unresolved, not failed. A current implementation may narrow a stored replay permission
but never upgrade an old non-replayable action.

Serialize conflicting workspace mutations or isolate workspaces. Session serialization
alone does not coordinate filesystem writes across sessions. The durable workspace
coordinator is host-owned outside the agent-writable checkout and addressed through a
WorkspaceBindingId; do not put final claim authority in `.ion/claims.sqlite` or another
file normal workspace tools can delete. Claims are keyed by invocation/attempt identity.
Known mutations advance a host workspace revision; indeterminate mutations quarantine the
binding. Prepared mutations may bind expected workspace revision and exact base-content
facts, both rechecked before effect admission.

Worktree isolation does not imply independent Git metadata: worktrees can share object,
ref and configuration state. Execution backends therefore declare repository-level
resource claims separately from workspace-file claims; shared ref/config mutations
serialize or use an isolated repository clone.

Arbitrary exec is treated as mutating unless an enforceable backend restricts it.
Cooperating Ion writers serialize, but ordinary filesystem replacement is not atomic
compare-and-swap against an uncooperative external editor. Report that limitation.
Multi-file edits preflight all targets/bases and use per-file atomic replacement where
available, but they do not claim transaction rollback unless a backend supplies it.
Known partial application returns an error ToolResult **and** an exact EffectSummary of
the applied subset, advances workspace revision, and is never NotStarted.

An unresolved possibly-live operation keeps its binding quarantined until reconciled,
confirmed stopped or replaced by an isolated binding; turn abandonment does not release
it. Verification binds to the actual tested state, not a worker's earlier result.

Capabilities must cover alternate shell/browser/extension routes. In-process extensions
are trusted code, not a sandbox. Requested confinement must fail closed if unavailable;
explicitly unconfined execution is labeled as such. Credentials remain host-owned.

## Clients and optional workers

Rust, headless and terminal clients use the same submit/inspect/observe/cancel/resume
semantics. A command captures stable target IDs and revisions. TUI focus changes and
delayed replies cannot retarget drafts, approvals or submitted work.

The initial terminal surface is an inline conversation with native scrollback,
multiline composer and compact status. One terminal owner manages input modes and
restoration; one reducer owns frontend state. Sanitize untrusted control sequences,
handle graphemes/display widths, and keep paste distinct from submission. Rendering
never waits on external I/O; output must not steal focus or destroy scroll anchors.
Real-terminal and PTY tests are required in addition to reducer tests.

Workers are optional conversations using the same Turn engine. Context shape and
lifetime are independent axes:

- **Delegate/Fresh:** bounded delegation packet, no parent transcript; use for focused
  research/review/tests.
- **Fork:** explicit complete parent history/ContextEpoch cutoff; use when the child needs
  most parent context.
- **Joined:** creator turn owns result/cancellation reach.
- **Retained:** child survives the creator turn and remains separately addressable.

Read-only children may share the parent's workspace binding; mutating children default to
an isolated worktree/workspace. Shared mutation requires explicit serialization. Worker
messages are Conversation-attributed input, never User authority or approval. A joined
child returns a bounded result/evidence packet rather than automatically injecting its
whole transcript into the parent. History inheritance, lifetime ownership and permission
inheritance remain separate. Existing workers remain inspectable when new spawning is
disabled. Single-agent requests carry no mandatory worker instructions/tools/team state.
Worker expansion follows a measured coding baseline, not an arbitrary workflow abstraction.

## Acceptance and change

This contract fixes owners, recovery semantics and trust boundaries after the
2026-09-18 v0 refinement. The current source is intentionally allowed to lag while the
targeted cutover lands; do not add compatibility shims or a parallel runtime to bridge
the old internal shape. Concrete layout, provider wire behavior, token estimates, tool
format effectiveness and performance thresholds require evidence. Change this contract
again when evidence changes an invariant rather than preserving an early decision by
inertia.

A usable baseline requires real provider requests, bounded read/edit/exec, externally
verified coding tasks and the same behavior headlessly and through the terminal.
Deterministic crash/cancellation/corruption/overload tests establish failure contracts;
live evaluation establishes effectiveness. Neither green unit tests nor resemblance
to another agent establishes state-of-the-art performance.
