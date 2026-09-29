# Ion architecture

This file states the chosen coding-agent contracts. [README.md](README.md)
describes implemented and validated behavior. Ion is unreleased v0, so
obsolete runtime representations can be replaced directly.

## Product

Ion is a Rust agent for coding in a local working directory. A user can start
it in a terminal, choose and authenticate a model, ask it to inspect and
change files, run native commands, observe results and continue a saved
session after relaunch. The same behavior is available headlessly and through
a library host. The first tool set is read, edit, write and shell. Shell can
handle search and listing until a dedicated tool shows a benefit.

A usable coding agent includes project instructions, model discovery and
selection, automatic environment API keys, optional masked key entry, and
honest resume. The broader product target includes the common workflows of a
Pi-level coding agent: image input, reusable skills and prompt templates,
custom tools and extensions, earlier-point conversation exploration, and
long-lived programmatic control. Ion needs to complete real coding tasks end
to end through these surfaces; scripted model and storage tests alone do not
establish that outcome.

Pi is a direct source reference for the small interactions and failure cases
that make a coding agent usable: editing and steering prompts, inspecting
tools, finding sessions, switching models and managing context. Ion adopts
those user outcomes through its own Session and client design rather than
copying every Pi command or its TypeScript plugin runtime. The same coding
loop must be usable from terminal, headless and library clients;
client-specific rendering or input cannot own model/tool semantics.

Workers, personal memory, gateways, schedules and general workflow authoring
are outside this initial scope.

## Owners and loop

Ion's core is the production Rust architecture that carries every coding
workflow above: provider-neutral messages, one durable Session and Turn owner,
host composition, and thin clients. Pi's Pico and durable-harness work informs
the explicit ownership, passive-open and recovery boundaries. Ion's coding
Turn and typed log are the chosen Rust expression of those lessons.

`ion-ai` owns provider-neutral messages, streams and usage facts. Provider
adapters own wire encoding and provider-specific constraints. `ion-core`
owns one committed Session log and the coding Turn loop: ordered conversation,
continuation, recovery and bounded model context. A public host composition
layer selects models, credentials, project resources, tools and Sessions;
terminal, one-shot headless and sustained-control clients use that layer.
Terminal rendering and input never become a second agent loop.
For an active interactive or sustained client, the host owns one binding of
Session, selected model, resources and agent. New, clone, fork, switch, model
selection and resource reload update that binding through the same operations
for both clients. A replacement is prepared before it becomes visible; a
failed preparation leaves the previous binding usable. Client input queues,
rendering and protocol records remain client-owned.

```text
user input -> model stream -> final answer
                   | tool calls
                   v
               host tools -> results -> next model request
```

One Turn starts from an accepted user message. Each model request uses one
coherent selection of model, tools, instructions and context. The loop
builds model input, streams a response,
dispatches complete tool calls in order, records their results and continues
until a final response, cancellation or an explicit failure. There is no
fixed model-step cap on an active Turn. A tool result is available to the
model before a dependent request. Tool failure can be a result the model
reasons about; transport, storage and unrecoverable dispatch errors surface
to the client. Neither client infers task success from the model's prose alone.
An invented tool name or invalid arguments should reach a visible tool error
when the call can be represented safely. Preserve malformed streamed argument
text as a failed call, and never dispatch it or a truncated call. When a
response reaches its output-token limit with identifiable tool calls, commit
the incomplete assistant attempt and synthetic failure results in one Session
transaction, then let the model reissue complete calls. An unrepresentable
partial call ends the Turn without dispatch.
A completed response with no nonblank text and no calls ends the Turn as a
model error; it is not committed as a successful empty assistant answer.

## Session and recovery

A Session owns the working-directory identity and one typed, append-only
history in SQLite. It admits at most one executing coding Turn. One writer
holds an advisory lock for that Session, serializes submissions and append
transactions, then explicitly releases the lock after the store closes. A
briefly inherited file description cannot keep the Session locked after its
writer exits. Turn acceptance and its user message commit
together; record the nonsecret context needed to interpret the history without
duplicating a full request manifest. Save a complete assistant message
containing tool calls durably before executing those calls, then
save each observed result durably before another model request depends on it. A final
answer and explicit Turn-end reason commit together. Cancellation, provider
failure and limits also have explicit end reasons. Turn state is derived from
entries; any index is rebuildable. Partial model text and streaming tool
output may be shown live, but committed Session facts are the authority on
reopen.
Steering remains in a host-owned inbox until it commits to the Session. When
it arrives beside a completed assistant response, the assistant and steering
commit together. A failed write leaves the uncommitted prompt available to
the host for restoration.
Recorded assistant attempts retain their provider termination reason, so a
truncated call that was rejected is distinguishable from a complete call.
User-run shell commands are separate Session facts recorded after their
observed result. A command may be visible only in the transcript or also
projected as a user message for later model requests; the latter choice is
persisted and respected after reopen and compaction. Direct user shell work
uses the same live-directory executor as the model's shell tool and cannot
interleave with an active coding Turn.

An unmatched call after process loss has an unknown effect, including when
dispatch may not have begun. An accepted Turn without an end entry is
interrupted. Opening and reading a Session are passive; the read-only view
can project these facts without changing history. Before a later user message
enters model context, a writer closes unresolved calls with visible
interruption results and ends the interrupted Turn before accepting the new
input. It never
silently reruns a call. Session resume means continuing the conversation
across launches, not automatically resuming an interrupted external effect.

Cancellation prevents new dispatch and requests that active host work stop.
A command's direct exit, timeout or signal result is recorded as observed;
remote or detached effects may continue. A failure to persist history needed
for the next step stops that step. These rules give truthful recovery without
promising all-descendant quiescence or atomic filesystem changes. The typed
history is an explicit durable continuation, not a generic task framework.
A separate pre-effect marker, physical attempt ledger, immutable request
manifest, receipt graph and parallel outcome staging need a demonstrated
recovery or concurrency benefit before becoming part of this coding contract.

SQLite owns Session metadata and ordered entries; one transaction publishes a
related event batch. Its entries are the only authority for conversation and
Turn state. A derived index may be rebuilt. This avoids inventing a second
JSONL publication and recovery protocol for the first product. In unreleased
v0, do not keep two production runtimes or compatibility facades.
Discovery of recent Sessions must tolerate one damaged or partially created
file; opening that exact path must still report its error. A failed tool
result retains its error identity through persistence and provider replay.
An idle new Session with no accepted user Turn must not displace the latest
conversation or clutter the normal Session list.

## Context, tools and trust

Project instructions, the current request and useful Session history form a
bounded model input. Raw history and the model-context view are distinct;
context changes must leave the recorded conversation inspectable. Keep tool
calls and results intelligible together. If a request is too large, show an
actionable capacity error without hiding or dropping history. Daily use also
needs an explicit, recoverable way to reduce model context. A summary must
commit as a Session fact, retain the raw transcript, and keep complete
tool-call/result groups on either side of the cut. Evaluate its policy on
representative tasks. If the saved prefix is too large for one summary
request, summarize bounded settled prefixes in sequence; do not require a
larger model merely to reopen long work. No particular checkpoint or tail
algorithm is fixed.
If a recent call/result batch exceeds the preferred tail size, first use an older
settled cut when available so its assistant call and results remain exact in
the next request. Summarize that batch only when there is no earlier safe cut;
never divide a call from its results. The request-capacity check still applies
to the retained context.
If compaction is cancelled before its Session write, discard the generated
summary and leave the previous context projection in place.
Use the selected model's output ceiling for coding requests, clamped to the
estimated remaining context on each request. Do not impose a smaller fixed
app-wide generation cap.
Project `AGENTS.md` files inherit from ancestor directories. In a linked
worktree nested inside its main checkout, the worktree root's copy shadows
the main checkout's copy of the same file; other ancestor instructions still
apply.
A complete assistant response without tool calls is also a settled cut,
including when queued steering keeps the Turn active. A steering message
alone is not a settled assistant batch.
If a later model cannot encode stored history faithfully, report that or make
an explicit context change rather than silently dropping content.
An output-limit stop with observed output usage below the model's ceiling
may reflect context pressure. Try one compact-and-retry before committing
that incomplete response or dispatching its calls, and notify streaming
clients that any partial text from the attempt was replaced. A normal
output-limit stop does not trigger this recovery.

Default file and shell tools act on the live working directory with the host
user's permissions. There is no implicit sandbox, VM, importer or private
workspace registry. Shell commands use Bash where available, then POSIX sh.
File edits reject ambiguous matches. Writes report
creation or replacement; commands report exit status, launch/transport
failure and truncation. When command output is bounded, retain the diagnostic
tail and state what was omitted. For a complete capture whose displayed tail
is truncated, retain the observed full stream in a private temporary file and
return its path so the agent can inspect earlier output without rerunning the
command. The file is a host artifact, not a Session authority or a durable
resume promise. If a descendant keeps an output pipe open
after the direct command exits, retain bytes already observed and finish
after output becomes idle. Cancellation bounds this drain even if output
remains active; mark an unfinished capture rather than reporting it as
complete. Exact text edits must handle ordinary BOM and
line-ending conventions without silently changing unrelated text. An atomic
replacement of an existing file must still respect its effective write
permission; directory rename access alone does not make it an editable target.
For bounded text files, `read.base_digest` hashes the same full-file bytes
used for the returned page and can be passed directly to `edit` as its
change guard.
The read tool can return a workspace image as a typed tool result. Image
normalization and bounds have one provider-neutral owner shared with user
attachments. Session history retains the normalized bytes for replay; terminal
and inspection views show a marker rather than base64. Provider adapters
translate this result without changing its Session identity: Anthropic can
carry image blocks inside a tool result, while Chat Completions needs text in
the tool message followed by an image-bearing user message. If a selected
route cannot represent an image, report the limitation rather than silently
discarding it. Count tool images under the same request-size and context
bounds as user images.
Support optional command timeouts and cancellation, and bound payload and
output sizes at usable values. A raw input or provider response admitted by
the host must fit its encoded Session entry; model context can still be a
separate, actionable limit. Do not claim stronger effect or isolation
guarantees than a tool implements. Approval or sandboxing is a separate opt-in
product decision, not a prerequisite for native coding.

Project files and tool output are lower-trust data. Credentials belong to the
host and stay out of model-visible context and Session history.

## Model setup and clients

The catalog lists models with working transports and maintained capability
metadata. A user can discover, select and switch models without asserting
capacity values for known entries. Custom compatible endpoints remain
possible with the metadata their adapters actually need. Resolve an ambient
key automatically for its matching provider. Masked key entry and logout
operate on host-owned credentials. Do not silently
switch identities after a saved login fails. The exact initial provider list
is an implementation recommendation to verify, not a product requirement.

The host resolves a model identity to its endpoint, wire behavior and
credential source. Each Turn records the selected nonsecret identity. A
custom route is validated by the transport's endpoint and anonymous-loopback
rules when selected; model setup must not maintain a second URL policy. A
resumed Session restores that model when its route is available and reports a
missing route clearly; a global default applies to new Sessions. A custom
route stays resolvable after another model becomes the default. Explicit
per-invocation selection overrides the resumed choice for that invocation.
Model and provider transport remain stable while a Turn runs.
An explicit model switch starts a new model-facing replay epoch. Preserve raw
assistant history, but omit opaque replay from earlier model epochs in later
requests, including after switching back. Do not turn private reasoning into
assistant text or alter tool-call/result pairs. Same-model tool continuation
retains its replay. Anthropic signed thinking needs its own adapter policy.
An idle model switch is recorded in its Session so explicit reopen restores
it. Creating a fresh Session from the TUI or sustained-control client resolves
the current global default again; switching to an existing Session restores
that Session's selection.
Session replacement in an interactive or sustained client prepares the selected
model, access, project resources and agent before publishing the new client
binding. A failed switch leaves the previous Session and selection active.
Starting, cloning, forking or switching Sessions reloads applicable project
resources at that boundary; explicit reload remains available without a
Session change. A running Turn keeps its captured binding until settlement.
Provider adapters accept valid terminal responses and reject incomplete ones,
including stream truncation. Classify context overflow from a provider signal
or a narrow documented response pattern; a generic HTTP status is not enough
to rewrite model context. Preserve a bounded provider error reason when an
Anthropic SSE error arrives after HTTP success; unknown future Anthropic
event types do not invalidate an otherwise complete message. Stream framing
accepts SSE line endings across arbitrary transport chunk boundaries.
When a route emits reasoning that must accompany assistant history during tool
use, its adapter retains that continuation as provider-scoped opaque replay in
the committed assistant message and re-encodes it only for a compatible route.
DeepSeek and MiMo Chat Completions carry their exact streamed
`reasoning_content` string on every later assistant message when tools are
offered. This material is not answer text or a tool argument. Within one
model-facing replay epoch, a route that cannot replay a recorded form reports
incompatibility before sending the next request; it must not silently strip
it. The OpenRouter Chat Completions route
retains ordered `reasoning_details` when returned, reconstructs streamed text
and summary fragments, and replays the structured blocks rather than a plain
reasoning alias. If a response supplies only plain `reasoning`, replay that
string. This route is available to cataloged and custom OpenRouter models;
a custom model is qualified by a live tool turn, not merely by accepting the
wire setting. Preserve provider tool-call IDs in the encoded history: signed
tool continuations can bind the signature to the original call. Anthropic
Messages stores the provider's complete ordered assistant content array as
opaque replay beside neutral answer/tool content. The adapter checks that
visible replay blocks still agree with the committed answer and tool calls,
then sends the original array, including empty signed and redacted thinking
blocks. For models that bind signed blocks to their
request prefix, the adapter must preserve the provider-facing system, tools and
earlier messages that produced each retained block. Client-side compaction
and resource changes can change that prefix. When the prefix cannot be
preserved at a new Turn, the agent commits a replay epoch change before
dispatch and rebuilds context without prior opaque replay. Raw Session
history remains intact and the reset survives reopen. Do not rebase inside a
signed assistant tool continuation: preserve its prefix, or report that the
continuation cannot fit. A successful response on an older account does not
establish that replay is valid for every account.
For current native Claude models, request adaptive thinking with the documented
prefix check set to `error` and report known provider
`input_transformations` for dropped or mismatch-allowed reasoning. Keep
provider beta controls off custom Messages-compatible endpoints unless their
contract is qualified.
Transient request recovery, when enabled, must be
bounded, visible, cancellable and must not repeat a completed tool effect.
Coalesce streamed tool calls by their call index: later repeated or changed
metadata must not corrupt the first call identity, while argument fragments
continue to accumulate and distinct completed calls retain unique IDs.
Usage sent on later stream events can be partial; retain previously observed
fields when a provider omits them. Anthropic may send several `message_delta`
events; keep their cumulative usage and require a consistent terminal reason
before `message_stop`.
Retry only before any streamed event is observed; a partial response is
reported as incomplete rather than silently replayed. A valid provider retry
delay takes precedence over local backoff, up to a bounded automatic wait;
longer requested waits are surfaced as errors rather than held open.

The TUI shows prompt, streaming response, tool calls/results and errors while
keeping terminal input and restoration reliable. Terminal input is parsed
incrementally across read boundaries; a lone Escape waits briefly for a
possible key sequence, with a longer wait over SSH. Bracketed paste and
enabled mouse/keyboard sequences remain semantic events rather than draft
text. The input reader releases the tty before a synchronous login prompt.
An external editor receives only the unsent draft in a private temporary
file while Ion releases terminal ownership; failure leaves the original
draft intact. Copy uses committed assistant text. A readable export derives
from committed Session entries, marks image content without inlining its
bytes, and creates a new user-selected file without replacing existing data.
Headless mode exposes the same loop without terminal dependencies. Text mode
writes only the committed
final answer to stdout after a successful Turn; provisional streamed text can
be discarded or replaced and must not masquerade as the answer. Failures have
a nonzero exit status. A JSONL output mode emits one session identity, ordered
progress events and a terminal invocation result on stdout; diagnostics stay
on stderr.
Tool lifecycle events carry call IDs so a host can correlate them. This is a
view of the same loop, not a second Session authority or a copy of Pi's event
schema.
Start a fresh Session by default, explicitly continue recent work or select
an earlier Session by human-visible identity. Users can name sessions and
clone the current conversation into a new Session to explore an alternate
approach without erasing the source history. A clone copies committed
conversation facts and context boundaries; its future history is independent.
Both Sessions still act on the same live working directory, so cloning is not
a filesystem snapshot. An unfinished Turn retains its interruption and
unknown-effect semantics in the clone. Exact
session paths remain available to scripts. The TUI preserves draft input
during a running Turn, distinguishes steering from follow-up work, and makes
full tool results inspectable even when the default view is compact. Active
directory, Session, model and known context pressure are visible.

## Common-workflow expansion

A user request may contain ordered text and image parts. The host validates
image type, size and selected-model capability before accepting it; a Session
commits the accepted content as one user message. Provider adapters encode
images for their own wire format. On resume, switch or compaction, the context
builder either preserves content selected for replay or reports an explicit
incompatibility. It must not replace an image with an unannounced placeholder.
Image generation is a separate capability.
An image-only user request may carry an empty text part in the Session; a
multimodal provider request omits that empty text block while preserving the
image and any nonempty text parts in order.
File input is decoded at the host boundary, oriented and bounded for
inline transport, then stored as normalized image bytes rather than a path to
a mutable source file. A resize note identifies the dimensions sent to the
model. The request byte bound still includes encoded image data; context-token
estimation treats image payloads separately from text and yields to observed
provider usage when available. Inspection and terminal history show an image
marker rather than the stored base64.
Deserializing typed image content must recheck the declared MIME, source bytes
and decode/resource bounds before that content can enter a Session.
The terminal clipboard adapter reads file lists before images to avoid
mistaking a copied file's icon for image input. Raw clipboard pixels enter
through the same host normalization boundary as files. A queued prompt owns
its attachments when queued; later pasted images cannot attach to an earlier
follow-up. Steering is a typed user message with the same image validation as
Turn input. A busy-turn submit carries its text and images to the next model
step; an explicit follow-up remains a later Turn.

The host discovers applicable project instructions, Agent Skills and prompt
templates. Skill summaries belong in the model's available instructions;
complete skill content is read when invoked. Templates expand before user
input commits to a Turn. Resource discovery, precedence, errors and trust
belong to one host owner and are consistent across interactive and
programmatic clients. A public Rust host interface also composes the model
catalog, credentials, tools and Session selection; embedding should not
require reproducing executable-private setup.
Repository skills and templates are lower-trust local text, like project
instructions. Discovery does not execute them. Invalid resources produce
diagnostics; an explicit command expands a template or skill before Turn
acceptance, and a model can choose to read an advertised skill. The live
working-directory permission boundary still applies.

An embedded host can supply custom tools through `ToolHost`; host composition
adds them to the local tools, with a same-name custom tool deliberately
replacing only that built-in. External model-callable tools use named MCP
servers over stdio rather than a second private executable protocol. Server
startup is explicit, never triggered merely by opening a repository. A
server crash or cancellation produces an honest tool error; it cannot alter
committed Session facts outside ordinary tool results. External extensions
beyond text tools may return MCP image blocks; the host validates declared
MIME, decodes and normalizes them through the same image owner as local read,
and hands the bounded typed result to the coding Turn. Unsupported media
receive an explicit tool error. Extensions beyond MCP tools need a documented
lifecycle for registering commands,
observing relevant Turn events, and using client UI capabilities when present.
Extension callbacks cannot mutate committed Session entries or provider wire
state behind their owners. The external mechanism need not execute Pi's
TypeScript modules. One-shot JSONL remains a progress stream; a long-lived
bidirectional control mode must correlate requests, distinguish acceptance
from settlement, and expose Session, model and resource operations through the
same host.
The control process reads one LF-delimited JSON command at a time and reserves
stdout for JSON records. A prompt response confirms the user Turn was committed
to its Session; it does not claim model completion. A separate terminal record
reports the Turn outcome. Command IDs correlate responses, while Turn IDs
correlate progress and settlement. A running Turn keeps its selected Session
and model fixed. Cancellation requests the current Turn to stop; it never
claims to undo tool effects. Closing input cancels active work and waits for
its terminal record before exiting. Clients must keep draining stdout.
RPC image input accepts local paths or inline MIME and base64 data. Both enter
the same host normalization and selected-model capability check before Turn
acceptance, so a controller can submit images without sharing Ion's filesystem.
The command-size bound includes the encoded bytes; invalid or oversized inline
content receives a command error without committing a Turn.
RPC steering is input for the active Turn; a follow-up is retained by the
control process for a later Turn and is not a Session fact until accepted.
Normalize its images and expand its resource command when queued. A queued
follow-up starts after the current Turn settles, even if that Turn was
cancelled, unless the client clears the queue. Report its later Turn ID and
return uncommitted queued input when the control process closes.

Earlier-point exploration selects a committed Turn boundary without erasing
later history. Forking before a selected user Turn makes that input editable
again; forking after its settled end continues from its result. Each fork is a
new Session containing the valid prefix, including applicable compaction facts.
An unfinished Turn cannot be an after-Turn point. The source and fork still
act on the same live filesystem; a Session fork is never a worktree snapshot.
If related alternatives later need in-Session switching or branch-local
extension state, introduce an active branch projection then, rather than
duplicating that state in the current linear Session.

## Qualification

Exercise the built headless executable and a real terminal on coding tasks:
inspect, edit/create, run a native command, verify the changed files, then
relaunch and continue. Check each platform and provider route Ion claims to
support and state remaining limits in README. Use deterministic tests for
specific crash, cancellation, storage, provider and terminal boundaries that
the implementation changes. Run the repository's required format, Clippy,
tests and offline smoke gates after the last code edit. More elaborate test
matrices and comparison benchmarks may guide improvements; they are not
independent first-version requirements.
