# Ion architecture

**Design target under review, 2026-09-26.** This file states the proposed
coding-agent contracts for implementation. The target has not yet been
validated as a complete working product. [README.md](README.md) describes
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
owns the Session, model/tool continuation, ordered conversation and bounded
context. The executable composes models, credentials, project instructions,
host tools and clients. Terminal rendering and input never become a second
agent loop.

```text
user input -> model stream -> final answer
                   | tool calls
                   v
               host tools -> results -> next model request
```

One Turn starts from a user message. The engine builds model input, streams a
response, dispatches complete tool calls in order, records their results and
continues until a final response, stop or limit. A tool result is available
to the model before a dependent request. Tool failure can be a result the
model reasons about; transport, storage and unrecoverable dispatch errors
surface to the client. Neither client infers task success from the model's
prose alone.

## Session and recovery

A Session records an ordered conversation and its working directory for
resume across launches. Save a complete assistant message containing tool
calls before executing those calls, then save each observed result before
another model request depends on it. Partial model text and streaming tool
output are live progress until a complete record exists. On reopen, an
unmatched call is visibly incomplete or has an unknown effect; Ion does not
silently execute it again. Opening and reading a Session are passive. A later
user request can inspect the checkout and decide how to continue.

Cancellation prevents new dispatch and requests that active host work stop.
A command's direct exit, timeout or signal result is recorded as observed;
remote or detached effects may continue. A failure to persist history needed
for the next step stops that step. These rules give truthful recovery without
promising all-descendant quiescence or atomic filesystem changes. A separate
pre-effect marker, logical/physical attempt ledger, immutable request
manifest, receipt graph and parallel outcome staging are not required by the
initial contract. Introduce them only for a reproducible failure or measured
concurrency need.

The storage implementation may use SQLite or JSONL. It has one authority for
conversation order and must make reopen and incomplete-call handling
unambiguous. It need not preserve R1's schema or APIs. In unreleased v0, do
not keep two production runtimes or compatibility facades.

## Context, tools and trust

Project instructions, the current request and useful Session history form a
bounded model input. Keep tool calls and results intelligible together. If a
request is too large, show an actionable capacity error. When actual sessions
need compaction, evaluate a summary policy on representative tasks and keep
history available for inspection. No particular checkpoint or tail algorithm
is fixed by the baseline.

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
