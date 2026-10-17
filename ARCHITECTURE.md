# Ion architecture

This document owns accepted engine and application contracts, including targets
not yet implemented. [README.md](README.md) describes delivered behavior and
qualification limits; current source establishes implementation. Ion is unreleased
v0: replace obsolete APIs, development formats and owners rather than retaining
parallel implementations or compatibility facades.

## Product

Ion is a composable Rust agent engine. A complete TUI and CLI coding agent is its
first application and qualification workload, not the boundary of the engine.
Embedded applications, bots, durable services, phone integrations and multi-agent
coordinators must be able to use the same execution and context machinery without
reimplementing the model/tool loop or reaching into executable-private state.

Ion owns agent execution, durable execution facts, shared control and managed
Session delegation. Applications may own deployment, transport, authentication,
business data, delivery queues, scheduling and swarm strategy. No mandatory daemon,
generic workflow language or Ion-owned coordination service follows from this.
Leaving infrastructure external must not leave callers to reconstruct acceptance,
execution identity, parentage or settlement themselves.

Pi and fx are primary implementation references for harness design and workflows;
fx also guides restrained presentation. Codex is a secondary Rust reference, and
Claude Code's documented contracts provide secondary workflow/SDK evidence. Inspect
relevant source; distinguish established behavior, experiments and vendor claims.
Adapt semantics to Rust, not entire runtimes or feature inventories.

## Engine concepts and owners

- **Session:** one ordered committed history and its derived conversation,
  context and execution state. Stable logical identity is separate from a file
  path, display name, provider affinity and process-local handle.
- **Turn and active operation:** a Turn is the logical work from accepted input
  through model/tool steps to settlement. Its durable identity is distinct from
  the live operation task. One exclusive operation runs per Session; different
  Sessions may run concurrently. Coding's direct shell and manual compaction are
  operations, not additional agent loops.
- **Prepared request:** one model selection, bounded context and immutable
  catalogue binding declarations to executable routes and captured authority.
- **Agent configuration:** caller-selected model services, instructions,
  capabilities and execution environment. Coding tools, project discovery and
  local workspace behavior are an application configuration, not universal defaults.
- **Ownership:** live work has a controlling owner independent of observers.
  Delegation records the relationship between independently executing Sessions;
  it does not create another worker engine.

`ion-ai` owns neutral model/content contracts. `ion-core` owns Session facts, the
single agent loop, context, recovery and durable delegation relationships.
`ion-host` owns providers/credentials, tool environments, configuration assembly,
shared control and live child supervision. `ion-app` owns client input, protocols
and presentation; `ion-terminal` owns physical terminal mechanics.

Shared control owns preparation, reservations, steering/follow-ups, stopping,
settlement and unaccepted-input recovery. Clients retain editors, modal UI and
protocol correlation. An application coordinator may compose runs, not implement
a second model → tools → model loop.

## Application configuration and authority

High-level shared control must accept a complete application configuration, not
require lower-level integration to escape coding defaults. A restricted bot must
be able to select only its own capabilities and instructions without inheriting
native shell/file tools, project rules or operator-home resources. Model and tool
implementations remain replaceable through the same lifecycle API.

Context inheritance, workspace/environment binding and callable authority are
independent choices. A Session need not require a local coding directory. A child
may inherit selected conversation context while using another workspace. Explicit
configuration and prepare-before-publish replacement must preserve these choices
across model changes, reload, reopen and Session creation.

Applications authenticate callers and authorize control/inspection. Host tools
capture trusted principal, environment and grants; model-provided arguments cannot
grant authority. Execution receives trusted Session/Turn/tool-occurrence identity
when needed for delegation, idempotency or audit, not a parent inferred from text
or a reusable provider call ID. Persist declarative identity/scope needed for
recovery, not live credentials or executors; unavailable or changed authority must
fail or be explicitly reconciled, never silently rebound by name.

Capability selection is not OS isolation. The coding configuration deliberately
uses the host user's permissions, not an implicit sandbox, VM or rollback facility.
Untrusted multi-tenant/device integrations need restricted tools and appropriate
execution isolation. OAuth is an optional access workflow, not a first-use gate.

## Agent loop and effects

Accept input, prepare, generate, commit the assistant response, settle its calls,
and continue or end. Direct calls execute in order; opted-in composition can admit
bounded concurrent tool children. There is no fixed production step-count cap
masquerading as task completion.

- Commit calls and resolved semantic activity before dispatch; commit observations
  before dependent model or guest consumption.
- Commit final answer and Turn end together. Assistant plus admitted steering is
  also atomic; failed commits leave unaccepted input recoverable.
- Admission, dispatch, observed outcome and settlement are distinct. Publish
  authoritative events after commit. Observed, not dispatched and unknown are
  different outcomes, not interchangeable error strings.
- Reject malformed/truncated calls before effects. Identifiable output-truncated
  calls may commit with synthetic not-dispatched results; otherwise do not invent
  call identity. A completed empty response is not successful work.
- Stop closes dispatch, requests cancellation and joins started work. Rendering,
  observation and connection failures cannot drop execution custody. Cancellation
  is not rollback, a hard deadline or proof of descendant/remote quiescence.
  Storage failure blocks dependent work; uncertainty grants no replay authority.

## Session, durable admission and recovery

Use one typed append-only SQLite history, an exclusive writer lease and atomic
related-fact batches. Validate candidate state before writing and publish derived
state after commit. Required durable submission, ownership and recovery facts
belong with execution truth, not a second mutable execution ledger. Business
queues/state may remain external; another storage backend needs a concrete consumer.

Distinguish these promises:

- **Conversation continuation:** reopen committed context and submit new work.
- **In-process retention:** cancelled waits do not abandon tasks or outcomes.
- **Live attachment:** observe work retained by a surviving owner.
- **Durable submission:** recover an acknowledged request and its acceptance after
  restart without submitting it twice.
- **Logical-Turn continuation:** explicitly recover authority to continue the same
  interrupted work through the existing engine.

A local queue acknowledgement is not durable acceptance. Durable submission must
bind a caller-scoped request identity to its committed admission and outcome.
Repeating that identity returns the same admission/status rather than starting
another Turn; conflicting content is an error. If queued custody is promised
across restart, persist it before acknowledging that promise. An external delivery
queue must be able to reconcile acceptance without guessing from prompt text or
maintaining competing execution truth. Admission deduplication does not make
external tool effects exactly-once.

Open and inspect passively. A committed call without an observation is unknown,
even if it might never have begun. Conservative recovery atomically closes
interrupted work before new authority. Logical-Turn continuation requires an
explicit recovery policy and reconstructed, validated authority; it does not
restore an arbitrary future, rerun a whole effectful Turn, or replay unknown calls.
Tool-specific reconciliation/idempotency may resolve uncertainty. Process loss
and task panic do not prove effects stopped. Durable and foreground operation
must use the same loop and effect ordering, not separate execution implementations.

Coding's direct shell commits command and context-sharing choice before dispatch,
then the actual host observation. Missing results remain unknown. `!` shares
command/outcome with model context; `!!` excludes both, including interruptions.
Clone/fork creates independent conversation/context from a valid committed prefix
with fresh provider affinity. It neither snapshots nor rolls back a workspace,
nor establishes delegation or security isolation. An unfinished Turn is not a
settled fork boundary. Empty Sessions must not displace useful recent conversations.

## Managed delegation and coordination

A managed subagent is another Session plus an explicit owned relationship. Record
stable parent/child identity, the initiating occurrence, chosen context and
capability/environment scope, ownership mode and observed outcome. Creation,
restart and redelivery must find the same delegation, not accidentally spawn a
second child. Parentage must be available to clients without parsing prose.

- **Foreground:** child work participates in parent settlement. Stop propagates;
  started child work joins before parent settlement. Cancelling a wait does not
  abandon the child.
- **Background:** responsibility transfers explicitly to a longer-lived owner
  before the parent releases it. Detaching a handle is not a transfer. Parent
  cancellation and service shutdown have distinct, documented scopes.

Host supervises children using the same engine and shared control. Recovery keeps
unknown child effects explicit and reconciles the relationship before redispatch.
Fresh context and an explicit fork are different. Neither automatically grants
parent secrets; confidentiality requires actual capability/environment isolation.
Code Mode's nested tool calls are not child-agent Sessions.

A swarm coordinator may choose roles, topology, message routing, budgets and
scheduling. Ion supplies execution identity, authority, observable relationships,
stop/join and outcomes. Concurrent agents sharing files need conflict policy;
cooperating-writer locks alone do not resolve incompatible edits. Worktrees,
containers or read-only roles are strategies, not a mandatory engine architecture.

### Coding delegation policy

Managed delegation is a normal capability of the main coding preset, not a
feature gate imposed on the reusable engine. Restricted applications still choose
their own complete capabilities. Child coding roles do not automatically enable
further delegation.

Unless explicitly configured otherwise, a coding child uses the initiating
request's selected model route and reasoning, the same workspace and the coding
resources/tools. Fresh context does not reset execution preferences. Context,
model, environment and authority remain independently configurable by Host;
model arguments select only choices the application has authorized.

Fresh context and foreground waiting are defaults; an explicit fork selects an
eligible committed prefix, and background execution transfers to the coding
application's managed owner. The owner is shared across runtime reload and
binding replacement, not captured by child recipes. Parent cancellation,
explicit child cancellation and application shutdown have distinct scopes.
Parent Session shutdown does not stop transferred background children.
Application shutdown stops and joins them. A one-shot headless host retains
ownership through normal completion; interruption, RPC EOF and terminal quit
stop and join it before potentially blocking final publication. No daemon is
implied.

Model-facing coding actions are start/list/status/wait/cancel/acknowledge. A
background start receipt means committed responsibility transfer, not completion.
Wait and status return retained typed outcomes without consuming them;
acknowledgement releases only joined capacity. A child's original visible answer
is retained only when its requested Turn completed and full Host/Core joining
succeeded. Failure clears it; it is never reconstructed by reopening storage.
Notifications are invalidations, not result delivery or completion proof.

Do not fabricate user instructions or successful answers to deliver
notifications. Automatic root continuation must not silently select or adopt a
managed child; explicit child selection remains available. Live-model adherence,
service-scale delegation and native GUI visual acceptance remain qualification,
not reasons to narrow the designed coding workflow.

## Capabilities and trust

A registration binds definition, semantic metadata and executor. Freeze them as
one request catalogue. MCP bindings capture connection and original remote name;
unavailable connections fail instead of redirecting or replaying calls. Local
capture cannot freeze a remote server's implementation.

Callable authority and model exposure differ. Direct tools are declared; deferred
tools are callable but omitted from the declared loadout until discovery activates
them for a later request, never the issuing request. Hidden capabilities are not
model-reachable and their MCP resources are omitted from resource tools. Restore
activation only while its definition remains identical. Exposure changes affect
future requests, never an issued request. Tool visibility is not a permission
system or sandbox.

Host validates untrusted arguments/content. Native exact edits validate disjoint
replacements against one original snapshot, preserve unmatched bytes/BOM/line
endings and respect effective write permissions. Bound inline capture and identify
omission, decoding loss and incomplete EOF. Full artifacts have a separate disk
lifecycle, not an aggregate quota or retention guarantee. Images have one bounded
normalization owner across user/native/MCP inputs; unsupported content is explicit.

Resources do not execute repository text or start unconfigured servers. Instructions,
skills/templates and results are lower-trust data. Report failures/omissions with
provenance, not silent rule truncation. MCP discovery isolates server failures and
refreshes at later request boundaries; possibly effectful invocations are never
transparently retried. MCP resources are bounded read-only context operations:
listings use frozen server identity, reads use the ordinary image or private-file
path rather than model-visible base64, and an observation grants no effect,
settlement or replay authority. Dynamic extension loading is not implied:
in-process extensions are compiled Rust application/tool registrations,
out-of-process tools use MCP, and inert instructions, skills and templates remain
lower-trust resources. Executable hooks and UI contributions are separately
scoped trusted powers, not permission to mutate history or provider wire state.

## Context and providers

Keep committed facts, bounded model context and human presentation distinct.
Commit observed outcomes with their model-projection decisions. Delivery limits
must not rewrite successful effects as failed raw observations. Reopen, fork and
compaction use the recorded projection. Failed result commits leave unknown
effects, not fabricated outcomes. Prose and summaries are claims, not verification.

Context projection, branching and compaction have one owner. Compaction changes
context, not history; commit complete call/result cuts atomically and retain the
old projection when cancelled before commit. Preserve retrieval coordinates without
replay or implicit evidence injection. Measure continuation quality, not compression
alone. Context bounds do not imply bounded resident history or artifact retention.

Prepare wire declarations and durable context from the same catalogue. Admit
model/context changes atomically before issuance. Pre-output retries retain that
preparation; compaction/reset prepares again. Clamp output ceilings to remaining
capacity. Exhausting the dispatched output budget is not itself context pressure.
Recovery is bounded and visible; incompatible context must fail or change explicitly.

Host prepares replacements before publishing them. Failed preflight leaves the
previous binding usable; active operations retain captured configuration. Logical
selection, effective model, returned identity and usage remain distinct. Model
epochs survive reopen, including A → B → A; routing policy is not correctness state.

Adapters own framing, errors and opaque provider continuation. Human-readable
thinking differs from signatures/encrypted replay and returns to providers only
through adapter-owned replay. It counts as observed output whether displayed or
not. Preserve signed prefixes or report reset/incompatibility; never reset inside
an outstanding signed exchange. Core must not branch on provider names.

Caching/affinity are optimizations, not authority. Affinity survives reopen and
changes on fork/clone. Warming is bounded, separately accounted and subordinate
to settlement. Transient provider retries are bounded, cancellable and allowed
only before observed output, without repeating completed tools.

## Clients and extension boundaries

Clients use typed shared control; RPC encodes it rather than defining a lifecycle.
Input custody is `draft → preparing → ready/queued → accepted | returned | withdrawn`.
State whether queue/acceptance promises are volatile or durable. Preserve original
kind, literals, attachments and reservations until acceptance or explicit handoff.
Validate before acknowledgement; a byte reservation is not a context/RSS guarantee.
Expand queued resources at admission. Steering joins a safe boundary in its Turn;
only explicit follow-ups start later Turns. Return late steering and unaccepted
inputs on failure/stop, including successful preparation racing cancellation.
Preparation remains cancellable/joinable without blocking controls; bounded active
decoders need not support hard interruption.

Owners and observers are separate. Dropping/timing out/aborting an owner requests
stop without relinquishing started-work custody. A service can retain and drive
that owner independently of transport connections. Observation loss/detachment
cannot decide execution lifetime. Foreground clients may stop on genuine I/O loss,
but saturation is backpressure, not execution failure. Reserve lifecycle/recovery
capacity before effects; do not use unbounded deferred replies or put cancellation
behind output.

Remote attachment must identify the Session/work and provide an authorized
snapshot plus an ordered committed revision/cursor or equivalent reconciliation
contract. Include relevant lifecycle/queue/settlement state, not just model deltas.
Live progress may be lossy; gaps require resync, never suffix-splicing. Attachment,
transport acknowledgement and snapshot access do not grant execution authority.
Applications may implement authenticated network transports over these shared
operations; neither a required daemon nor a second client loop is implied.

Headless text publishes committed final text. Protocols distinguish acceptance,
progress, settlement and stop intent without exactly-once transport promises.
Broken output cannot promise delivery; committed inspection remains available.
Controlled foreground EOF closes admission and stops/joins before final recovery
publication. A healthy final pipe still requires a reader.

Shutdown closes admission before stop/join. Cancelling its wait retains closure,
handles and failures; finish it before destroying the runtime. Drop cannot promise
async cleanup. A genuine task panic blocks live owner reuse and reports unknown
effects. No synchronous idle guard crosses await.

## Terminal experience

Inline native scrollback is primary; fullscreen is another viewport over the same
semantics. Prefer restrained fx-like hierarchy and progressive disclosure to tool
cards or a flat stream. Prompts, prose, activity, mutations and notices must remain
legible without color. Nested activity uses recorded parentage, not an invented
process tree. Queued is not running or successful.

Keep drafts editable during work: multiline/history, paste/images, external editor,
steering and follow-ups. Commands/pickers retain unsent input. Budget activity and
composer together; keep state/exceptions visible with honest omission/inspection.
Coalesce benign repetition, not mutations/failures. Details retain arguments,
observations, capture paths and delivery notices.

Use one control-safe grapheme/display-width policy for content; composer byte/cursor
mapping is separate. Semantic styles survive redraw and irreversible publication.
Publish settled content once. A closed group need not be final. Native reflow belongs
to the emulator; resume publishes a bounded recent tail with earlier facts inspectable.
Resize, scrolling, viewport changes and exit preserve composer/terminal custody.
I/O failure stops input/redraw and awaits settlement before reporting; panic/crash
cannot promise that. Physical publication is never execution authority.

## Hybrid composition

Code Mode is opt-in beside direct calls/discovery. A confined Host VM gets owned
code, limits and a bounded bridge; Core retains Session/catalogue authority. It is
not another agent loop or a durable JavaScript runtime. Pi is the model-facing
authoring reference: named discoverable calls, useful declarations, selected
structured/text/image output and actionable errors. README states the implemented
subset; no comparative speedup is assumed.

Identify a parent by committed assistant-entry sequence and call ordinal, not a
reusable provider ID. Commit nested tool intent before dispatch and observation
before guest consumption. Parent settlement joins its tool children; recovery
closes unknown children without rerunning code. Selected results reach model
context; raw child facts stay inspectable, not fabricated conversation messages.

Guest failure exposes a stable parent locator for bounded metadata/JSON inspection.
Inspection preserves observed/not-dispatched/unknown, excludes private shell
context, opaque replay and normalized image bytes, and grants no replay authority.
Capture paths may disappear; saved observations, host state and missing evidence
are distinct.

Normal return drains transferred requests and started effects. Failure, cancellation
and budget/storage faults close admission and request stop while retaining futures
and joining the VM. Failed observation commits block consumption/dependent dispatch,
even when guest code catches errors. VM limits, native resources, capture/audit
and delivery have separate owners; VM isolation does not sandbox host tools.

## Qualification

Qualify the shared engine through coding and at least one restricted non-coding
embedding rather than assuming terminal success proves reuse. Durable requests,
logical continuation, attachment and foreground/background delegation need their
own crash/cancellation/redelivery evidence before being advertised.

Preserve distinct integrity/concurrency/cancellation/recovery protections; prune
obsolete fixtures and private-call assertions. Exercise changed built clients and
repository gates. Source, mocks/PTY, live tasks and native-emulator/human review
establish different things. Neither green tests nor reference similarity certifies
performance, model adherence, terminal stability or daily-driver readiness.
