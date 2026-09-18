# Ion architecture

Accepted contract, 2026-09-15, replacing the earlier task-runtime design.
[README.md](README.md) states what the current source implements; this file states the
contracts the maintained engine must satisfy. Ion is unreleased: obsolete abstractions
are replaced, not supported through compatibility layers.

The durable turn engine and its primary storage/recovery boundaries are implemented
and covered by regressions, with the current implementation gaps called out in
[README.md](README.md). An opt-in durable workspace mutation coordinator exists, but
approval, revocation, reconciliation and confinement do not. Also not yet implemented:
context compaction, reset and history forks, artifact publication, providers other than
the scripted service, a client binary and workers. Where this document describes those,
it describes the required shape rather than shipping behavior.

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
- `ion-core` owns turns, context, admission, recovery, storage and execution
  interfaces. Providers and tools do not receive arbitrary database/scheduler access.
- The application composes providers, credentials, execution policy and clients.
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
| Turn | A coding request's continuation, limits, cancellation and outcome. |
| Model attempt | Frozen semantic request, dispatch/result evidence and usage. |
| Tool invocation | Prepared action, authorization binding and execution evidence. |

A turn replaces the former distributed root-task/membership/closure representation;
it is not an additional layer over it. There is no durable Agent object, generic
Effect object, arbitrary task DAG or public programmable settlement plan.

History ancestry, turn ownership, workspace binding, observation and authority are
separate relationships. A fork copies no running work or permission. One conversation
has at most one unfinished turn. Inputs can queue without entering model context.
Session identity is global; other identities and commit cursors are distinct Rust
newtypes over a private session-local monotonic sequence. IDs escape only after commit.

## Turn execution

```text
admit input → prepare request → call model → validate response
                   ↑                           ↓
                   └──── record tool results ← execute tools
                                               or finish
```

The turn owns continuation. Initial tool execution is sequential within a turn;
parallelism must earn its scheduling complexity. Attempts and tool invocations retain
recovery evidence, not independent generic successor graphs. Engine-owned semantic transactions admit
input/start a turn, settle a response/admit its calls, and settle results/advance the
turn. Every operation has a stable identity and idempotent settlement.

Placement, inclusion in a request and answer completion are different facts.
Duplicate admission with the same request key and content returns the original
receipt; conflicting reuse rejects. Steering enters the next complete-exchange
request boundary, never an already-dispatched request. Follow-ups start later turns.
Withdrawing unplaced input does not erase previously placed transcript entries.

Opening and inspection start no work. Submit and explicit resume authorize driving.
A dropped waiter or disconnected frontend does not cancel accepted work. A host
process that exits cannot promise continued background execution without another host.

## External actions, cancellation and recovery

Persist prepared evidence before dispatch and results before dependent continuation.
A dispatch-intent record means an action **may** have happened; it cannot prove delivery.
Recovery either adopts a known result, safely repeats under recorded policy,
reconciles an external identity, or reports uncertainty. Unknown is not failed, free,
or safe to repeat. Missing implementations or unreadable evidence never mean unstarted.

Cancellation commits intent before signaling local executors. Only committed **terminal
turn success** wins if it precedes cancellation; response-ready evidence and individual
tool results do not authorize continuation afterward. Generations fence obsolete executors.
Executors return bounded evidence to the supervisor, which owns joining and persistence;
fresh reconciliation may retain late facts without resuming cancelled continuation.
Cancellation is not rollback or proof that a remote action stopped. Unpersisted evidence
lost in a crash remains uncertain.

Local work is supervised and joined. Unexpected invocation failure stops its automatic
continuation and becomes observable; ambiguous persistence failure fences the whole
session. No blind crash/restart loop. Cleanup has bounded capacity independent of
normal execution so saturated tools cannot prevent it.

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

Publish committed observations after commit. Snapshot acquisition and subscription
are atomic. Durable cursors identify coverage; overflow/restart gaps require resnapshot.
Token/tool progress is bounded, provisional and invocation-addressed. Final committed
content replaces it by identity. Cancellation/control traffic remains serviceable under
output floods. Queues are bounded by both count and bytes.

Large content is published with integrity metadata before its durable reference.
Crashes may leave reclaimable orphan content, never knowingly publish missing content.
Reserve bounded control/settlement capacity at admission and before dispatch; new inputs
and output growth cannot consume it. Truncation is explicit. Managed quota refusal is
not disk failure: actual I/O failure can still fence the session. Enforce limits while
reading/writing, not after unlimited buffering. Durability remains conditional on the
filesystem/platform.

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

Capture configuration revision, context cutoff and input provenance atomically.
Persist resolved instructions, model controls and tool definitions for the attempt,
using immutable references rather than copying growing history repeatedly. Freeze
semantic requests, not credentials or expiring transport authentication. Configuration
updates affect later request boundaries; live authority remains separately revocable.

The provider adapter preserves ordered content and provider-scoped replay information,
reports unsupported controls explicitly, and supplies typed failures and unknown-aware
usage. The engine owns retry, deadline, compaction and budget policy. Hidden retries
cannot bypass durable attempt accounting. Validate complete responses and tool calls
before admission; incomplete output cannot authorize tools or masquerade as success.

The validated terminal event ends an attempt's stream; EOF without it is incomplete.
Close the owned stream after that event rather than waiting indefinitely for EOF or
promising to inspect events after closure. Provider neutrality permits model-specific
prompt/tool profiles and does not erase real API differences.

## Execution and authority

Tools prepare exact operations; the host authorizes and executes them in the bound
environment. Approval binds canonical arguments, implementation compatibility,
resources, workspace/base revision and expiry. Recheck live authority at execution;
revocation cannot undo an already-started external action. Ordinary text is never approval.

Serialize conflicting workspace mutations or isolate workspaces. Session serialization
alone does not coordinate filesystem writes across sessions. Arbitrary exec is treated
as mutating unless an enforceable backend restricts it. Recheck expected file state;
cooperating Ion writers serialize, but ordinary filesystem replacement is not atomic
compare-and-swap against an uncooperative external editor. Report that limitation.
An unresolved possibly-live operation keeps its binding quarantined until reconciled,
confirmed stopped or replaced by an isolated binding; turn abandonment does not release it.
Verification binds to the actual tested state, not a worker's earlier result.

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

Workers use the same turn engine. Joined lifetime explicitly propagates cancellation
and collects a selected result; retained lifetime is independent of the creator's
waiter/turn. History and permission inheritance remain separate. Existing workers
remain inspectable when new spawning is disabled. Single-agent requests carry no
mandatory worker instructions or tools. Worker expansion follows a measured coding
baseline, not an arbitrary workflow abstraction.

## Acceptance and change

This contract fixes owners, recovery semantics and trust boundaries. Concrete layout,
provider wire behavior, token estimates, tool format effectiveness and performance
thresholds require evidence. Change this contract when that evidence changes an
invariant; do not compensate with a parallel authority or compatibility shim.

A usable baseline requires real provider requests, bounded read/edit/exec, externally
verified coding tasks and the same behavior headlessly and through the terminal.
Deterministic crash/cancellation/corruption/overload tests establish failure contracts;
live evaluation establishes effectiveness. Neither green unit tests nor resemblance
to another agent establishes state-of-the-art performance.
