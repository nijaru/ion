# Coding tools and context

Implemented native tools, managed processes and agents, questions, context recovery and optional composition. All effects use captured host authority.

[Start](../README.md) · [Terminal](terminal.md) · [Configuration](configuration.md) · [Tools](tools.md) · [Automation](automation.md) · [Rust embedding](embedding.md)

## Native files, discovery and shell

Tools act directly in the working directory with the host user's permissions.
There is no implicit sandbox. Settlements distinguish observed host output,
calls known not to have been dispatched, and unknown effects. Cancellation
closes unused calls as not dispatched. After process loss, a call without a
committed result remains unknown even if dispatch might not have started. The
next explicit prompt commits that recovery without rerunning the call;
reopen, fork and export preserve the distinction. `ion --continue inspect`
reads the existing log without making that repair. `exec` retains the final
64 KiB of raw bytes observed from each stream and decodes the tail as UTF-8.
`stdout_lossy` / `stderr_lossy` mark replacement decoding, including a cut through
a multibyte character; a genuine U+FFFD character does not set them.
`stdout_complete` / `stderr_complete` mean pipe EOF was observed. Omitted counts
are raw bytes, or `null` when capture is incomplete. A complete truncated or
lossy stream has a private temporary raw file at `stdout_full_path` or
`stderr_full_path` when capture succeeds; `*_full_error` reports failures.
Inspect these files instead of repeating effects, but do not assume they survive
across launches. An inherited pipe left open after output goes idle can produce
an incomplete capture with no full-output path. Capture loss does not change a
known command exit. Shared `!` shell context preserves the complete structured
host observation, including these diagnostics; `!!` remains private.
`exec` and direct user shell commands use Bash when available, then fall back to
POSIX sh, with closed stdin. They do not capture the user's login-shell environment.
Use `process` for managed background, pipe-interactive or PTY-backed programs,
including bounded terminal screen inspection. Managed subagents are separate agent
Sessions, not process-session handles. Foreground commands have no default
timeout; pass `timeout_ms` when a deadline is needed. The deadline
covers output capture even after the direct shell exits. On timeout, Ion requests
process-group stop and stops acquiring output, then awaits started capture I/O.
This is not a hard wall-clock or all-descendant termination guarantee.
Omit optional native input fields to use their defaults; explicit JSON `null`
is not a valid integer or string option. Output metadata may still contain `null`.
### Repository discovery

The coding preset includes native discovery without requiring fd, ripgrep or a
shell. These are model tools, available directly and through enabled Code Mode:

| Tool | Arguments and results |
| --- | --- |
| `ls` | Optional `path` and `limit` (default 500, maximum 1000). Lists immediate entries, including hidden names, with `name` and `type`: `file`, `dir`, `symlink` or `other`. Returned names are sorted; a work-limited subset is not a complete alphabetical listing. |
| `find` | Required `pattern`, optional `path` and `limit` (default/maximum 1000). Returns regular-file paths relative to `root`. A glob without `/`, such as `*.rs`, matches basenames at any depth; `src/**/*.rs` matches relative paths recursively. |
| `grep` | Required `pattern`, optional `path`, `glob`, `ignore_case`, `literal` and `limit` (default 100, maximum 1000). Regex is the default; `literal: true` searches exact text. Returns one item per matching line with `path`, one-based `line`, first-match zero-based `byte_offset`, and a UTF-8-safe snippet up to 1024 bytes. `truncated` marks omitted line content. |

Paths default to `.` relative to the captured working directory; absolute paths
are allowed with the host user's permissions. Results identify the canonical
`root`. For `grep`, `root_kind: "directory"` means join the relative match path
to `root`; `root_kind: "file"` means read `root` itself. Pass a match's
`byte_offset` to `read` to inspect that area, including matches in long lines.
Globs use gitignore-style syntax, with leading `#` and `!` treated as literal name
characters rather than comments or negation. Recursive scans respect `.gitignore`
and `.ignore`, include non-ignored hidden files, exclude discovered `.git` trees
and do not follow discovered symlinks. An explicit root may resolve a symlink.
Ignore rules are convenience, not a confidentiality boundary.

Discovery returns at most 64 KiB of serialized result data, inspects up to 20,000
visible walker entries, and descends at most nine levels from the selected root.
`grep` inspects at most 8 MiB per file and 32 MiB total, with a charged one-byte
growth probe; regex compilation and DFA cache each have a 1 MiB bound. Patterns,
globs and input paths are limited to 4096 UTF-8 bytes. Binary/invalid UTF-8 files,
oversized files, unrepresentable paths, ignore-file errors and filesystem failures
are reported rather than silently turned into no matches. Check `exhaustive`,
`skipped`, `omissions` and bounded diagnostics before inferring absence. Refine the
root or filter after a limit: results are a live scan, not a snapshot or stable
restart cursor. Ignore-file parsing remains library-owned; entry limits are not a
universal syscall or memory quota. Cancellation joins the filesystem worker and
cannot interrupt an in-flight native syscall.

### Read and guarded mutation

`read` uses one-based line `offset` and a line-count `limit` (default and
maximum 2000), returning complete original lines within a 64 KiB text bound.
`start_line` and `next_line` identify the recorded range; when `has_more` is true,
pass `next_line` as `offset` to continue. Oversized lines are never silently
clipped or skipped. Empty files and exactly one line past EOF return an empty
page; larger line offsets reject.

For oversized lines or saved JSON observations, use zero-based `byte_offset`
and `byte_limit` instead of line selectors. The byte limit defaults to 65536 and
accepts 4–65536 bytes. Pages preserve UTF-8; the endpoint may round backward, so
continue at the returned `next_byte`, not the requested offset plus limit.
`start_byte` identifies the requested start; starts inside a character reject.
An offset exactly at EOF returns an empty page; offsets beyond EOF reject.
Line/byte selectors cannot be mixed, and neither applies to images. Pages are
raw text fragments, not necessarily individually valid JSON.

Continue reading when a complete file is needed; a partial window is not safe
full-file replacement content. For files within its 8 MiB text-edit bound,
`read` still reads the full file to compute `base_digest`, even for a byte page;
64 KiB bounds returned text, not total file I/O. The digest covers the full
file, not that window, and can guard a later `edit` or `write`.
`edit` requires `base_digest` from a read of the complete current file; missing
or stale digests reject before replacement. `write` requires the same guard to
replace an existing file, while creation needs neither field and creates missing
parents. `unconditional: true` is the explicit opt-out for an intentional unread
replacement; it cannot be combined with `base_digest` and does not create a
read. Guarded writes do not create missing parents. A successful write returns
`new_digest`, SHA256 of the content it published; that observation does not
lease the file against later changes.

`edit` takes an `edits` array of `{old_text, new_text}` replacements against one
original file snapshot. It rejects ambiguous or overlapping matches before
writing, accepts ordinary LF or CRLF text, and preserves the file's BOM and
unaffected line endings. Native `edit` and `write` coordinate through an advisory
parent-directory lock across Ion instances/processes, keeping digest checks and
replacement together. Sibling-file mutations also serialize; waiting for the
lock is cancellable. Locking errors are reported without an unlocked fallback.
External editors and `exec` writes do not participate; this is not a filesystem
transaction.

## History, compaction and provider failure

A damaged Session file is skipped by `sessions` and `--continue`, while opening its exact path reports the
error. Reopen and catalogue/startup metadata validate facts one at a time without
retaining all raw child output. Metadata inspection also releases historical model
messages instead of retaining the full conversation. Writable Sessions still retain
model messages; full inspection, detailed terminal history, clone and fork
materialize raw history. Large records and these retained histories can require
substantial memory. There is no aggregate Session memory quota.

Ion summarizes settled history when its request
nears a known model's context window or exceeds its transport bound, and can
retry one model request after a provider reports context overflow. The raw
conversation remains inspectable; `compact` and `/compact` also trigger this
explicitly. Longer saved histories are summarized in bounded steps when one
summary request cannot fit. Summary requests treat the selected prefix as
historical data, exclude opaque replay and human thinking, and carry its images
as typed attachments when the summary model supports them. Text-only summary
models receive explicit image-omission notices instead of base64 transcript text.
The compacted model projection names the exact one-based committed entry range it
replaced. Summaries are model-generated and can be wrong; the raw facts remain
inspectable. Incomplete, empty, wrong-role or non-text summary responses are
rejected without replacing the existing context. Errors distinguish
output/context limits, content filtering and invalid response shape; unknown
provider details are not echoed.

The coding preset declares a read-only `history` tool, including under
`--read-only` and in managed coding children. A restricted application receives
it only by explicitly enabling it with `ToolSet::with_history`; it is not ambient
authority. `history` searches or lists committed model-facing Session entries
newest first, then reads one entry by its stable one-based committed entry
coordinate. Coordinates are the full physical Session sequence: excluded private
records leave gaps rather than renumbering results. Search is literal and
ASCII-case-insensitive and returns kind, Turn, tool, state and a bounded preview;
`next_before` continues through older records. Entry coordinates, not reusable
provider call IDs, are authoritative. A read takes a zero-based UTF-8 byte
`offset` and 4–16384-byte `limit`, returns a JSON text fragment with
`next_offset` and `total_bytes`, and preserves character boundaries. Fragments
need not parse as complete JSON until assembled.

A `tool_result` read returns the original observed output JSON text even when the
result was withheld from model context, plus its original
observed/not-dispatched/unknown state and error flag. Other entries return a safe
committed record projection. Historical reads are evidence, not current workspace
state, and never authorize rerunning a tool or repeating an effect. Search
examines at most 256 physical entries and normally 8 MiB of stored entry bodies
per call; a single larger record may be inspected alone and is reported as
`oversized_entry`, so scanning progresses without skipping records. A logical
shell record includes both its admission and settlement bodies, so that
exception can inspect up to 128 MiB for that pair; it is not an 8 MiB hard cap.
Thinking, provider replay, typed image bytes, context/configuration snapshots
and private `!!` shell records are excluded from both search and read. Image
attachments are reported only as metadata and are never returned as text pages.
These exclusions are structural, not a general secret scrubber for ordinary user
or tool text.
Tool results that individually exceed the route's
request bound, or return images to a text-only route, give the model a bounded
explanation naming their stable committed entry coordinate. Their observed output
remains available through `history`, Session inspection, export and tool detail,
with a notice that it was not shared. The Session's
64 MiB encoded-entry limit still applies; a failed result commit leaves the
effect unknown rather than silently discarding data or replaying the tool.
A large prompt or accumulated context can still exceed the limit when no
settled group can be summarized. The MiMo adapter classifies a zero-output
length stop near a known context window as context pressure, independent of
the configured provider name.
Transient provider failures can trigger up to two cancellable retries before
generated output, including tool fragments and opaque reasoning. Input-usage
updates and replay-transformation notices do not count as generated output.
Retry events appear in the TUI and JSONL output. A response that stops after producing partial
output is not replayed silently.
If an output-token limit cuts off identifiable tool calls, Ion records them
as skipped errors and lets the model reissue complete calls. No tool from the
truncated response runs.
When observed output use is below the request's dispatched output budget, Ion first makes one
compact-and-retry attempt if a settled history prefix is available. It drops
the incomplete attempt and reports the restart to streaming clients.
Coding requests use the catalog model's output ceiling, reduced when the
current context leaves less estimated room; there is no separate 16k app cap.
A completed response with no answer or tool call fails the Turn; it does not
save an empty assistant message that would break later provider replay.

## Managed programs

The coding preset includes `process` in run, chat and RPC, without an enabling
flag. Use `start` with a foreground shell command to keep a server or line-oriented
program alive across tool calls and Turns. Do not append `&` or daemonize it:
Ion owns the live direct child and its process group, not arbitrary detached
subsequent work. Bash/sh selection matches `exec`. The default `backend: "pipe"`
provides piped stdin/stdout/stderr; select `backend: "pty"` for a program that needs
an actual terminal. PTY programs have a fresh controlling terminal and merged
stdout/stderr, including terminal input echo. The invoked command still runs via
Bash/sh `-c`; start an interactive shell explicitly if needed. There is no
login-shell environment snapshot.

`/attach [ID]` in the terminal client takes exclusive human input of a running PTY
program in the current Session and captured directory. Without an ID it selects
the only running PTY or lists the candidates. The view uses the existing screen
projection, cursor and input modes with no local echo, and owns ordinary keys
including Ctrl-C; Ctrl-] detaches and restores the composer without stopping the
program. While attached, model `write` and `resize` are rejected; model `read`,
`screen`, `inspect` and `wait` remain available, and automatic terminal replies
stay driver-owned. Acquisition serializes with the input queue: a partially
delivered model write reports its actual prefix, queued input is rejected, and a
stale lease can neither write nor release a newer one. The lease is live-only and
bound to the Session and captured directory; it survives Turn stop but not process
exit, application shutdown or Session replacement, and grants neither secrecy nor
process lifecycle authority. Headless and RPC clients do not attach. There is no
restart attachment, login-shell snapshot, graphical terminal protocol support, or
split keyboard ownership with a question/approval dialog.

`start` returns an observed process handle, not completion. `list` and `inspect`
show retained state; bounded `wait` observes without consuming or killing it.
`write` sends literal input and reports actual delivery, including partial writes;
`eof` closes piped stdin. It is rejected for PTYs: write a literal control character
such as `"\u0004"` when the terminal's line discipline should interpret EOF.
Cancelling a wait or input action does not kill an already
started program. A start cancelled before returning its receipt stops and joins
its admitted process. `close` explicitly stops and joins while retaining output;
`acknowledge` releases a joined slot. Coding child jobs stop, join and release their
own programs before returning their answer; parent services should be started by
the parent, not left behind in an inaccessible child scope. Nonzero child exit
and lost supervision are different observations, not interchangeable failures or
permission to replay.

`read` pages raw bytes by offset: separate `stdout`/`stderr` for pipes, one
`terminal` stream for PTYs. Its rolling retention is bounded; check gaps, decoding
loss and EOF rather than assuming a page is complete history. Base64 reads preserve the selected bytes. After direct-child
exit, capture has a 100 ms total drain bound rather than following an endlessly
writing descendant. A ready final chunk/EOF is checked before closing; a known
exit still does not certify that inherited output pipes reached EOF. Capture windows
are not durable files or retention leases; only returned observations committed
by Core remain in conversation history.

PTY start accepts initial `rows`/`columns` (default 24×80; at most 200 rows,
400 columns and 20,000 cells). `screen` reads a bounded page of the current terminal
grid with cursor, dimensions, modes and a source-byte
cursor; it is not scrollback or a second transcript. `resize` changes the actual
terminal dimensions and projection, letting the OS notify its foreground job.
Terminal protocol replies are automatic and bounded; clipboard and outer-terminal
control sequences are not forwarded to your terminal. Screens are text/VT
projections, not support for graphical terminal protocols or every emulator query.
Control strings over 4 KiB disable the projection while raw capture continues;
protocol reply overload stops the program with a supervision error. An unavailable
screen does not erase raw output or justify replaying the command.

The application retains the live supervisor through model/resource changes and
Session replacement. Controls require the initiating Session and captured working
directory; another Session, a fork or a guessed ID does not grant access. Returning
to that Session in the same application can find its live programs. Reopen after
application exit cannot reconnect or restart them. Application quit, RPC EOF and
one-shot completion stop and join programs before releasing their environment;
normal one-shot runs still drain delegated agents first, but do not wait forever
for servers. This is process-local supervision, not a daemon, sandbox or guarantee
that escaped descendants have stopped. `--read-only` excludes this capability.

## Typed questions

Interactive terminal coding offers `ask_user_question` within the current Turn.
A request contains `questions`: 1–4 single-select questions, each with question
text and 2–6 `{label, description?}` options. The dialog adds **Type something.**
for custom text; do not author that label or **Other** as an option. Multiselect,
headers and rich previews are not implemented.

Use Up/Down and Enter to choose. A custom answer supports Unicode editing and
paste; Enter accepts it and Shift-Enter adds a newline. Answers advance through
the batch and submit together. Esc cancels the whole questionnaire with an
explicit cancellation observation; the model may continue without an answer.
Ctrl-C stops the Turn. The dialog preserves the composer draft and waits for an
existing picker, detail view or attached PTY to release input rather than stealing
keystrokes or revoking a terminal lease.

Answers become ordinary committed tool observations, not steering, new user
messages, approval or permission. An answered result carries an `answers` array:
question ordinals are one-based, option indices are zero-based, and selected
labels come from the original request. A live responder lease and request nonce reject
stale/duplicate replies; requests carry their Core Session/Turn/tool occurrence.
Losing the responder while a question waits stops that Turn before releasing the
waiter. Shutdown revokes requests even if a caller retains its responder handle.
Pending questions are live-only: reopening a Session does not restart a dialog or
reinterpret an old request as authorization.

Requests are bounded to 64 KiB of serialized arguments. Question text is limited
to 4096 UTF-8 bytes, labels to 120 bytes and descriptions to 1024 bytes. Custom
answers are nonempty and at most 4096 UTF-8 bytes each; the serialized answer
batch is bounded to 48 KiB. Invalid answers leave the request available for a
corrected reply. Only one batch can wait per control; concurrent asks fail
explicitly. Without a connected responder, an explicitly installed question tool
returns unavailability rather than waiting or auto-answering. The tool also
requires a Turn admitted through shared control: a genuine Core origin alone does
not supply its owning Turn's stop authority, including inside Code Mode.

Headless, RPC and managed coding children omit this capability. Rust applications
can configure the same typed Host port and responder through shared control;
terminal presentation is its first consumer, not a separate agent runtime.
Explicit tool selection applies to direct calls and Code Mode as usual.

## Coding subagents

The main coding preset includes `subagent({...})` in run, chat and RPC. For
example, `ion run 'Delegate a self-contained coding task, then review it'` needs
no enabling flag. `CodingApplication` retains this policy through reload and
binding replacement; supplied external registrations can override coding defaults.
Restricted applications still choose their own complete `Application` recipe.

The tool has explicit actions: `start`, `list`, `status`, `wait`, `cancel` and
`acknowledge`. `start` accepts a self-contained `task` of at most 64 KiB UTF-8,
optional `mode` (`foreground`, the default, or `background`), and optional
`context`. Fresh context is the default; `{"type":"fork"}` copies the settled
committed prefix before the initiating Turn, not the active conversation, and an
explicit `point` may select an earlier `before_turn` or `after_turn` boundary.
Core validates and captures that cut in the admission transaction. The child
inherits the initiating request's selected model route, including invocation-local
overrides, and its reasoning. It also inherits the same workspace, project/home
resources, native and supplied external tools (MCP for the CLI), and the
application's Code Mode setting. It does not automatically enable another
built-in subagent. Model arguments cannot choose tools, credentials, model,
workspace or arbitrary authority.

Foreground `start` joins execution and cleanup before returning the typed
child/outcome locator and request receipt. Background `start` returns only after
responsibility has transferred to the coding application's owner; it is not a
completion claim and survives parent stop, parent-Session switch and reload, but
not application shutdown. `list` and `status` inspect retained application jobs;
`wait` waits for one joined report without consuming it; `cancel` explicitly
stops one child; `acknowledge` releases one joined result's capacity without
deleting its committed facts. A cancelled wait does not stop a background child
or prevent other waiters from observing its joined report.

A joined answer is the child's actual original final visible text, retained after
full Host/Core joining of the requested completed Turn. It is not reconstructed
from storage, and execution, cleanup or storage failure clears it. Treat it as
the child's claim, not verification of its work. Raw child conversation,
thinking and intermediate tool output do not automatically enter parent context.

Managed files live under the workspace catalog's `children/`, outside automatic
`--continue` root selection; use the returned path to inspect or explicitly open
one. The same workspace is not conflict isolation, and native tools retain host
permissions. Parent stop and foreground client EOF stop/join their owned children;
background children require explicit cancel or application shutdown. Cancellation
does not roll back effects. Normal headless completion lets delegated agents
finish, then stops and joins managed programs. Interruption, RPC EOF and terminal
quit stop and join both. These are
offline lifecycle guarantees; live-model adherence, service scale and native
GUI visual acceptance remain open.

## Optional Code Mode

`ion --code-mode run 'Read the files in parallel and return a summary'` adds
`code_mode` alongside direct tools. The same flag works with chat and RPC;
`CodingApplication::with_code_mode(true)` enables it for that application's
initial setup and later binding changes. It is off by default. Its reserved tool name replaces a custom
`code_mode` registration when enabled.

The model supplies an async JavaScript **body**, for example:

```js
const results = await Promise.all(
  ['src/main.rs', 'Cargo.toml'].map(path => tools.read({path}))
);
return results.map(r => ({characters: r.value.content.length, failed: r.is_error}));
```

`tools.read(arguments)` and `tools[name](arguments)` invoke exact callable names
from the request's frozen catalogue, including deferred tools. The namespace is
finite, frozen and has no prototype or normalized aliases. Use brackets for names
such as `tools['server_lookup-item'](arguments)`. Unknown members are `undefined`;
`call` and `inspect` are ordinary tool names, not dispatcher/inspection helpers.
Callable names `then` and `toJSON` prevent Code Mode startup rather than risk
implicit dispatch during promise resolution or serialization; direct calls remain
available. The complete name manifest must fit the per-value and cumulative host
JSON budgets before any guest or child work starts; an oversized catalogue is not
silently truncated.

Calls return `{value, is_error, images}` for observed tool outcomes,
including native failures with `is_error: true`. Invalid bridge requests and
gateway faults reject instead. Catching a rejection does not undo prior effects
or establish that unknown work is safe to retry.
Global `searchTools(query, {limit})` returns bounded `{name, description}`
candidates. Options and `limit` may be omitted (default ten); a supplied limit
must be an integer 1..10. Null/nonobject options, unknown keys and invalid limits
reject before admission, without coercion or clamping. Matching uses
ASCII-insensitive name/description substrings, with exact ASCII-insensitive
names first and frozen catalogue order within each rank, without duplicates.
Global `describeTool(name)` looks up an exact, case-sensitive name in Core's
frozen catalogue and returns its full provider-neutral
`{name, description, input_schema}` ToolSpec, or explicit `null` if missing.
Both include callable deferred tools and exclude intrinsic `tool_search` and
`code_mode`. Discovery is read-only: it neither activates tools nor records child
effects, and shares existing bridge, JSON, reply, cancellation and guest budgets.
This is not full Pi API parity; typed result schemas are not yet exposed.
Select model-visible output with a JSON `return`, `text(value)` or `image(block)`:

```js
const picture = await tools.read({path: 'plot.png'});
text({description: 'Selected plot', failed: picture.is_error});
image(picture.images[0]);
```

`text` snapshots primitives as strings and objects as JSON. `image` accepts an
invocation-scoped `{type:'image', id, mime_type}` block from a tool reply; Core
resolves it to the committed image. Image bytes never pass through JavaScript.
Handles from another invocation, including saved/copied handles, are invalid;
URLs and raw image data are not accepted. This deliberately differs from Pi's
inline-image input rather than copying large base64 payloads through the VM.
The parent envelope contains `result` (null without an explicit return), ordered
`output` items (`{type:'text',text}` or `{type:'image',index}`), and call/failure/skip
counts. Image indexes refer to its separate typed image attachments. The model
route's existing image-support and request-size projection still applies.

Raw child arguments and observations remain in the Session, export and nested
Ctrl-O/tool detail, without automatic addition to model context. Explicit
selections survive ordinary script failure alongside the parent error, counters
and Session-scoped `parent` locator. Invalid image selections or a combined
output-limit failure return an error instead of the selected payload;
infrastructure faults still block dependent consumption. A later guest can
explicitly retrieve saved evidence without rerunning the failed script,
even if compaction has removed that locator from visible context:

```js
const saved = await inspectSaved({kind: 'parents'});
const parent = saved.parents[0].parent; // select the relevant saved parent
const children = await inspectSaved({kind: 'children', parent});
const page = await inspectSaved({
  kind: 'output', parent, child: 0, pointer: '/stdout_full_path'
});
return {children, capture_path: JSON.parse(page.json), original_failed: page.is_error};
```

`parents` returns up to ten settled parents with saved children, newest first,
with their locators and `child_count`. Pass `next_before` as `before` for older
pages; a null marker ends pagination. The earlier result's locator also works.
These coordinates are local to the current Session, including copied fork/clone
history; the page contains no code, arguments or raw output.

`children` returns up to ten summaries with a `next_after` marker; pass it as
`after` to continue. Names and nondispatch reasons are bounded display labels
(`name_truncated`, `reason_truncated`); key discovery omits oversized keys
(`keys_truncated`). `root_value_type` describes the saved root, not the selected
pointer. `output` selects a JSON
Pointer into saved `output.value` and returns encoded JSON text in `json`.
`offset` and `next_offset` are UTF-8 byte positions; `limit` defaults to 4096
and accepts 4–65536 bytes. Concatenate pages before parsing a larger value.
Observed results preserve `is_error`; `not_dispatched` and `unknown` have no
invented output. Only settled parents with saved children are inspectable.

Global `inspectSaved(query)` is read-only and shares the guest's bridge/reply budgets. Invalid
queries reject their promise. Page size bounds returned data, not lookup CPU:
a saved value is decoded by an owned read task outside the Session writer lock
and guest VM; cancellation stops delivery but still joins that read. It exposes
neither normalized image bytes, private direct-shell history nor provider replay.
Only the later guest's explicit selections enter model context. Saved observations
are not fresh verification, and a stored capture path does not guarantee the file
still exists. Inspect current host effects or disclose evidence that is missing.

Child intent commits before dispatch and output before guest consumption. A
normal return drains requests already transferred by the guest, even if
unawaited. Failure or cancellation closes admission and awaits started work.
A failed result commit blocks dependent consumption; interrupted children are
recorded as unknown before their parent is recovered. Reopening never reruns
the script. `Promise.all` does not roll back writes or external calls.

Each QuickJS invocation owns a separate native thread, not a slot in the host
blocking pool its tool calls need. The operation retains the VM join through
teardown; receiving a result is not join completion. Admission therefore costs
one VM thread per concurrent invocation, with native stack space beyond its
checked guest stack. There are no ambient filesystem, network, module or timer
APIs. Default limits are a cumulative 30-second runnable guest
budget, 64 bridge requests, four concurrent calls, a 64 MiB JS heap,
512 KiB stack, 64 KiB source and 1 MiB per JSON value. The callable-name manifest
and cumulative JSON reply data share an 8 MiB host-delivery budget. A separate
8 MiB selected-output limit bounds the combined JSON return, text items and
actual image attachments, not just handle sizes. Text-buffer exhaustion fails
even if caught and closes admission; Core validates the complete selection after
joining the VM and child work. A 32 MiB child-audit admission threshold stops
further dispatch; already-started outcomes still commit, so it can overshoot. Successful
guest return ends its budget, not host settlement. Idle waits for pending Core
bridge replies are excluded; guest computation, promise jobs, serialization and
reply resolution still consume the budget. Unresolved promises without pending
host work also consume it. There is no default whole-script wall timeout: tools
retain their own deadlines, and user cancellation still stops and joins work.
Guest failure diagnostics include thrown primitive strings and native Error own
string data messages (including parser errors), capped at 256 UTF-8 bytes.
Reporting does not run guest getters, coercion or Proxy traps; unsafe exceptions
retain a generic diagnostic. Cancellation and budget exhaustion take precedence.

These limits are not an OS sandbox or a process-RSS/disk quota. Native and MCP
calls retain host permissions and their own capture/timeout contracts. Full
command captures can consume disk; there is no aggregate artifact quota or
retention guarantee. No comparative latency/token benefit is established yet.
