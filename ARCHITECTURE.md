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
| Turn | A coding request's continuation, inline immutable TurnEnvironment, limits, cancellation and terminal outcome. |
| Model step | Versioned semantic request plus `open | selected(AttemptId) | superseded` disposition, exact ProviderBinding, context/input provenance, assembly revision and digest. |
| Model attempt | One physical provider dispatch with immutable failure/result/usage evidence. |
| Tool invocation | One assistant call, frozen binding/prepared action, source index and one exchange result. |
| Tool attempt | One physical execution/replay with intent, executor receipt, optional progress checkpoint and monotonic external outcome evidence. |

A turn replaces the former distributed root-task/membership/closure representation;
it is not an additional layer over it. There is no durable Agent object, generic
Effect object, arbitrary task DAG or public programmable settlement plan.

History ancestry, turn ownership, semantic environment, workspace binding, observation
and live authority are separate relationships. A fork copies no running work or
permission. One conversation has at most one unfinished turn. Inputs can queue without
entering model context. Conversation configuration is the default for a **future** turn. Starting a turn captures
one immutable TurnEnvironment **inside the Turn**, including a frozen allowed
ProviderBinding set/routes, frozen allowed ToolBindings, generation-control ranges,
workspace/executor identity and AuthorityCeiling. The baseline has no active-turn
environment rebase. The Turn also stores a small revisioned TurnSettings selection
(provider binding, permitted controls and active tool-loadout subset) constrained by the
environment. Explicit user/host changes or frozen fallback policy affect future
ModelSteps only; no new implementation/capability can enter mid-turn. Changing
conversation configuration while that turn runs affects later turns only. Session
identity is global;
other identities and commit cursors are distinct Rust newtypes over a private
session-local monotonic sequence. IDs escape only after commit.

Persisted provider/tool implementation revisions are **semantic compatibility IDs**, not
build/display versions. Current code may execute/recover old work only when it explicitly
advertises the exact persisted ID; changes to preparation, request encoding/replay,
receipt interpretation or effect/recovery semantics require a new ID. Build hashes may be
diagnostic metadata but do not prove compatibility, and a shared name never implies it.

## Turn execution

```text
admit input → prepare request → call model → validate response
                   ↑                           ↓
                   └──── record tool results ← execute tools
                                               or finish
```

The turn owns continuation. A logical step/invocation is distinct from each physical
provider/tool attempt so retries never erase earlier uncertainty or spend. Core does not
grow a generic batch resource scheduler: each frozen ToolBinding is either `Serial`
(default) or host-proven `ParallelSafeReadOnly`. Execute maximal contiguous source-order
runs of `ParallelSafeReadOnly` calls with a hard concurrency limit; every `Serial` call
is a barrier that waits for the preceding run, executes alone, and settles before later
calls start. Mutating/unknown/remote-effect tools are Serial in baseline. Before any tool in an admitted assistant batch crosses the effect boundary, the batch
must be **continuation-representable**. Reserve/model-bound one minimal truthful result
envelope plus bounded preview per call and verify that, after legal compaction of older
history, at least one frozen allowed next-step ProviderBinding can hold the active exact
input + complete current exchange. If even the minimum exchange cannot fit, do not start
the effects; settle/park with bounded truthful not-started/context-capacity results.
Complete tool output may spill to BlobStore; only the reserved preview enters model
context. A TurnSettings change to a smaller binding is rejected if the required current
continuation cannot fit it.

A complete result is durably staged as
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

Opening and inspection are semantically passive. Open may acquire exclusive
ownership and validate/load bounded local durable state, but it performs no semantic
recovery writes and no provider/tool-backend reconciliation, claim acquisition,
dispatch/retry, timer drive, worker start or cleanup effect. Inspection may therefore show
unreconciled durable attempts exactly as stored.

Submit and explicit resume authorize driving. Resume reconciles existing attempt/backend
evidence first, persists recovered facts, rechecks cancellation/supersession eligibility,
and only then may start new physical work. A dropped waiter or disconnected frontend does
not cancel accepted work. A host process that exits cannot promise continued background
execution without another host.

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
retains uncertainty. Safe replay never rewrites earlier evidence. Baseline never
intentionally overlaps two physical ToolAttempts for one ToolInvocation: every prior tool
attempt must be known non-live/terminal **and its outcome evidence must authorize another
execution**. Automatic retry is limited to NotStarted or a typed retryable failure with
NoMutation/equivalent binding receipt under the frozen recovery policy.
KnownChanges/MayHaveMutated do not become retry-safe merely because execution terminated.
Backend idempotency may deduplicate accidental delivery or strengthen reconciliation, but
does not authorize overlap. Changing to an isolated workspace/binding is later work under
a **new invocation/turn**, not replay of the frozen PreparedAction. Unknown is not failed,
free or proof of non-execution. Missing implementations/unreadable evidence never mean
unstarted.

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
later batches atomically. The subscription tracks overflow/reset generation: overflow
before the snapshot handoff completes invalidates the handshake and forces a fresh
snapshot/watch instead of returning a stream that already has a gap. Lag/overflow after
handoff likewise requires resnapshot. The ring is bounded by both count and bytes.
SessionUpdate is structural notification, not another semantic database.

Token/tool progress is bounded and provisional outside the durable update stream,
addressed by model/tool attempt identity and attachment epoch. Final committed content
replaces it by identity. Late external evidence is persisted to its exact attempt before
its commit's update is published. Cancellation/control traffic remains serviceable under
output floods.

Large durable content lives in a host-owned immutable **Session-scoped**
BlobStore outside the agent-writable workspace. Baseline has no cross-session blob
dedup/refcount: workers/conversations in one Session share a namespace, so GC only needs
that Session's durable references. A BlobRef is content-addressed within its owning
Session and bounded by hard spool/content quotas. Publication writes/finalizes/verifies
the blob under the configured durability policy before that Session DB may reference it;
crashes may leave reclaimable local orphans, but committed state must never knowingly
reference missing content.

Canonical tool results keep a bounded model-visible preview plus explicit truncation
metadata and optional BlobRef for complete output. Full blobs never enter model context
implicitly; bounded artifact reads page them explicitly. Running output remains
provisional, with an optional bounded attempt ProgressCheckpoint separate from the final
blob/result. Session-local reachability GC preserves everything referenced by immutable
history/context-boundary entries and active request/attempt state; Session deletion
closes ownership before removing its blob namespace. Cross-session export copies and
verifies content explicitly.

The field holding a BlobRef determines whether it is **semantic-required** or auxiliary.
Required out-of-line content needed to reproduce an active Turn/ModelStep (such as a
large frozen AllowedToolSet) is digest-verified before use; missing/corrupt required
content fences Session mutation and blocks resume/dispatch rather than being rebuilt from
current host state. Auxiliary evidence such as full stdout behind an already-committed
bounded ToolResult may become unavailable without rewriting that result or effect truth;
artifact reads return explicit ContentUnavailable instead of empty/fabricated bytes.
Open need not hash all historical blobs: verify required content at consumption,
auxiliary content at read, and all reachable content only in explicit integrity/export.

Every ModelAttempt dispatch also captures the host CostQuote revision/rates or
conservative bound used for monetary admission. Price is not part of semantic request
identity. A configured monetary ceiling dispatches only when a conservative reservation
can be made; missing/unknown bounded pricing parks rather than guessing, unknown usage
retains its reservation, and later price-catalog changes never rewrite historical
attempt accounting. Provider-reported billed cost may be stored separately; host-side
caps are not represented as provider billing guarantees.

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
- **Retained inputs:** ordered references to immutable Input records, not copied payloads.
  They contain only User/Steer content already consumed into the active Turn's
  model-visible exchange plus verified InteractionReply Inputs already consumed through
  their canonical tool result. Queued/unplaced input is never exposed early. Admission/
  steering/question budgets keep this current-turn set exact or refuse/defer further
  consumption. Older ordinary user messages are **not** mechanically re-emitted across
  turns: without explicit instruction-lifetime/rollback semantics that could resurrect
  superseded text. Older history remains immutable evidence and is represented by the
  advisory checkpoint/tail; requirements that must persist exactly across turns belong in
  explicit conversation instructions/configuration. Model memories, worker chatter and
  live approval authority do not enter this layer.
- **Continuation checkpoint:** bounded versioned typed model-generated execution frontier
  (goal, progress, blockers, decisions, validated evidence refs, unresolved work, next
  action/terminal condition). Schema validation is required, but content remains advisory
  and never authority.
- **Operational tail:** bounded lossless suffix of recent complete exchange groups.

Compaction/reset appends one immutable **ContextBoundary Entry** containing its source
cutoff, ordered exact active-turn RetainedInput IDs, typed checkpoint, raw-tail range,
checkpoint/compactor revision and optional provider-owned opaque artifact. The boundary
never duplicates retained input bodies. The opaque artifact is an encoding optimization with explicit provider/model/
replay-family compatibility identity, never semantic truth: compatible bindings may use
it; incompatible switching reconstructs from the typed checkpoint + retained inputs +
raw tail instead. The compatibility choice is part of the request manifest/fingerprint.

Projection is source-aware: a retained InputId is rendered only when its canonical
model-visible occurrence is no longer in the raw tail/current suffix. Ordinary user/steer
input is therefore never duplicated. A targeted InteractionReply appears through its
canonical tool-result exchange while that exchange is retained; after compaction removes
that raw exchange, the exact verified question + reply is reconstructed from the
invocation PreparedAction and immutable reply Input. Model-generated checkpoint text
never substitutes for or duplicates this retained evidence.
`ContextEpoch` is only the model-facing projection identified by that EntryId; it is not
another table/entity. The initial epoch is implicit before the first boundary. It does
not fork the visible conversation. The renderer keeps retained inputs distinct from
checkpoint text and deduplicates exact retained instructions.
Compact only at safe complete-exchange boundaries. Decide from the estimated **next
assembled request**, including newly placed input, tool results, exact active-turn
retained input and tail, rather than stale last-provider usage. Never evict retained
current-turn input merely to fit: compact/trim older history/tail first, then refuse or
defer further growth if the exact active request still cannot fit. Large outputs are bounded/spooled before this path.

Starting a turn captures one bounded immutable TurnEnvironment value directly in the
Turn: conversation configuration revision, resolved instructions/project context, frozen
allowed ProviderBindings plus default/fallback/compaction routes, frozen allowed
ToolBindings, permitted generation-control ranges, context policy, canonical
workspace/executor binding and an AuthorityCeiling defining the maximum execution
classes/resources that turn may receive. It also installs initial revisioned TurnSettings
(provider binding, controls, active tool-loadout subset) constrained by that environment.

Persistent semantic configuration updates and **AuthorityCeiling widening** affect
later turns. Explicit turn-setting changes may switch model/reasoning/service tier or tool
subset only within the frozen environment and affect future ModelSteps. Credentials/live
availability remain refreshable.

Live policy is mutable **inside** the frozen ceiling: revocation/narrowing applies
immediately, and an explicit authenticated user/host policy change may alter future
allow/ask decisions within that ceiling during the turn. Passive config/file reloads do
not silently widen current-turn policy. Per-action approval can satisfy one `ask` only
inside the captured ceiling; neither approval nor policy can widen the ceiling. Do not
carry environment-rebase machinery until a measured requirement justifies it.

Baseline also does not auto-reread agent-writable AGENTS/project instruction files during
the active Turn. Resolved project/developer instructions are part of the frozen
environment; User/Steer Inputs are the dynamic instruction channel. This prevents tool
effects from silently changing their own future prompt semantics and keeps request replay
independent of later workspace bytes. A future live instruction provider, if measured
need justifies it, must enter through an authenticated host operation that durably
captures the exact future-step snapshot rather than implicit filesystem observation.

Each model step persists a versioned request manifest containing the TurnEnvironment
digest, captured TurnSettings revision/value, exact ProviderBinding/tool loadout,
ContextBoundary EntryId (or implicit initial epoch), context cutoff/input provenance and
purpose, plus **two fingerprints**: the canonical provider-neutral semantic request digest
and the exact frozen adapter's canonical provider-request fingerprint. The semantic digest
includes model-visible messages/tools/controls but excludes live credentials, auth
headers, trace IDs and transport timestamps. The provider fingerprint covers the
provider-specific body/replay/tool/control representation and stable idempotency material,
but excludes live auth/routing/telemetry.

Before every physical dispatch, including recovery, Ion reconstructs and verifies the
semantic request, re-prepares it with the frozen adapter/encoding revision and verifies
the provider fingerprint. Any mismatch or unavailable encoding blocks before network
dispatch. Immutable references avoid storing another full copy of growing history/raw
wire bytes.

The provider adapter preserves ordered content and provider-scoped replay information.
The frozen ProviderBinding carries a semantic **service realm** (provider/backend +
model/route contract plus data-egress/privacy class), adapter/request-encoding revision,
relevant capability snapshot,
context-window/token-estimator revision and semantic controls. Credentials/auth refresh,
proxies/DNS and explicitly equivalent regional/network routing may remain live host
capabilities inside that realm. Switching to a different compatible API/backend is a new
ProviderBinding, not a live endpoint refresh; EffectKey idempotency is never assumed
across service realms. Every provider dispatch also rechecks current host data-egress
policy inside the Turn's frozen ceiling. A local/on-device route cannot silently fall
back to a cloud realm unless that realm was explicitly frozen as allowed and remains
permitted live; a blocked realm parks before network I/O. Every ModelStep manifest names the binding/context-boundary
projection and normalized request digest.

Canonical history links tool calls/results by ToolInvocationId, never by a provider's
call/item ID. Origin provider IDs are replay metadata. ReplayProjection preserves exact
IDs for compatible replay families; for an incompatible target it deterministically maps
each ToolInvocationId to a target-valid wire ID and uses that alias consistently for the
call/result pair. The map is derived and collision-checked during request preparation,
not stored as a second identity system, and is covered by the provider-request
fingerprint.

ModelStep is the prepared semantic request and carries one durable disposition:
`open | selected(AttemptId) | superseded(reason, successor_step?)`. A ModelAttempt is
created only when physical dispatch intent commits; there is no durable Prepared-attempt
state. Every physical retry is therefore a new AttemptId with typed
dispatch/start/failure/response/usage evidence.

ResponseReady is attempt evidence and may be persisted even after cancellation or
supersession. An `open` step selects at most one validated ResponseReady attempt only
through a transaction that also rechecks the Turn's current cancellation generation/
intent and that the step is still the current eligible step. That transaction changes
`open → selected(AttemptId)`. If cancellation or supersession committed first, the late
response remains usage/diagnostic evidence and is never selected. A timed-out earlier
same-step retry therefore cannot append a second assistant entry or admit tools. Physical
retries to the same binding share the ModelStep/EffectKey.

A provider/model fallback, compaction-mediated regeneration or otherwise semantically
different request is another ModelStep. Creating that successor atomically changes the
predecessor `open → superseded`; a selected step cannot be superseded. After commit,
signal old live attempts to stop best-effort, but do not infer remote termination.
Superseded attempts remain charged/reserved until evidence resolves and may still settle
usage/diagnostics, while being permanently ineligible for semantic selection. Budget/
capacity policy may delay the successor if it cannot conservatively cover both. Thus a
late old-provider response can never race a different fallback request into the
transcript.

The engine owns retry, compaction and budget policy. Timing is boundary-specific:
ProviderBindings/ModelAttempts and PreparedActions/ToolAttempts capture their concrete
timeouts; retry backoff has bounded attempts and durable not-before times. A Turn has no
mandatory wall timeout, but may carry an explicitly configured absolute wall deadline for
unattended/automation use. Durable user questions/approvals may otherwise remain parked
without consuming compute. A timeout never proves an already-admitted effect stopped.
Hidden retries cannot bypass durable attempt accounting. Validate complete responses and calls against the
**frozen bindings**, including tool arguments, before admitting execution; incomplete
output cannot authorize tools or masquerade as success.

The validated terminal event ends an attempt's stream; EOF without it is incomplete.
Close the owned stream after that event rather than waiting indefinitely for EOF or
promising to inspect events after closure. Provider neutrality permits model-specific
prompt/tool profiles and does not erase real API differences.

Baseline provider requests expose **no provider-hosted side-effect tools**. Any action that
can mutate workspace/account/browser/external state must return as an Ion ToolInvocation
and cross the frozen ToolBinding, live host authority and ToolAttempt boundary. This keeps
uncertain ModelAttempt replay limited to billing/output nondeterminism rather than hidden
user-side effects. A future provider-native read-only/isolated augmentation may be added
only with explicit bounded data/usage, privacy and replay/projection semantics;
provider-hosted computer/action tools do not bypass execution authority.

ProviderBinding also freezes model-routing semantics. Default `ExactModel` rejects an
unexpected returned semantic model when the protocol exposes one. Explicit
`ServerRoute` bindings may name a bounded allowed returned-model/replay family, but their
capability snapshot is the conservative guarantee common to **all** members: required
features intersect, context/output limits use safe minima, and monetary-cap admission
uses a route-wide worst-case CostQuote. Request preparation may rely only on those
guarantees. Every ModelAttempt records the actual returned model when observable, and
anything outside the frozen route is a protocol/configuration failure rather than a
hidden fallback. An unbounded/unstated route is not a ServerRoute contract.

## Execution and authority

The model-facing tool boundary is declaration plus deterministic preparation:
validate/canonicalize arguments into one exact bounded PreparedAction under the frozen
ToolBinding. It performs no external effect. Preparation runs at most once for the logical
invocation under that binding revision and persists the PreparedAction before physical
execution. A ToolAttempt is created only by the transaction that commits execution
intent; there is no durable Prepared-attempt state. Replay reuses the PreparedAction. If
the exact preparer is unavailable, Ion does not reinterpret the call under a newer
implementation.

The host execution boundary separately owns live authority, approval, workspace claims,
sandboxing, effect admission, stop/join and reconciliation. Approval binds the exact
invocation/prepared-action digest, binding/executor revision, resources/workspace base
and expiry. Policy never silently rewrites an approved action; a changed action needs a
new digest/decision. Recheck live authority at effect admission; revocation cannot undo an
already-started action. Ordinary text is never approval.

A ToolInvocation owns one assistant call and at most one model-visible result.
Its `outcome_ready` state records provenance: the exact eligible ToolAttemptId that
supplied the canonical result, or an explicit synthetic source such as
cancelled-before-start/accepted-unknown. Every physical run or replay begins at an
execution-intent commit and has a distinct AttemptId
with monotonic ToolAttempt evidence: ordinal, cancellation generation,
implementation/executor binding, optional start receipt/progress checkpoint and
outcome/usage. **Only NotStarted proves the effect never began.** A settled execution
carries a canonical ToolResult plus a typed EffectSummary; ToolResult success/error and
mutation knowledge are independent.

EffectSummary has a small closed certainty vocabulary: `NoMutation` when the enforced
execution class proves no user/external mutation; `KnownChanges` when concrete changed
resources/content identities are known; `MayHaveMutated` when execution is known
terminal but its permitted mutation scope cannot be enumerated exactly; or a
binding-specific external receipt summary whose semantics are frozen by the binding.
Baseline arbitrary exec settles as `MayHaveMutated`, whether it exits zero or returns an
error. A terminal `MayHaveMutated` attempt is no longer “possibly still running,” but
the affected workspace/resource revision advances conservatively so stale prepared bases
are invalidated. Indeterminate remains unresolved and retains any required quarantine.
A current implementation may narrow a stored replay permission but never upgrade an old
non-replayable action.

Serialize conflicting workspace mutations or isolate workspaces. Session serialization
alone does not coordinate filesystem writes across sessions. The durable workspace
coordinator is host-owned outside the agent-writable checkout and addressed through a
WorkspaceBindingId; do not put final claim authority in `.ion/claims.sqlite` or another
file normal workspace tools can delete. Claims are keyed by invocation/attempt identity.
Known mutations advance a host workspace revision; indeterminate mutations quarantine the
binding. Prepared mutations may bind expected workspace revision and exact base-content
facts, both rechecked before effect admission. Registry authority is cross-process for the
current host user and outlives Session loss: a missing/deleted/corrupt Session leaves an
orphan quarantine, not a cleared claim. Baseline has no TTL or force-clear for
possibly-live attempts; reconcile with execution evidence or continue in an isolated
replacement binding.

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

An unresolved possibly-live operation keeps its binding quarantined until reconciled
or confirmed stopped; turn abandonment does not release it. Later coding work may move to
a **new** isolated binding, but that does not clear or replay the original invocation.
Verification binds to the actual tested state, not a worker's earlier result.

Capabilities must cover alternate shell/browser/extension routes. In-process extensions
are trusted code, not a sandbox. Requested confinement must fail closed if unavailable;
explicitly unconfined execution is labeled as such. Preflight path canonicalization is
not confinement: the execution backend must enforce filesystem/network authority at the
actual effect boundary, and confined file tools must resolve against the bound root
without allowing symlink/path swaps to escape it. Use platform-native race-resistant
resolution/enforcement where available or refuse the stronger claim. Ordinary edit tools
do not mutate protected repository administrative metadata or Ion host state through the
generic file path; those require explicit authority/resource handling. Credentials remain
host-owned.

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
inheritance remain separate.

Budgets are **transferred, not shared through a live parent pointer**. Joined spawn
atomically carves a fixed child allowance from the creator Turn's remaining worker/spend
allowance and installs it in the child; the creator can no longer spend it, preventing
concurrent child oversubscription. Baseline accounting is monotonic and does not reclaim
unused child allowance. Model-driven spawn is Joined by default. An authenticated
host/user may create a Retained worker directly with an independent budget/configuration,
because it may outlive the originating request. Baseline has no Joined→Retained promotion;
continuity uses an explicit new retained Fresh/Fork conversation instead of transferring a
live cancellation edge.

Fan-out also has explicit durable limits: per-Turn child count, worker-origin depth and
Session active-worker count. Worker depth follows creator/origin metadata rather than
history ancestry, so a fresh parentless delegated conversation still consumes one depth
level. Spawn admission checks limits, records origin/lifetime, transfers budget and
creates the child Turn atomically; refusal leaves neither a child nor a consumed
allocation.

Existing workers remain inspectable when new spawning is disabled. Single-agent requests
carry no mandatory worker instructions/tools/team state. Worker expansion follows a
measured coding baseline, not an arbitrary workflow abstraction.

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
