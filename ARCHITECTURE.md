# Ion architecture

**Chosen implementation target, 2026-09-26.** This file states the
coding-agent contracts. The product has not yet been validated end to end.
[README.md](README.md) describes
what the current executable can do. Ion is unreleased v0, so obsolete runtime
representations can be replaced directly.

## Product

Ion is a Rust agent for coding in a local working directory. A user can start
it in a terminal, choose and authenticate a model, ask it to inspect and
change files, run native commands, observe results and continue a saved
session after relaunch. The same behavior is available headlessly and through
a library host. The first tool set is read, edit, write and shell. Shell can
handle search and listing until a dedicated tool shows a benefit.

A usable first version includes project instructions, model discovery and
selection, automatic environment API keys, interactive login with a suitable
OAuth route, and honest resume. Ion needs to complete real coding tasks end to
end; scripted model and storage tests alone do not establish that outcome.
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
until a final response, stop or limit. A tool result is available to the
model before a dependent request. Tool failure can be a result the model
reasons about; transport, storage and unrecoverable dispatch errors surface
to the client. Neither client infers task success from the model's prose alone.

## Session and recovery

A Session owns the working-directory identity and one typed, append-only
history in SQLite. It admits at most one executing coding Turn. One writer
serializes submissions and append transactions. Turn acceptance and its user message commit
together; record the nonsecret context needed to interpret the history without
duplicating a full request manifest. Save a complete assistant message
containing tool calls durably before executing those calls, then
save each observed result durably before another model request depends on it. A final
answer and explicit Turn-end reason commit together. Cancellation, provider
failure and limits also have explicit end reasons. Turn state is derived from
entries; any index is rebuildable. Partial model text and streaming tool
output may be shown live, but committed Session facts are the authority on
reopen.

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

## Context, tools and trust

Project instructions, the current request and useful Session history form a
bounded model input. Raw history and the model-context view are distinct;
context changes must leave the recorded conversation inspectable. Keep tool
calls and results intelligible together. If a request is too large, show an
actionable capacity error. When actual sessions need compaction, evaluate a
summary policy on representative tasks. No particular checkpoint or tail
algorithm is fixed by the baseline. If a later model cannot encode stored
history faithfully, report that or make an explicit context change rather
than silently dropping content.

Default file and shell tools act on the live working directory with the host
user's permissions. There is no implicit sandbox, VM, importer or private
workspace registry. File edits reject ambiguous matches. Writes report
creation or replacement; commands report exit status, launch/transport
failure and truncation. Bound runtime, payload and output sizes at usable
values. Do not claim stronger effect or isolation guarantees than a tool
implements. Approval or sandboxing is a separate opt-in product decision,
not a prerequisite for native coding.

Project files and tool output are lower-trust data. Credentials belong to the
host and stay out of model-visible context and Session history.

## Model setup and clients

The catalog lists models with working transports and maintained capability
metadata. A user can discover, select and switch models without asserting
capacity values for known entries. Custom compatible endpoints remain
possible with the metadata their adapters actually need. Resolve an ambient
key automatically for its matching provider. Interactive login and logout
operate on host-owned credentials; an OAuth route uses its own valid flow and
matching wire adapter, with expiry handled at request time. Do not silently
switch identities after a saved login fails. The exact initial provider list
is an implementation recommendation to verify, not a product requirement.

The TUI shows prompt, streaming response, tool calls/results and errors while
keeping terminal input and restoration reliable. Headless mode exposes the
same loop without terminal dependencies, with useful output and exit status.
Resume selects an existing Session rather than starting an unrelated hidden
conversation.

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
