# Ion architecture

This document owns Ion's accepted design and execution contracts.
[README.md](README.md) describes implemented behavior and qualification limits;
source establishes what the current build does. Ion is unreleased v0: replace
obsolete APIs and development formats rather than keeping parallel implementations
or compatibility layers.

## Product

Ion is a local Rust coding agent with complete TUI, CLI and embedded-library
use of the same engine. A user chooses a model, inspects and changes a working
directory, observes and intervenes in work, and continues after relaunch.
Native tools inherit host permissions, not an implicit sandbox or rollback
facility. OAuth is an optional access workflow, never a first-use gate.

Design the execution, context and ownership boundaries for durable work and
subagents as well. Other applications can supervise Sessions, coordinate agents
or provide services without reimplementing the coding loop. Ion need not own
that surrounding infrastructure; broader capability delivery follows concrete
workflows, not a union of reference feature inventories.

Pi and fx are the primary implementation references for harness design and
workflows; fx also guides restrained presentation. Codex is a secondary Rust
reference, and Claude Code's documented contracts provide secondary workflow
and SDK evidence. Inspect relevant upstream implementation, distinguishing
established behavior, experiments and vendor claims. Adapt useful semantics
idiomatically to Rust; copy no runtime, appearance or architecture wholesale.

## Four concepts

1. **Session:** ordered committed conversation and execution facts, including
   acceptance, recovery and context history.
2. **Active operation:** one exclusive coding Turn, direct shell or compaction,
   retained until its started work settles. A user message starts a Turn; a
   model response is a step within it. Different Sessions may run concurrently.
3. **Prepared request:** one model selection, bounded context and immutable
   executable catalogue binding declarations to execution routes.
4. **Host services:** providers, credentials, tools, MCP, project resources and
   shared session control. Clients translate input and present results.

`ion-ai` owns neutral model/content contracts; `ion-core` owns Session, execution,
context and abstract capabilities; `ion-host` implements services and the shared
control API; `ion-app` owns client interaction/protocol/presentation;
`ion-terminal` owns physical terminal mechanics. There is one implementation of
the model → tools → model loop. External applications may compose its runs.

Shared session control owns submission/preparation, steering, follow-ups and
unaccepted-input recovery. These rules must not be independently implemented
by terminal, RPC and library callers. Editors, modal UI and protocol correlation
remain client responsibilities, not fields in a universal frontend framework.

## Coding loop and effects

Accept input, prepare, generate, commit the assistant response, settle its calls,
and continue or end. Direct model calls execute in order; opted-in composition
can admit bounded concurrent children. There is no fixed production step-count
cap masquerading as task completion.

- Commit assistant calls and resolved semantic activity before dispatch;
  commit observations before dependent model or guest consumption.
- Commit final answer and Turn end together. Assistant plus admitted steering
  is also atomic; a failed commit leaves unaccepted input recoverable.
- Distinguish admission, execution start, observed outcome and settlement.
  Publish authoritative events only after commit. Observed, not dispatched
  and unknown are different outcomes, not interchangeable error strings.
- Reject malformed/truncated calls before effects. Identifiable output-truncated
  calls may commit with synthetic not-dispatched results; otherwise do not
  invent call identity. A completed empty response is not successful work.
- Stop closes new dispatch, requests cancellation and joins started work.
  Rendering, observer or connection failure cannot drop its execution future.
  Cancellation is not rollback, a hard deadline or proof of descendant/remote
  quiescence. Storage failure blocks dependent work; uncertainty never grants
  automatic replay authority.

## Session and recovery

Use one typed append-only SQLite history, an exclusive writer lease and atomic
event batches. Validate candidate state before writing, publish state after
commit, and derive indexes/lifecycle facts from that history. Do not add a
second mutable lifecycle ledger or persist live executors and credentials.

Open and inspect passively. A committed call without an observation is unknown,
even if execution might never have begun. Before new coding or shell authority,
atomically close interrupted work and admit new input; failed recovery grants
no authority. Resume continues the conversation, not an interrupted external
action. Process loss and task panic do not prove effects stopped.

Direct shell commits its command and context-sharing choice before dispatch,
retains that identity through settlement, and commits the actual host observation.
A missing result stays unknown. `!` shares command/outcome with model context;
`!!` excludes both, including interruptions. Shell operations are not coding
Turns.

Clone/fork creates an independent Session from a valid committed prefix and
context boundary, with fresh transport affinity. It neither snapshots nor rolls
back the workspace. An unfinished Turn is not a settled fork boundary; empty
Sessions must not displace useful recent conversations.

## Capabilities and trust

A registration binds a definition, semantic metadata and executor. Freeze them
as one request catalogue, not a mutable host/name registry. MCP bindings capture
the connection and original remote name; an unavailable connection fails rather
than silently redirecting a call. Local capture cannot freeze a remote server's
implementation.

Callable authority and model exposure differ. Direct tools are declared;
discovery activates deferred registrations for later requests, never the issuing
one. Restore an activation only while its definition remains identical. Overrides
and refresh affect future requests. Exposure is not a permission system or sandbox.

Host boundaries validate untrusted arguments and content. Exact edits validate
all replacements against one original snapshot, preserve unmatched bytes and
ordinary BOM/line endings, and respect effective write permissions. Bound inline
captured text and identify omission, decoding loss and incomplete capture. Full
capture artifacts have a separate disk lifecycle, not an aggregate quota or a
retention guarantee. Images have one bounded normalization owner across user,
native and MCP inputs; unsupported content is an explicit error, not silent discard.

Resources do not execute repository text or start unconfigured servers.
Instructions, skills/templates and results are lower-trust data. Report resource
failures/omissions with provenance, rather than silently truncating instructions.
MCP discovery isolates server failures and refreshes at later request boundaries;
possibly effectful invocations are never transparently retried.

## Context and providers

Keep committed facts, bounded model context and human presentation distinct.
Commit an observed tool outcome with its model-projection decision; delivery
limits must not rewrite successful host effects as failed raw observations.
Reopen, fork and compaction use the recorded projection. Storage limits still
apply; failed result commits leave unknown effects, not fabricated outcomes.
Generated prose and summaries are claims, not independent verification.

Context projection, branching and compaction have one shared owner. Compaction
changes context, not history; commit complete call/result cuts atomically and
leave the old projection intact if cancelled before commit. Preserve source
coordinates for explicit retrieval without replay or implicit evidence injection.
Measure continuation quality, not merely compression.

Prepare wire declarations and durable context from the same catalogue. Admit
model/context changes atomically before issuance. Pre-output retries retain that
preparation; compaction/reset prepares again. Output ceilings are clamped to
remaining capacity; exhausting the dispatched output budget is not itself context
pressure. Capacity recovery is bounded and visible; incompatible context must
fail or change explicitly, never silently lose content.

Host owns endpoint/capability/credential resolution and prepares replacements
before publishing them. Failed preparation leaves the previous binding usable;
a running operation retains its captured binding. Logical selection, effective
model, returned identity and usage remain distinguishable. Model epochs survive
reopen, including A → B → A; routing policy is not Session correctness state.

Adapters own framing, error classification and opaque provider continuation.
Human-readable thinking is distinct from signatures/encrypted replay and returns
to providers only through the adapter-owned replay path. It counts as observed
stream output whether displayed or not. Preserve signed prefixes or report an
explicit reset/incompatibility; never reset inside an outstanding signed tool
exchange. Core must not branch on provider names.

Caching and affinity are optimizations, not authority. Affinity survives reopen
and changes on fork/clone. Warming is bounded, separately accounted and
subordinate to effect settlement; failure cannot alter Turn correctness.
Transient provider retries are bounded, cancellable and allowed only before
observed output, without repeating completed tools.

## Clients and extension boundaries

Clients use typed shared session-control operations; RPC encodes them rather
than defining another lifecycle. Input has explicit custody:

`draft → preparing → ready/queued → durably accepted | returned | withdrawn`.

Preserve original input kind, literal editor data, attachments and reservations
until acceptance or explicit handoff. Validate before queue acknowledgement;
queue acceptance is not durable Session acceptance or a full-context/RSS promise.
Expand queued resources at admission, not later execution. Steering joins a safe
boundary in its active Turn; follow-ups start after settlement. Late steering
must not silently become follow-up work in one client. Cancellation/failure
returns unaccepted inputs, including successful preparation racing with stop.
Preparation must remain cancellable/joinable without blocking client controls;
active bounded decoders need not support hard interruption.

An owning run handle requests stop and joins; an observation subscription only
observes. Dropping/timing out/aborting an owner requests stop without relinquishing
started-work custody. A service may retain that owner while clients detach.
Foreground applications may stop on genuine I/O loss, but progress saturation
is backpressure, not by itself execution failure. Bound publication/admission;
coalesce provisional progress with explicit gap/reconciliation semantics and
retain authoritative settlement/input recovery. Do not solve slow readers with
unbounded deferred replies or by blocking cancellation behind output.

Headless text publishes committed final text. Protocols distinguish acceptance,
progress, settlement and stop intent without promising exactly-once commands.
Broken output cannot promise delivery; committed inspection remains available.
Controlled connection completion closes admission, stops/joins its foreground
work and preparation, then returns unaccepted input. Healthy final pipe delivery
still requires a reader to drain or close it.

Shutdown closes admission before stopping/joining. Cancelling its wait leaves
closure and task custody intact; callers must finish it before destroying their
runtime. Async cleanup cannot be guaranteed by Drop. Unread failures remain
observable through join/shutdown; a genuine operation-task panic blocks live
owner reuse and reports unknown effects. No synchronous idle guard crosses await.

Skills/templates, tool sources, executable hooks and UI contributions are
distinct powers with explicit scope and disposal. Trusted callbacks cannot
mutate Session history or provider wire state behind their owners. Delegation
composes Sessions with explicit context, authority, parentage and foreground or
background ownership; a coordinator need not become another coding engine.

## Terminal experience

Inline native scrollback is primary; fullscreen is a viewport of the same
semantics. Prefer restrained fx-like hierarchy and progressive disclosure over
colored tool cards or a flat stream. Keep prompts, prose, activity episodes,
mutations and notices legible without relying on color. Grouping represents
source semantics; nested execution needs recorded parentage, not an invented
process tree. Admission is queued, not running or successful.

Keep the draft editable during work, including multiline editing, history,
paste/images, external editor, steering and follow-ups. Commands/pickers retain
unsent input. Budget activity and composer together using semantic items,
keeping current state and exceptions visible with honest omission/inspection.
Coalesce repetitive benign output, not consequential mutations or failures.
Details retain arguments, observations, capture paths and delivery notices.

Use one control-safe grapheme/display-width policy for rendered content;
composer byte/cursor mapping remains separate. Semantic styles survive mutable
rendering and irreversible native publication. Publish settled content once;
redraw/resize must not publish history or maintain a second committed screen.
A closed group is not necessarily final. The emulator owns native reflow; resume
publishes a bounded recent semantic tail with earlier facts inspectable.

Scrolling, resize, viewport changes and exit preserve composer/terminal custody.
I/O failure stops input/redraw and awaits owned settlement before reporting;
panic/crash cannot promise that. Physical publication is not execution authority.

## Hybrid composition

Code Mode is opt-in alongside direct calls/discovery. A confined Host VM gets
owned code, limits and a bounded bridge; Core retains Session/catalogue authority.
It is not another agent loop or a durable JavaScript runtime. Pi is the primary
model-facing authoring reference: named discoverable calls, useful input/result
descriptions, selected structured/text/image output and actionable errors.
README describes the currently supported subset; no comparative speedup is assumed.

Identify a parent by committed assistant-entry sequence and call ordinal, not a
reusable provider ID. Commit child intent before dispatch and observation before
guest consumption. Parent settlement requires all children settled; recovery
closes unknown children first without rerunning code. The guest selects what
reaches model context; raw child facts remain inspectable, not automatically
injected as fabricated messages.

On guest failure, expose a stable discoverable parent locator. Explicit later
inspection uses bounded metadata/JSON windows, preserves observed/not-dispatched/
unknown states, and grants no new effect or replay authority. Keep coordinates
rebuildable from facts without whole-history clones. Model-tool inspection must
exclude private shell context, opaque provider replay and normalized image bytes.
Capture paths may disappear; saved observation, present host state and missing
evidence are different.

Normal guest return drains transferred requests and started effects. Failure,
cancellation or budget/storage faults close admission and request stop, retaining
started futures and joining the VM. Failed observation commits block consumption
and dependent dispatch even if guest code catches an error. Guest limits, host
capture/artifacts, audit and final delivery have distinct owners; VM isolation
does not bound native process resources or guarantee capture retention.

## Qualification

Preserve distinct integrity, concurrency, cancellation and recovery protections;
remove duplicate fixtures and private-call assertions that no longer protect a
contract. Exercise changed built clients and repository gates. Source inspection,
mock/PTY checks, live coding and real-emulator/human review establish different
things. Neither passing tests nor reference similarity certifies performance,
model adherence, terminal stability or daily-driver readiness.
