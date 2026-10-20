# Headless and RPC clients

Use the built executable from scripts or a foreground JSONL controller. RPC is Ion’s protocol, not an ACP implementation.

[Start](../README.md) · [Terminal](terminal.md) · [Configuration](configuration.md) · [Tools](tools.md) · [Automation](automation.md) · [Rust embedding](embedding.md)

## CLI output and metadata

Plain CLI metadata (`sessions`, `models`, `use`, resources and MCP listings)
replaces C0/C1/DEL control characters within fields with spaces, preserving
column delimiters and ordinary Unicode. These display fields are not exact
identities: use RPC `list_sessions` for exact Session IDs and `inspect` for raw
model references and conversation facts.
Storage, configuration and machine-readable data remain unchanged; coding
answers, tool observations and inspect/export are not filtered by this policy.
Write failures for command results or startup diagnostics exit 1 after owned
cleanup rather than panic. Effects already applied before a failed confirmation—
such as a saved model selection, transcript or Session copy—are not rolled back
or automatically retried.

## Headless text and JSONL

For headless integrations, `--json` works with `run PROMPT` or `--print PROMPT`.
Headless prompts prepend UTF-8 data piped through stdin, up to 8 MiB. The
selected model's context window and Ion's encoded request bound can reject
large input even when it fits that raw input limit.
Plain text mode writes the committed final answer to stdout after a successful
turn. Tool progress and errors go to stderr; a failed turn does not print a
provisional answer as if it had completed.
Headless publication is asynchronous and bounded: a stalled stdout or stderr
pipe does not prevent SIGINT from cancelling and settling the coding operation.
A broken output stops further work and waits for settlement; durable Session
inspection remains available. Final delivery to an open pipe still requires its
recipient to read or close it.
In JSONL mode, stdout contains one JSON object per line: a `session` record with the ID and
directory, ordered `text_delta`, tool lifecycle, recovery and final records,
then a `run_end` record with `completed`, `cancelled` or `failed` status.
`model_output_observed` records generated content received without human deltas.
It carries no content and does not mean a tool was admitted or ran.
`tool_started` and `tool_finished` share a `call_id` and include a semantic
`activity` object (kind plus bounded subject when available). `tool_finished`
records `state`: `observed`, `not_dispatched` or `unknown`. Only `observed` has a
`model_projection`: `observed`, `images_unsupported` or `request_limit_exceeded`.
The latter two withhold the payload from model context, not saved inspection;
`is_error` describes the observed tool outcome, not the delivery limit. Other
states carry a diagnostic instead of an invented host observation. `turn_ended`
publishes a committed Turn closure; clients still await their operation task.
Code Mode adds `child_tool_admitted` (intent and parent call ID),
`child_tool_started` (progress), and `child_tool_finished` (committed host output,
MIME markers and `observed`/`not_dispatched`/`unknown` state). Parent identity is
`{assistant_entry, ordinal}` and each child has its own ordinal; provider call
IDs can repeat in later steps. An observed host cancellation response need not
establish that a remote server stopped its external effects.
`assistant_committed` publishes a durable assistant boundary with `turn`,
`content`, `tool_activities` and `termination`; `content` uses the library's
`Content` encoding (for example, `{"Text":"answer"}` or `{"ToolCall":{...}}`).
It replaces the current provisional response, including content that arrived
without text deltas. `steering_committed` publishes accepted steering as a
`turn` and typed `input` message, in Session order. A queued steering
acknowledgement alone does not mean that the input is durable.
`response_restarted` discards only text deltas since the last assistant commit;
it never removes previously committed assistant content or steering.
`provider_replay_rebased` means a changed request prefix caused Ion to omit
older opaque reasoning before dispatch while retaining the raw Session facts.
`provider_replay_notice` reports a provider's count and reason for dropped or
allowed-mismatch reasoning blocks.
Diagnostics stay on stderr, and failure also sets a nonzero exit status. The `final` record is
the committed assistant answer; earlier text deltas are for live display.

## RPC lifecycle and commands

`ion rpc` keeps a Session open for a subprocess client. It emits a `ready`
record, then accepts one LF-terminated JSON command per stdin line. Commands
can carry a string `id`; each response repeats it. For example, send
`{"id":"1","type":"prompt","message":"Inspect this project"}`. A successful
prompt response includes a Turn ID and confirms that the input entered the
Session. Keep reading progress records with that Turn ID until `turn_end`
reports `completed`, `cancelled` or `failed`; a response alone is not the
answer. `final` is the committed answer. Other commands are `steer`,
`follow_up`, `submit_request`, `get_request`, `clear_queue`, `abort`, `get_state`,
`inspect`, `list_sessions`, `list_turns`, `list_models`,
`list_resources`, `reload_resources`, `shell`, `compact`, `set_model`, `new_session`,
`clone_session`, `fork`, `switch_session`, `set_reasoning`, `get_background`,
`cancel_background`, `acknowledge_background` and `set_name`.
`set_name.name` must be a string; null or omission clears it. `fork.after` must
be a boolean when present and defaults to false when omitted. Wrong field types
are rejected before mutation.
`get_state.reasoning` reports the active Session's generation preference.
`prompt`, `steer` and `follow_up` accept `images` as an array of local paths (relative to the
Session's working directory) or inline `{ "mime_type": "image/png", "data": "BASE64" }`
objects. Ion validates and normalizes both before accepting the input.
Image preparation occupies one pending-input slot, not an accepted queue entry.
`get_state` reports shared control activity, including `preparing_input`. Image
preparation remains owned and joinable while controls and other queued input stay
available; active decoders cannot be forcibly interrupted. A preparation error
returns the original typed input. Steering preparation cannot attach to a later
Turn when its target ends.

`steer` targets the active coding Turn. `follow_up` queues a separate Turn behind
current work, including shell/compaction, or starts when idle. Its queued
acknowledgement means prepared Host custody, not durable Session acceptance;
`follow_up_started` supplies the committed Turn ID. `clear_queue` acknowledges withdrawal intent; separate `uncommitted_steering`
and `uncommitted_follow_up` records return originals. It does not cancel the
initial prompt. `abort` also stops initial
preparation and requests stop of active work. Stop wins queued dispatch even if
an observed result races to success; failed operations return unaccepted inputs.

For restart-safe direct **literal text** delivery, supply a separate scoped key:

```json
{"id":"wire-1","type":"submit_request","key":{"scope":"my-client","id":"job-42"},"message":"/check stays literal"}
{"id":"wire-2","type":"get_request","key":{"scope":"my-client","id":"job-42"}}
```

`submit_request` replies with `data.disposition` (`started` or `existing`) and
`data.receipt`: logical `session` UUID, `key`, `turn` and committed `end` reason.
A new `started` response means the literal input and key committed together and
Host retained execution; read through `turn_end` for settlement as with `prompt`.
Exact redelivery returns the same admission/latest status without starting work,
including while busy or after reopening. Different text under the same key is an
error. A new key requires idle, drained control; it is not queued.
`get_request` returns `data.receipt` or null without repairing history or dispatching
work. It shares the inspection reply slot. `get_background` returns the application
owner's retained job/result snapshot; `cancel_background` explicitly stops one
child; `acknowledge_background` releases one joined result's capacity. The
independent `background_changed` record is an invalidation, not a result or a
completion proof. An `end` of null does not prove liveness:
process loss leaves unfinished facts until new explicit work closes them
conservatively. Neither lookup nor redelivery resumes the lost execution.

Keys are case-sensitive nonblank `scope`/`id` strings, each at most 256 bytes with
no control characters. They are caller-supplied identities, not credentials or
namespace authorization. These two commands reject unknown fields. The keyed
message is not template-expanded and has no `images` field or local-file
preparation; use ordinary prepared input for those. Wire `id` values, pending queues and reply
custody remain volatile. If an acknowledgement is lost, reconcile using the same
key and exact text, not a new identity or assumed exactly-once tool effects.

Inputs share a process-local 32 MiB allowance for originals, prepared messages and
client metadata, plus 64 retained admission/result slots. Route/image checks apply
before acknowledgement. Accepted metadata stays reserved through settlement;
returned input stays reserved until handoff. These are custody bounds, not
context-fit or process-memory guarantees.

Output pressure alone never cancels work. `ready.publication` advertises a
32-command reply window; two control slots and one inspection slot are separate.
Credits remain held through lifecycle settlement and physical reply delivery.
Provisional output has an 8 MiB high-water mark. Overflow emits `publication_gap`:
discard incomplete progress and reconcile with `inspect` and reliable settlement.
Commands beyond reply capacity are not admitted and are counted in a coalesced
gap, not buffered indefinitely. Abort still conveys stop when its reply cannot
fit. A gap can replace individual refusal replies; reconcile outstanding requests
rather than blindly resubmitting possible effects. Keep reading stdout: healthy
final delivery still needs a reader to drain or close the pipe.
Closing stdin cancels active work and returns uncommitted steering and
follow-ups as `uncommitted_steering` and `uncommitted_follow_up` records.
Returned typed inputs can include image payloads, so clients should handle them
as their own input data. Input errors also cancel and await active work, then
return terminal records and pending inputs while stdout remains healthy before
exiting with an error. If stdout is broken, no terminal record can be promised;
reopen or inspect the Session for committed outcomes.
A Core operation-task failure reports through its terminal record (or
`operation_failed` when no Turn was known) while stdout is healthy, and ends the
connection with an error. A failed protocol record is not itself durable Turn
closure: unfinished effects remain unknown and are not retried automatically. Pending steering and
follow-ups are returned, including a starting follow-up whose input had not
entered the Session; input already accepted by the Session is never returned
as uncommitted.
Session, resource, model, shell and manual-compaction commands require an idle
operation with no pending input preparation. Send `{"id":"s","type":"shell","command":"cargo test"}` to run a direct
command. Its acknowledgement means the operation started, not that the command
was dispatched. Read through the correlated `shell_end` record for
completed/cancelled/failed status. An `outcome` with `kind: "observed"` contains
the committed native output and `is_error`; an error without an outcome makes
no effect claim. Inspect the Session after admission/storage failure.
Shell commands share their command and outcome with later model context by
default; `exclude_from_context: true` excludes both, like terminal `!!`.
Shell operations make no model request or coding Turn. They accept `abort`
and explicit queued follow-ups, but cannot be steered. EOF and connection faults still cancel and await
settlement. `get_state.operation` distinguishes `shell`, `turn` and `compact`.
`compact` acknowledges that the operation started, can be cancelled with
`abort`, and later emits `compact_end` with completed/cancelled/failed status
and whether the model-context projection changed. `set_model` saves the idle selection for that Session; `new_session` selects the current global
default, `clone_session` copies the current committed conversation into an
independent active Session, and `switch_session` restores the selected Session's model. Starting,
cloning, forking or switching Sessions refreshes project resources; use
`reload_resources` to refresh them within the current Session.
Malformed commands receive a failed response, commands over 8 MiB are
rejected, and an incomplete final line is left unexecuted with a framing
error. Stdout is reserved for protocol records, stderr for diagnostics.
Uncommitted steering is returned as a typed `input` message if a Turn ends
before the Session accepts it.
