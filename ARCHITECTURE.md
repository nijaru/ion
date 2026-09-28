# Ion architecture

**Chosen implementation target, under source-level qualification as of
2026-09-28.** This file states the coding-agent contracts. The core loop has
completed end-to-end macOS tasks on the live routes named in
[README.md](README.md); provider and tool edge-case correctness is still being
checked against current Pi implementation and focused failures. Those tasks
do not establish Pi-level correctness across the supported path.
[README.md](README.md) describes what the current executable can do. Ion is
unreleased v0, so obsolete runtime representations can be replaced directly.

## Product

Ion is a Rust agent for coding in a local working directory. A user can start
it in a terminal, choose and authenticate a model, ask it to inspect and
change files, run native commands, observe results and continue a saved
session after relaunch. The same behavior is available headlessly and through
a library host. The first tool set is read, edit, write and shell. Shell can
handle search and listing until a dedicated tool shows a benefit.

A usable first version includes project instructions, model discovery and
selection, automatic environment API keys, optional masked key entry, and
honest resume. Ion needs to complete real coding tasks end to end; scripted
model and storage tests alone do not establish that outcome.

Pi is a direct reference for the small interactions that make a coding agent
usable: editing and steering prompts, inspecting tools, finding sessions,
switching models and managing context. Ion adopts those user outcomes through
its own Session and client design rather than copying every Pi command or
its plugin runtime.

Workers, personal memory, gateways, schedules and general workflow authoring
are outside this initial scope.

## Owners and loop

`ion-ai` owns provider-neutral messages, streams and usage facts. Provider
adapters own wire encoding and provider-specific constraints. `ion-core`
owns one committed Session log and the coding Turn loop: ordered conversation,
continuation, recovery and bounded model context. The executable composes
models, credentials, project instructions, host tools and clients. Terminal
rendering and input never become a second agent loop.

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
A complete assistant response without tool calls is also a settled cut,
including when queued steering keeps the Turn active. A steering message
alone is not a settled assistant batch.
If a later model cannot encode stored history faithfully, report that or make
an explicit context change rather than silently dropping content.
An output-limit stop with observed output usage below the requested ceiling
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
tail and state what was omitted. If a descendant keeps an output pipe open
after the direct command exits, retain bytes already observed and finish
after output becomes idle; mark an unfinished capture rather than reporting
it as complete. Exact text edits must handle ordinary BOM and
line-ending conventions without silently changing unrelated text. An atomic
replacement of an existing file must still respect its effective write
permission; directory rename access alone does not make it an editable target.
Support optional command timeouts and cancellation, and bound payload and
output sizes at usable values. Do not claim stronger effect or isolation
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
Provider adapters accept valid terminal responses and reject incomplete ones,
including stream truncation. Classify context overflow from a provider signal
or a narrow documented response pattern; a generic HTTP status is not enough
to rewrite model context. Preserve a bounded provider error reason when an
Anthropic SSE error arrives after HTTP success; unknown future Anthropic
event types do not invalidate an otherwise complete message. Stream framing
accepts SSE line endings across arbitrary transport chunk boundaries.
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
Headless mode exposes the
same loop without terminal dependencies, with useful text output and exit
status. A JSONL output mode emits one session identity, ordered progress
events and a terminal invocation result on stdout; diagnostics stay on stderr.
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
