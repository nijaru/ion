# Ion architecture

This is the chosen design for Ion, not an inventory of everything implemented.
[README.md](README.md) describes current behavior and qualification. Ion is
unreleased v0: replace obsolete APIs and development formats directly, without
migrations, deprecated facades or parallel old/new implementations.

## Product

Ion is a polished local coding agent in Rust. A user chooses a model, asks it
to inspect and change a working directory, observes its work, intervenes, and
continues the conversation after relaunch. Terminal, headless, RPC and embedded
hosts use the same coding semantics. Native tools inherit host permissions;
there is no implicit sandbox, private workspace importer or rollback promise.

Pi 1.0 is the primary reference for mature workflows and harness semantics.
fx is the primary reference for terminal presentation. Adopt useful outcomes
and ownership boundaries, not either implementation's class hierarchy, runtime
or complete feature inventory. A smaller implementation is not an improvement
if it merely transfers execution or recovery responsibility to every client.

The common-workflow target includes file/shell tools, project instructions,
models and credentials, images, skills/templates, tool discovery, custom/MCP
tools, steering/follow-ups, context management and Session exploration. Workers,
personal memory, gateways, scheduling and generic workflow authoring are outside
this scope. OAuth is an optional access workflow, never a first-use gate.

## Four concepts

1. **Session:** ordered committed conversation and execution facts. It owns
   acceptance, recovery and context history, not HTTP clients or terminal state.
2. **Active operation:** one exclusive coding Turn, direct-shell operation or
   compaction. It owns continuation and cancellation until its started work
   settles. An accepted user message starts a Turn; a model response is a step
   within it, not another user Turn.
3. **Prepared request:** a coherent model selection, bounded context and frozen
   executable capabilities. Its definitions and dispatch bindings cannot change
   while the issued calls are being handled.
4. **Host services:** provider transports/credentials, native tools, MCP and
   project resources. Clients control input and presentation, not another loop.

`ion-ai` owns neutral model/content contracts; `ion-core` owns Session, Turn,
context and abstract capabilities; `ion-host` implements and composes services;
`ion-app` owns client interaction/protocol/presentation; `ion-terminal` owns
physical terminal mechanics. Split mixed responsibilities within these owners
rather than adding a generic frontend, scheduler or projection framework.

## Coding loop and effects

Accept input atomically, prepare a request, generate, commit the assistant
response, settle its calls, and continue or end. There is no fixed step-count
cap. Model-issued calls execute in order. Opted-in composition can run bounded
child calls concurrently under the parent operation's admission and settlement
contract, without a detached task per call.

- Commit an assistant call and its resolved semantic activity metadata before
  dispatch. Commit an observed result before a dependent request consumes it.
- Commit a final answer and Turn end together. Assistant plus queued steering
  is also one atomic transition; a failed write leaves uncommitted input with
  the host. Publish authoritative observations only after successful commit.
- Distinguish an admitted call, execution start, observed outcome and operation
  settlement. Progress cannot establish success or fabricate a durable call.
- Reject malformed or truncated calls without executing them. Identifiable
  output-truncated calls can be committed with rejected results atomically;
  otherwise fail without inventing call identity. A completed empty response
  is a model error, not a successful empty answer.
- Cancellation stops new dispatch, requests stop and awaits already-started
  host work. Rendering, connection or cache-refresh failure cannot drop its
  execution future. A host returns what it observed, not an assertion that all
  descendants stopped or arbitrary external changes were undone.
- Storage failure blocks dependent work. Process loss or task panic may leave
  unknown effects. Neither authorizes automatic replay.

Steering joins the active Turn at a safe model boundary. Follow-up input stays
client-owned until accepted as a later Turn, with its own attachments. Restore
uncommitted input on cancellation/failure. Core owns shared encoded-byte
admission across steering and follow-ups, including retained host metadata.
Validate individual messages against the active agent before acknowledging
queue acceptance. Keep reservations with uncommitted input through queue
transfer and submission wait; release on Session acceptance or explicit
editor/client handoff. Connection completion disables admission before its
final steering drain. Preserve literal editor input and image-note association
on refusal/recovery; expand queued resources at admission, not later dispatch.
Queue acceptance is not Session acceptance, and a stop request is not operation
completion. Queue limits do not promise process-RSS or full model-context fit.

## Session and recovery

Use one typed append-only SQLite history, one writer lock and atomic event
batches. Validate a candidate state before writing; publish derived state only
after commit. Turn state and indexes derive from entries, not another mutable
lifecycle record. Keep SQLite durability and locking explicit.

Opening/inspection is passive. A committed call without a result has an unknown
effect, even if dispatch might not have started. Before later input or direct
shell authority, commit interrupted-call recovery and close the interrupted
Turn. A failed recovery grants no execution authority. Resume continues the
conversation; it does not resume an external action automatically.

Direct shell commits the command and context-sharing choice before granting
host authority, then holds Session exclusivity through observed-result commit.
The permit owns that identity and metadata; hosts cannot resupply them at commit.
Keep observed output raw, including errors and cancellation. A missing result is
an unknown external effect in passive inspection, not observed termination.
The next explicit coding or shell admission closes an interrupted shell and
admits new input atomically, without replay. Failed admission/recovery grants no
host authority; failed result commit leaves the command unknown and inspectable.
Share command and outcome with model context for `!`; keep both out for `!!`,
including unknown interruptions. These are direct coding-shell facts, not Turns.

Clone/fork copies a valid committed prefix and context boundaries into an
independent Session. It does not snapshot or
roll back the shared working directory. An unfinished Turn cannot be a settled
fork boundary. New empty Sessions do not displace useful recent conversations.

There is no general task graph, physical-attempt ledger, receipt protocol,
second coding loop or replay cursor. Add new facts for a concrete coding contract,
not because a durable framework offers them.

## Capabilities and trust

A tool source publishes registrations containing a definition, semantic
presentation metadata and a bound executor. Freeze those registrations for the
request. Do not freeze only a host object and later rediscover the tool through
its mutable name registry. Native registrations bind an operation; MCP
registrations bind the selected connection and original remote name. A remote
server's implementation cannot be frozen locally; an unavailable captured
connection produces an error, not implicit replacement or effect replay.

Callable capabilities and the model's declared subset are different. Direct
tools are declared normally; discovery selects deferred registrations for a
later request. Restore an activated definition only if it remains identical.
Refresh and overrides replace future registrations, never issued calls.
Exposure is not a sandbox or permission policy. The Turn still owns execution
sequencing and durable publication; invoking an executor alone is not a durable
coding operation.

Derive the wire declarations and durable context from the same prepared
catalogue. Admit effective-model and context changes in one Session transaction
before issuance; a failed admission publishes neither change. Pre-output retry
retains that preparation, while compaction or replay reset prepares again.
Elide identical consecutive snapshots. Retain historical semantic metadata so replay
does not reinterpret old actions through today's tool names. Persist no live
executor, credential or connection handle.

Validate untrusted arguments/content at host boundaries. Exact edits validate
all replacements against one original snapshot before writing, preserving
unmatched bytes and ordinary BOM/line endings. Respect effective write
permissions. Bounded command output keeps diagnostic tails, identifies omitted
or incomplete capture and supplies private complete-capture paths when available.
Artifacts are inspectable host output, not guaranteed durable Session storage.

Resource discovery does not execute repository text or start unconfigured
servers. Instructions, skills/templates and tool results are lower-trust data.
MCP discovery isolates failing servers, refreshes at later request boundaries,
preserves original names and server error identity, and never transparently
retries a possibly effectful invocation. Images enter through one bounded,
validated normalization owner for user, native and MCP inputs; unsupported
content is an explicit error, never silently discarded.

## Context and providers

Keep three views distinct: raw committed history, bounded model context and
human conversation/activity. Commit the observed tool outcome and its model
projection decision together. A route's payload/image limit must not replace
observed output with an error in raw history or classify successful work as
failed. Reopen, fork and compaction use the recorded model projection; human
inspection retains the observed output and delivery notice. Storage limits
still apply, and failed commits leave unknown effects rather than invented
outcomes. Compaction changes model context, not raw history.
It commits atomically, respects complete call/result cuts, uses bounded settled
prefixes when needed, and leaves the old projection intact if cancelled before
commit. Measure successful continuation, not merely summary compression.

A prepared request uses the selected model's output ceiling clamped to remaining
context. Filling that dispatched budget is output exhaustion, not context
pressure. Capacity recovery must be bounded and visible; an incompatible
history must fail or undergo an explicit context change, never silently lose
content. Retry a transient provider failure only before stream output, within
bounded cancellable waits, without repeating completed effects.

Host model setup owns catalog capabilities, endpoint resolution and credentials.
Use matching environment keys automatically, masked entry when needed, and
explicit custom endpoints. A failed saved login does not silently select another
identity. Prepare Session/model/resource replacements before publishing them;
failed preparation leaves the old binding usable. A running operation retains
its captured binding.

Retain logical selection and effective execution identity, including usage and
provider-returned IDs. Routing remains direct-only; a virtual router is not a
requirement. Effective-model changes advance durable opaque-replay epochs,
including A → B → A and reopen. Provider-scoped signed reasoning is not answer
text. Preserve its exact supported continuation and prefix or report an explicit
reset/incompatibility; never rebase inside an outstanding signed tool exchange.
Provider adapters own wire validity, SSE framing and error classification.

Caching/affinity are optimizations, not truth. Session affinity survives reopen
and is fresh on clone/fork; it is transport metadata, not prompt content. Warming
is bounded, subordinate to effect settlement and separately accounted. Failed
warming cannot alter Turn correctness. Idle warming, speculative compaction and
router policy require their own demonstrated consumers and lifetimes.

## Terminal experience

Polish means legible work and stable interaction, not maximal density. Prefer
fx's whitespace and activity-tree hierarchy over colored per-tool cards or a
uniform flat stream of dots. Narrative, user input, activity groups, commands,
mutations and notices must remain distinguishable without relying on color.

- User prompts have a clear boundary; assistant prose remains readable prose.
  Related silent tool steps form an activity episode, separated by meaningful
  narrative/input boundaries. Tree connectors represent that grouping, not an
  invented operating-system process tree or fictitious parent/child effects.
- A restrained root dot summarizes an episode; indented branches identify its
  actions and subjects. Queued, running, completed, failed, cancelled, rejected
  and unknown work have distinguishable labels. Do not show all admitted calls
  as running. Nested composition uses recorded parentage, not guessed labels.
- Coalesce repetitive successful observations, not consequential mutations or
  exceptions. Compact output makes current work and recent failure intelligible;
  full current detail retains arguments, results, capture paths and notices.
  Glyphs alone are not evidence of correct grouping or execution state.
- Budget the current activity and composer together. Overflow is selected from
  semantic items, not blindly sliced rendered strings. Keep current state and
  exceptions visible with an honest omission/inspection affordance. There is no
  promise that every action fits a small viewport.
- Keep the draft editable during work, with coherent multiline movement,
  history, paste/images, external editor and steering/follow-up behavior. Commands
  and pickers preserve unsent input. Idle help/resources are content, not a
  single-line busy preview.
- Use one control-safe grapheme/display-width policy for transcript/chrome/detail.
  Composer byte-to-cursor mapping is a separate input contract. Style belongs to
  semantic presentation roles, not escape strings mixed into untrusted text.

Inline native scrollback is the default. Publish settled content explicitly;
mutable redraw/resize must not publish history or maintain a second virtual
committed screen. A group being closed to new children does not prove finality.
The terminal emulator owns native reflow. Resume publishes a bounded recent
semantic tail; earlier facts remain inspectable.

Fullscreen is another viewport policy over the same conversation, not another
runtime. Current-conversation detail/pickers may be modal in either policy.
Scrolling away from the tail, resizing, policy switches and exit must preserve
composer/terminal ownership. I/O failure stops input/redraw, cancels and awaits
active settlement before reporting failure. Panic/crash cannot promise that.

## Clients and extension boundaries

Share host assembly and replacement operations across clients; do not reproduce
model/tool/resource setup in executable-private code. Headless text emits the
committed final answer, not provisional streaming fragments. JSONL/RPC records
correlate acceptance, progress and settlement without pretending command IDs are
an exactly-once protocol. A connection owns and joins its active task; EOF or
I/O failure cancels and waits, and follow-ups start only after settlement.
Broken output cannot promise delivery; durable inspection remains available.

Skills/templates, tool sources, executable hooks and optional UI contributions
are distinct powers. Additional extension registrations need explicit scope
ownership and disposal; no generic reactive framework is assumed. Callbacks
cannot mutate Session facts or provider wire state behind their owners.

## Hybrid composition

Code Mode is opt-in alongside direct calls and discovery. A confined host
executor receives owned code, resource limits and a bounded request channel;
Session and catalogue authority remain in the coding operation. QuickJS is
an execution service, not a second coding loop or durable JavaScript runtime.
No comparative performance benefit is established.

Identify a parent by committed assistant-entry sequence and call ordinal, not a
reusable provider ID. Commit child intent with frozen definition/activity before
dispatch, then observed output before guest consumption. Raw child facts enter
human inspection and recovery, never ordinary model messages or fabricated
assistant calls. A parent cannot commit its result while children are pending.
Recovery closes unknown children before the parent, without rerunning code.

A successful guest return closes its sender; drain already-transferred requests
and started effects before the parent settles. Failure, cancellation or a
budget/storage fault closes further admission and requests stop. Retain started
futures and join the worker through settlement. A failed observation commit
blocks consumption and dependent dispatch, even if the guest catches errors.
Neither promise rejection nor guest isolation establishes rollback, physical
termination or remote quiescence.

Keep guest heap/stack/time, call count/concurrency, argument/reply data, raw audit,
host capture/artifacts and final model delivery independently owned. Guest
limits do not cap native process resources or full-capture disk usage. The first
path bounds guest data and audit admission; native capture remains tool-owned,
without an aggregate artifact quota. Artifact quotas and typed advisory RESULT
contracts remain open work, not claims about the current runtime.

## Qualification

Preserve distinct data-integrity, replay, recovery, concurrency and cancellation
protections; remove duplicate fixtures and obsolete implementation assertions.
Exercise changed built headless/terminal/control entry points and repository
gates. Source review, deterministic VT/PTY checks, live-provider coding and human
terminal review are different evidence. Do not claim emulator-native reflow,
comparative coding performance or broad usability from mock-provider tests.
