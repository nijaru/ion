# Ion architecture

Accepted product direction, revised 2026-09-26. [README.md](README.md) describes
the current implementation and its limits. This file defines the coding-agent
contracts that guide changes. A contract here is a product or correctness need,
not a checklist of features borrowed from other agents.

## Product

Ion is a Rust coding agent for a terminal and for headless/library use. Its
baseline is one agent working in a local repository: understand the request and
project instructions, inspect files, edit or create files, run native commands,
observe the results, and continue until it can report verified work. The TUI and
headless host drive the same agent loop and session state. macOS and Linux run
their own host binaries and toolchains.

The ordinary launch path should be `ion` in the working directory, with a
saved model/provider choice and Session state in an application-owned location.
Headless use should accept a prompt and produce usable text or machine-readable
events and a meaningful exit status. Advanced flags may override these defaults;
ordinary coding must not require the user to create a registry, assert model
capacity numbers, or select an execution implementation on every run.

The first useful tool set is small: read, exact text edit, file write/create,
and a native shell command. Search and listing may use the shell until a
dedicated tool proves better in representative tasks. Tools should work on
ordinary repositories, including larger source files, ignored build inputs and
Git commands. The agent must see bounded output and be able to inspect full
output when it was truncated. Tool responses distinguish command exit status,
execution errors and output truncation.

Provider neutrality means adapters for real wire APIs and honest per-model
capabilities, not one synthetic least-common-denominator request. The model,
tools, instructions and relevant Session context used for a request must be
observable and reproducible enough to diagnose failures. Project instructions
are loaded from applicable files at the working directory and reported to the
user; file contents and tool output remain lower-trust data. Credentials stay
with the host, outside model-visible context.

Workers, generic workflows, schedules, gateways, personal memory, automatic
prompt optimization and an everything-is-a-plugin framework are outside the
initial coding baseline. Add a feature only for a demonstrated task or
integration need and measure its effect on coding outcomes, latency and cost.

## Owners

- `ion-ai` owns provider-neutral request/response content, streams, usage and
  provider facts. It knows nothing about Sessions, storage or command policy.
- `ion-core` owns the coding Turn, Session history, context projection, tool
  invocation/result pairing, dispatch and recovery. It provides narrow model
  and tool interfaces and has no TUI dependency.
- The executable composes providers, credentials, project context, host tools
  and client policy. `ion-terminal` owns terminal input, rendering and
  restoration; it does not implement a second agent loop.

Keep one semantic owner for each rule. Ion is unreleased v0: replace obsolete
representations instead of carrying migration facades, duplicated runtimes or
unused public surfaces. A Turn is the continuation owner, not a generic task
graph or a public programmable workflow.

## Agent loop and Session

```text
user input → assemble context → model stream → final answer
                               ↘ tool calls → tool results ↗
```

One user request owns a Turn with a bounded number of model steps and tool
calls. The default loop executes tool calls in assistant order. It appends
complete model-visible tool results before the next model request; a provider
tool-call ID never substitutes for Ion's logical invocation identity.
Provider and tool streams are provisional UI progress until the final result
is known. The transcript records what the agent said, requested and observed.
The application can reopen the Session and continue a later user request.

The Session persists enough state to reconstruct committed conversation
history and avoid silently repeating an uncertain external action. A logical
model step or tool invocation may have more than one physical attempt, but
earlier attempts remain visible. Complete responses and tool results enter
history at most once for their logical call. A new attempt is allowed only
when the prior attempt is known not to have started or its backend explicitly
proves a safe replay. An interrupted command with unknown effects is not
automatically rerun. Show the uncertainty to the user and allow a later
request to inspect and repair the workspace.

Opening or inspecting a Session does not silently run a model or a tool.
Explicit user submission or resume starts work. Cancellation prevents new
tool admission, requests stop for active work and records what is actually
known. It does not imply that a remote request, detached child or delegated
effect stopped. Closing the host waits for work it still owns where feasible
and preserves unresolved evidence; it must not convert uncertainty into
success, failure or proof of nonexecution. A failed write to durable Session
state must not permit another effect based on uncommitted state.

History and live progress are separate. Clients can take a bounded snapshot
then observe committed changes without losing the handoff. Large output is
bounded at collection and kept in an inspectable artifact only when the host
can store it. A missing artifact reports unavailability, never an empty result
or a fabricated success. The exact storage schema, receipt types and batching
are implementation details unless another consumer demonstrates a need.

## Model context

The model sees current project instructions, the active user request and a
bounded useful conversation history. Tool call/result pairs remain balanced.
When the history no longer fits, compact older completed exchanges into an
advisory checkpoint and retain a recent verbatim tail. Keep the current user
request exact. A checkpoint is context data, not an assistant answer, a new
user command or proof that a claimed tool effect happened. Its semantic role
must survive request assembly even if a provider API represents it with a
conversation role. Preserve original history for inspection.

Request-size decisions must use the selected provider's actual constraints
where known. A conservative byte ceiling may protect memory but is not a
tokenizer or proof of model acceptance. Capacity errors should state the
actual limit/estimate and let the user continue after a relevant change.
Compaction quality and prompt/tool wording are evaluated on real tasks;
string matching against one model's bad final answer is not a general
completion validator.

## Host tools and authority

The default shell tool starts a native host command in the current working
directory with the host user's permissions. It sees the live checkout and
ordinary host toolchains. It is not a sandbox. File tools have the same
trust boundary; they should make exact edits predictable and report failures
without implying that a whole repository mutation was atomic. A user may
choose an available sandbox or per-command approval policy, but neither is a
prerequisite for the default coding path.

The host bounds command/output resources and captures the direct child's
exit status. On timeout or cancellation it signals the command's process
group or equivalent platform mechanism and joins what it directly owns.
This is best-effort cancellation. A descendant that changes its process
group/session, a remote action or a preexisting broker may continue. Report
that limitation accurately and never describe a direct-child exit as
all-descendant quiescence. Normal command completion means the direct command
exited; it does not certify the external world is idle.

Tool preparation validates the exact model arguments before an effect. If
approval is enabled, approval refers to the exact action about to run and
cannot be inferred from ordinary transcript text. File edit should reject an
ambiguous old-text match or a stale expected base rather than silently apply
to another location. File write/create should state whether it created or
overwrote a path. Tool failures return useful bounded evidence so the model
can recover within the same Turn. No tool may claim a capability it does not
actually enforce.

The default host command does not require a persistent workspace claim or a
private copy/importer. Its output and mutation scope may be unknown; record
that honestly. A future isolated backend must describe its actual filesystem,
network, process and import semantics as a separate opt-in execution mode and
qualify them with real tests. Do not impose its limitations on ordinary host
execution.

## Clients and qualification

The TUI shows the prompt, streamed model text, tool calls, bounded live tool
output, final results, errors and whether work is awaiting user input. It
keeps the editor usable during output, handles multiline paste and restores
terminal state on exit and panic. The headless path offers the same model,
context, tool and cancellation behavior without terminal dependencies.

Qualification requires actual repository coding tasks on macOS and Linux:
small and large files, discovery, edit/create, native tests/builds, Git use,
reopen/continue and a context boundary. Verify resulting files and commands
outside the agent. Exercise both TUI and headless paths, local and public
provider routes where credentials are available, plus deterministic tests for
known crash/cancellation/storage/provider failure boundaries. Scripted green
tests establish invariants, not coding effectiveness. State untested routes
and limits plainly in README.
