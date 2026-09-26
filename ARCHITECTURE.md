# Ion architecture

Accepted contract, 2026-09-15; materially refined 2026-09-18 for external
boundaries, 2026-09-25 for native edit safety, and 2026-09-26 for the
command workspace boundary. [README.md](README.md)
states what the current source implements; this file states the contracts the
maintained engine must satisfy. Optional extensions in this document are design
constraints if implemented, not prerequisites for the serial coding baseline.
Ion is unreleased v0: replace obsolete
abstractions directly rather than preserving them through compatibility layers.

The maintained Turn/Session runtime replaces the former generic task runtime
rather than wrapping it. The headless and terminal hosts have experimental
list/read/create/edit support, but the coding-agent product and native command
execution remain incomplete.
Host preflight and descriptor-relative access are not confinement; passing
scripted recovery tests or synthetic live file tasks do not qualify a coding
agent. For current capabilities, limitations and validation, use
[README.md](README.md). The contracts below describe the target even where the
implementation is incomplete.

## Product and boundaries

Ion is a local, provider-neutral Rust terminal coding agent with an equally capable
headless/library interface. One primary conversation is the default; cooperating
workers are optional. macOS and Linux are the initial execution targets. No cloud
account, daemon or telemetry service is required; local models are ordinary providers.
An exact literal loopback HTTP endpoint may serve a local model; public endpoints
require HTTPS. Provider clients follow no redirects or ambient proxies.

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
| Tool invocation | One assistant call, frozen binding, ready prepared action or unavailable disposition, source index and one exchange result. Unavailable cannot create a physical attempt. |
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
provider/tool attempt so retries never erase earlier uncertainty or spend. The usable
single-agent baseline executes tool calls serially in assistant source order. Keep
parallel scheduling optional until coding-task measurements justify it; if enabled,
only host-proven independent read-only calls may overlap under a hard bound, and
mutating or unknown-effect calls remain serial barriers. Core does not grow a generic
batch resource scheduler. Before any tool in an admitted assistant batch crosses the
effect boundary, the batch
must be **continuation-representable**. Reserve/model-bound one minimal truthful result
envelope plus bounded preview per call and verify that, after legal compaction of older
history, the selected next-step ProviderBinding can hold the active exact
input + complete current exchange. If even the minimum exchange cannot fit, do not start
the effects; settle/park with bounded truthful not-started/context-capacity results.
Complete tool output may spill to BlobStore; only the reserved preview enters model
context. Freeze each logical invocation's effective serialized-result allowance with
batch admission and apply it to execution, staged evidence and model projection after
reopen; a retention ceiling is not a mandatory per-call context allocation. A
TurnSettings change to a smaller binding is rejected if the required current
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

The authoritative watch surface subscribes before taking a **bounded snapshot of the
requested client projection** plus coverage sequence, then discards queued batches at or
below that sequence and applies later batches atomically. Complete means complete for
that bounded projection (current state + limited transcript/worker summary), never
full-history hydration; older immutable history remains paginated. Snapshot limits are
bounded by count and bytes. The subscription tracks overflow/reset generation: overflow
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

A ModelAttempt captures any host CostQuote revision and conservative bound used for
monetary admission. Price is not part of semantic request identity. An uncapped Turn
may dispatch unpriced; that conveys no monetary protection or zero-cost assertion. A
configured ceiling requires a trusted, all-in bound for the exact request across the
frozen returned-model route, still valid at provider start. The attempt intent and
its Turn-wide reservation commit atomically; missing pricing or insufficient allowance
parks rather than guessing. Unknown usage and even a completed response retain the
bound; only durable proof that the attempt never started releases it. Later catalog
changes never rewrite historical accounting. Provider-reported billed cost is a
separate fact, not a substitute for an upper bound or a billing guarantee.

Reserve bounded control/settlement capacity, including admitted start receipts
and terminal attempt evidence, before dispatch; new inputs and output growth
cannot consume it. An oversized backend result may lose its captured output,
not demote a known terminal effect into replayable intent. Managed quota
refusal is not disk failure: actual I/O failure can still fence the session. Enforce limits while reading/writing, not after
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
defer further growth if the exact active request still cannot fit.
Until a provider-specific token and wire-format bound is qualified, serialized request
bytes also cap admission against the selected provider's asserted input-token limit.
This conservative proxy can park early and does not prove a provider will accept the
request; a wire-specific encoding may add overhead.

Compaction is **incremental from the current projection**: previous typed checkpoint plus
selected complete exchanges from the current tail/new suffix. It never reloads and
resummarizes the entire raw EvidenceHistory prefix. Trigger policy reserves enough
headroom that this bounded compactor input fits the frozen compaction ProviderBinding
before generation becomes stranded. If it cannot fit, return a typed ContextCapacity
block; do not recursively compact the compaction request or silently select another
provider. Compactor ModelSteps expose no tools/provider-hosted actions, require the typed
checkpoint output schema and use ordinary ModelStep/Attempt budget/recovery semantics.
Large outputs are bounded/spooled before this path.

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
`ServerRoute` bindings may name a bounded list of exact allowed returned-model IDs (not
prefix/pattern grants) and a compatible replay family, but their
capability snapshot is the conservative guarantee common to **all** members: required
features intersect, context/output limits use safe minima, and monetary-cap admission
uses a route-wide worst-case CostQuote. Request preparation may rely only on those
guarantees. Every ModelAttempt records the actual returned model when observable, and
anything outside the frozen route is a protocol/configuration failure rather than a
hidden fallback. An unbounded/unstated route is not a ServerRoute contract.

## Execution and authority

The model-facing tool boundary is declaration plus deterministic preparation:
validate/canonicalize arguments into one exact bounded PreparedAction under the frozen
ToolBinding. Remote/MCP ToolBindings freeze their service realm/catalog semantics and
data-egress class as part of semantic implementation identity; reconnect/auth/proxy may be
live only inside that realm, while a different remote backend is a new binding/turn. It performs no external effect. Preparation runs at most once for the logical
invocation under that binding revision and persists the PreparedAction before physical
execution. A ToolAttempt is created only by the transaction that commits execution
intent; there is no durable Prepared-attempt state. Replay reuses the PreparedAction. If
the exact preparer is unavailable *after a provider response has already arrived*, Ion
persists an unavailable disposition instead of inventing a PreparedAction or reinterpreting
the call under newer code. It closes that invocation with a source-ordered unavailable
result and no ToolAttempt; missing code before a new provider request parks.
Invalid model-supplied arguments are a separate source-ordered error result with
no PreparedAction or ToolAttempt; the model can correct them on continuation.
An invalid frozen host schema or incompatible prepared action is not disguised
as a model error.

PreparedAction includes a digest-bound required authority class: read-only, workspace
mutation, or unconfined execution. Unconfined execution requires both unconfined and
workspace-mutation permission; remote actions additionally require remote-tool permission
and their exact egress realm. These requirements are checked against the captured ceiling
at execution-intent commit. A read-only parallel declaration cannot prepare a stronger
action. The class declares requirements, not proof of confinement. Ceiling denial parks
as `AuthorityDenied`, not an approval request that could imply permission to widen it.

The host execution boundary separately owns live authority, approval, workspace claims,
sandboxing, data-egress policy, effect admission, stop/join and reconciliation.
PreparedAction identifies the execution/egress realm and bounded outbound resources for
remote effects, and live policy is rechecked before admission. Approval binds the exact
invocation/prepared-action digest, binding/executor revision, resources/workspace base
and expiry. Policy never silently rewrites an approved action; a changed action needs a
new digest/decision. Recheck live authority at effect admission; revocation cannot undo an
already-started action. Ordinary text is never approval.

A ToolInvocation owns one assistant call and at most one model-visible result.
Its `outcome_ready` state records provenance: the exact eligible ToolAttemptId that
supplied the canonical result, or an explicit synthetic source such as
cancelled-before-start/accepted-unknown/unavailable/invalid-arguments. Every physical run or replay begins at an
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
WorkspaceBindingId backed by a frozen descriptor: canonical root, execution-backend
identity and platform filesystem/repository/common-dir identity where available. A path
string alone is not durable workspace identity. Do not put final claim authority in
`.ion/claims.sqlite` or another file normal workspace tools can delete. The
registry namespace and every bound workspace/repository-admin namespace are
mutually disjoint; a workspace rooted *inside* registry state is also refused.
Claims are keyed by invocation/attempt identity. Safety-critical registry records are
bounded and self-contained: binding/resource claim, start/termination receipt identity
and effect-certainty summary cannot depend on Session SQLite or Session BlobRefs for
quarantine/release decisions. Session/Invocation/Attempt IDs are attribution only; large
outputs/diffs remain Session artifacts. Thus Session/blob deletion cannot erase an orphan
quarantine. Known mutations advance a host workspace revision; indeterminate mutations quarantine the
binding. Every mutating registry admission revalidates the current root/repository identity
against the frozen descriptor together with quarantine/revision checks. Directory
replacement, remount, symlink retarget or repository/common-dir change fails as
`BindingChanged` before a claim/effect. The old revision/quarantine never transfers to
a different object merely because the pathname is reused; adopting the replacement is an
explicit host operation. Prepared mutations may bind expected workspace revision and
exact base-content facts, both rechecked before effect admission. Registry authority is cross-process for the
current host user and outlives Session loss: a missing/deleted/corrupt Session leaves an
orphan quarantine, not a cleared claim. Baseline has no TTL or force-clear for
possibly-live attempts; reconcile with execution evidence or continue in an isolated
replacement binding.

Workspace discovery is a bounded read-only listing of one directory per call.
It returns sorted names, file kinds, a continuation cursor and the observed
workspace revision. Symlinks may be named but are not traversed; separate pages
are not a filesystem snapshot. A model can then request an exact file read.

For native single-file edit, the model supplies a concise change tied to an exact
base identity returned by `read`, rather than echoing both complete file versions.
Preparation verifies that identity against a bounded regular file, constructs the
complete expected and replacement bytes, and persists them in the PreparedAction;
the host captures the current workspace revision at preparation, then execution
rechecks that revision and the exact file base before mutation. The model does not
echo a workspace-wide revision: an unrelated earlier edit must not invalidate an
unchanged target file's digest. A create action names an absent base
and must commit only if the destination is still absent. Both operations retain
the same workspace claim, effect evidence and recovery rules. The model-facing
encoding remains versioned and must be evaluated on coding tasks; exact base
validation does not prove the model understood the user's intended change.

Stage verified replacement content in a host-owned,
rename-compatible filesystem namespace **outside** the agent-writable workspace.
Bind an exact original base and deterministically validate the proposed target
before any workspace claim; reject internally inconsistent arguments before an
effect.
The registry atomically admits an edit manifest and one immutable attempt
receipt; staging and rename eligibility are separate durable facts, not
replacement receipts.
Under permanent worker/staging custody, an exact no-follow vacancy check and
staging-parent durability barrier precede a receipt/slot/physical-parent/registry-
incarnation-bound allocation. Only a confirmed allocation permits exclusive stage
creation; it reserves bounded cleanup capacity even after Session loss or terminal
settlement. `Staged` adds physical identity, content and durability evidence for
rename eligibility. Allocation alone authenticates partial-write crash survivors
**only while the host continuously protects the namespace**, including against
same-user writers; permissions and advisory locks are not confinement. A witnessed
collision blocks cleanup rather than adopting or deleting the occupant. If that
block cannot be durably recorded/read back, stop namespace recovery: a later
unrecorded partial-stage occupant is no longer authenticated.
Disposal is independently discoverable after terminal settlement. Commit disposal
authority only after phase-compatible terminal proof, before unlink; sync the
staging parent even after an absent-name retry, then durably retire quota. An
armed unknown never gains cleanup permission merely to free capacity. Failed
cleanup retains the obligation without changing effect truth or Session evidence.
The owning, joined worker may settle an abort as `NoMutation` only if it can attest
it never invoked rename and all staging was host-internal. After durable rename
admission, recovery cannot infer nonexecution from an absent visible replacement:
retain quarantine without retry absent stronger terminal proof.
A successful cross-directory replacement requires both destination and staging-parent
durability barriers before terminal registry evidence releases the claim. Refuse native
edit where custody, identity, required barriers or same-filesystem rename cannot be
qualified; staging errors do not spill a file into the workspace.

Worktree isolation does not imply independent Git metadata: worktrees can share object,
ref and configuration state. Execution backends therefore declare repository-level
resource claims separately from workspace-file claims; shared ref/config mutations
serialize or use an isolated repository clone.

Arbitrary exec is treated as mutating unless an enforceable backend restricts it.
The first qualified prerelease must execute **native macOS binaries, including
macOS toolchains, on macOS** and native Linux binaries on Linux. Running a Linux
guest on a Mac does not satisfy the macOS requirement. Each platform needs a
host-owned enforceable command scope that can stop and positively observe
quiescence of its child and local descendants before recording known-terminal
execution evidence and releasing its claim. A truthful model-visible unknown
result may settle separately while execution remains uncertain and quarantined.
Process-group signaling alone cannot enforce the lifecycle when descendants
change group/session or after owner loss. If scope enforcement or quiescence
evidence is unavailable, refuse dispatch or retain quarantine; preparation-only
command declarations do not count as working exec.

A lifecycle scope alone is not filesystem or broker confinement. Commands must
not be able to corrupt the host-owned Session, registry or edit staging whose
integrity other tools rely on: a Linux cgroup, same-user permissions and advisory
locks do not establish that separation. A direct workspace bind exposed a
preexisting Unix-socket broker in a Fedora probe, which kept working after the
command's local descendants stopped. Therefore the initial confined command
path uses a host-private workspace view containing only ordinary source files
and directories, with an explicit symlink policy; it does not bind the live
checkout, host-owned agent state, user bus or network into the command scope.
Read-only operating-system toolchain paths, such as Linux alternatives, may be
exposed when native build tools require them. A host-selected user toolchain or
dependency cache is an explicit additional read-only mount: freeze its canonical
root identity with the tool implementation binding, keep credentials and host
configuration outside the view, and give build tools private writable state
with network disabled. Never infer from a read-only bind that a broker socket
inside the selected tree is harmless; qualify each mounted tree or refuse it.
The host must protect selected roots from hostile same-user replacement during
validation and dispatch. The host owns
bounded snapshot creation and the changed-file manifest. For a Git workspace,
the initial view omits Git-ignored paths and special filesystem entries, reports
those omissions, and rejects symlinks until their target and import semantics
are defined. A private copy of Git metadata may support read-only inspection,
but the generic importer never publishes changes to that metadata. The command
result must say which changes were imported, skipped or refused; an exit code
alone is not evidence that a private change reached the checkout. In particular,
a successful private Git command that changes metadata must report that its index,
refs or commits stayed private. After positive scope stop it imports permitted
regular-file changes through durable registry claims
and staging, persisting the bounded import plan before publication and checking
the captured base before each mutation. This applies even
when the command exits nonzero. Report exact partial import and quarantine any
unresolved outcome; never imply a failed command made no changes. Generic import
does not write protected Git metadata. Git mutations require a separate
repository-level operation or a truthful unsupported result. The snapshot,
scope and importer are one tool boundary, not a second agent runtime.

An explicitly unconfined backend may have wider effects, but its local process
receipt proves only local-descendant quiescence. It cannot claim that remote or
broker-delegated effects have finished. Its result remains `MayHaveMutated`, is
never retried automatically, and must not release a workspace claim on the
pretense that a stopped shell proves global effect completion.

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

Workers are optional and outside the usable single-agent baseline. If measured tasks
justify them, use the same Turn engine with explicit history/context inheritance,
lifetime/cancellation ownership, bounded budgets and workspace isolation. Worker messages
never become User authority or approval. Single-agent requests carry no worker prompt,
tool or coordination cost.

## Acceptance and change

This contract fixes owners, recovery semantics and trust boundaries after the v0
refinement. The current source is intentionally disposable while the **single maintained
runtime is rewritten/refactored** to this shape. Do not add compatibility shims, migration
facades or a parallel runtime to bridge the old internal representation; delete/replace
obsolete production code instead. Concrete layout, provider wire behavior, token
estimates, tool
format effectiveness and performance thresholds require evidence. Change this contract
again when evidence changes an invariant rather than preserving an early decision by
inertia.

A usable baseline requires real provider requests, file discovery, bounded
read/create/edit/exec, externally verified coding tasks and the same behavior
headlessly and through the terminal. Model routing, parallel tool scheduling, workers,
advanced compaction and automatic price discovery are options to validate after the
serial coding loop; they are not prerequisites to shipping that loop.
Deterministic crash/cancellation/corruption/overload tests establish failure contracts;
live evaluation establishes effectiveness. Neither green unit tests nor resemblance
to another agent establishes state-of-the-art performance.
