# Ion

Ion is an unreleased Rust coding agent for a local working directory. It has
one coding loop for terminal chat, headless prompts and Rust library hosts. The
agent can read, edit and write files and run a native shell command. Sessions
are saved in SQLite and continue across launches. Ion is usable for short
coding tasks; its session and model controls are still simpler than Pi's.

## Start

Build with the checked-in Rust 1.98.0 toolchain:

```sh
cargo build --locked -p ion
target/debug/ion models
```

Use `target/debug/ion` in the commands below, or install the binary on your
`PATH` with `cargo install --locked --path crates/ion-app`.

Ion picks a catalog model automatically when its provider key is present in
`DEEPSEEK_API_KEY`, `XIAOMI_API_KEY`, `OPENROUTER_API_KEY`, `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`. The default preference starts with DeepSeek Flash, then
MiMo Flash, then DeepSeek Flash through OpenRouter. To select one explicitly,
run `ion use PROVIDER MODEL` with the exact ID shown by `ion models`.
`ion login PROVIDER` accepts a key at a masked terminal prompt when no
environment key is available. `ion auth` shows which credential source is
active, and `ion logout PROVIDER` removes a saved key. Environment keys take
precedence.
With the existing Pi FNOX profile, for example:

```sh
fnox -c ~/.config/fnox/config.toml --profile pi exec -- target/debug/ion --provider openrouter --model deepseek/deepseek-v4.1-flash
```

From the project directory:

```sh
ion                        # terminal chat
ion run 'Inspect and fix the failing test'
ion -p 'Summarize the changes'
ion --json run 'Inspect and fix the failing test' > events.jsonl
ion rpc                    # persistent JSONL control on stdin/stdout
git diff | ion -p 'Review this change'
ion --continue             # reopen the latest session in this directory
ion sessions               # list saved sessions and their IDs
ion --session ID inspect   # committed history for a selected session as JSON
ion --continue clone       # copy the latest conversation to a new session
ion --continue turns       # list Turn numbers and prompt previews
ion --continue fork 2      # new session before Turn 2, preserving the source
ion --continue fork 2 --after # new session after settled Turn 2
ion --continue compact     # summarize old context; retain the raw Session
```

`--cwd PATH` chooses a working directory. By default, a run starts a new
session; `--continue` reopens the most recently active one in that directory.
`--session ID` selects a listed session, and `--session PATH` can create or
open an exact SQLite path for scripts. Opening and quitting an empty chat does
not displace the latest conversation in `--continue` or `sessions`. An
existing session uses its recorded directory, and an explicit `--cwd` must
match it. Headless runs print the
session ID to stderr. In the TUI, `/new`, `/clone`, `/fork`, `/fork-after TURN`, `/resume`, `/name`, `/session`,
`/model` and `/compact` manage the conversation; `/login PROVIDER` and `/logout PROVIDER`
manage saved keys. The TUI model picker searches catalog and configured
custom routes. A resumed session restores its model; `ion use` sets the
default for new sessions.

Terminal chat is inline-first. Settled transcript rows are appended once to
native terminal scrollback; Ion keeps only the active composer/progress region
mutable. Resuming or switching to a saved Session bootstraps at most the latest
six turns into scrollback and labels omitted earlier history as retained; the
complete Session remains available through inspect/export and tool detail.
Publication uses available terminal rows before scrolling. Ordinary redraw and
resize do not publish live rows; terminal-native reflow remains emulator-owned.
The active operation and cancellation status remain visible alongside steering,
queue and detail-close notices. If terminal rendering fails during a Turn, shell
or compaction, Ion requests cancellation and awaits the operation before returning
the error. Inspect the
Session for saved outcomes when the terminal can no longer display them.
During long Turns, the inline region keeps queued/running/exception counts,
the latest exception and a rooted preview of current work. Execution-start
progress changes only the corresponding committed call; later admitted calls
remain queued. Overflow selects whole actions, favoring running work rather
than a flattened tail. Individual rows may be omitted; Ctrl-O retains the full
current conversation. Missing saved results are unknown, not still running.
After settled history is published, an expanded live band shrinks back to the
rows the active composer/status actually need. Related tool calls are rendered
as semantic activity groups with a restrained tree (`•`, `├`, `└`) instead of
raw tool-call/result protocol rows. Semantic emphasis and terminal-palette
colors distinguish active work, mutations and exceptions without colored
background cards; labels still carry the execution state. Long action rows
wrap under their tree branch; compact output lines are shortened to fit. Styles survive native
publication as well as live/fullscreen rendering. Repeated successful observation
work can coalesce, while edits, writes, commands and exceptional outcomes remain
explicit.

Assistant responses render CommonMark headings, emphasis, lists, quotes, inline
code and code blocks. Code whitespace is retained (tabs use four spaces), with
wrapping at display columns. Links show their label and destination; images
show alt text and a destination, without fetching. HTML remains literal text;
links never open automatically, and unsupported/opaque URL schemes are hidden
in the formatted view. User prompts and tool output remain literal. Ctrl-O
source inspection, saved Session facts, copy and export retain original Markdown.
Tables, syntax highlighting and other Markdown extensions are not implemented.

Successful native edits record a unified patch from the text read and the
replacement written, alongside both digests. The patch capture is limited to
64 KiB and explicitly marked when truncated; it is not a later filesystem diff.
Compact output previews up to eight patch rows with addition/removal styles;
long lines and additional rows are omitted with an ellipsis. Ctrl-O or `/tool N`
shows the full recorded capture, including truncation, without interpreting its
contents as Markdown. Write-file diffs are not implemented.

`/settings` shows the current tool-output presentation; `/settings compact`
(the default) or `/settings expanded` changes it without changing model behavior.
Compact shows the first three read-content lines and the last three lines of
both command streams (four for direct shell), with explicit omission notices.
Expanded shows all recorded read/command output and edit capture, and separates
successful observations instead of coalescing them. Neither mode fetches omitted
file ranges or capture artifacts; capture truncation stays visible. Tool text is
literal and control-safe, not Markdown. The choice lasts for this terminal only
and applies to fullscreen and future inline publication. Existing native history
is not rewritten or republished; the bounded inline progress view stays compact.
Ctrl-O always retains source inspection. Thinking visibility is not yet implemented.

Inline remains the default, but persistent fullscreen is also available with
`--tui-mode fullscreen`; use `/tui inline` or `/tui fullscreen` to switch
inside chat. Fullscreen owns the transcript viewport and scrolling while using
the same Session, agent loop and semantic transcript projection. File/model/
session pickers and Ctrl-O conversation detail use alternate-screen views in either
mode. Working-directory, Session, model and context metadata are no longer
permanent footer rows; `/session` exposes Session/context detail on demand.
Cloning copies committed conversation and context into a new session with
independent future turns. Both sessions still use the same live working
directory; cloning does not copy or restore files.
`/fork` opens a searchable Turn picker and restores the selected user input
to the editor in a new Session; `/fork TURN` selects directly. `/fork-after
TURN` continues after that Turn's recorded end. CLI `fork` prints the new
Session ID. The source retains all later history. These operations copy
conversation facts, not working files, and an unfinished Turn cannot be an
after-Turn point. RPC clients can use `list_turns` and `fork` with `turn` and
optional `after: true`.
This unreleased branch uses Session format 10; earlier development Session
files are not reopened. Tool activity classification used by the transcript is
stored with each assistant tool-call batch, so resumed history is not
reinterpreted through the currently installed tool catalog.
For headless integrations, `--json` works with `run PROMPT` or `--print PROMPT`.
Headless prompts prepend UTF-8 data piped through stdin, up to 8 MiB. The
selected model's context window and Ion's encoded request bound can reject
large input even when it fits that raw input limit.
Plain text mode writes the committed final answer to stdout after a successful
turn. Tool progress and errors go to stderr; a failed turn does not print a
provisional answer as if it had completed.
In JSONL mode, stdout contains one JSON object per line: a `session` record with the ID and
directory, ordered `text_delta`, tool lifecycle, recovery and final records,
then a `run_end` record with `completed`, `cancelled` or `failed` status.
`tool_started` and `tool_finished` share a `call_id` and include a semantic
`activity` object (kind plus bounded subject when available). `tool_finished`
contains the observed output and `model_projection`: `observed`,
`images_unsupported` or `request_limit_exceeded`. The latter two withhold the
payload from model context, not from saved inspection; `is_error` describes the
actual tool outcome, not the delivery limit.
Code Mode adds `child_tool_admitted` (intent and parent call ID),
`child_tool_started` (progress), and `child_tool_finished` (committed host output,
MIME markers and `observed`/`not_dispatched`/`unknown` state). Parent identity is
`{assistant_entry, ordinal}` and each child has its own ordinal; provider call
IDs can repeat in later steps. An observed host cancellation response need not
establish that a remote server stopped its external effects.
`tool_rejected` reports a call that was never dispatched because the model
response was truncated.
`assistant_committed` publishes a durable assistant boundary with `turn`,
`content`, `tool_activities` and `termination`; `content` uses the library's
`Content` encoding (for example, `{"Text":"answer"}` or `{"ToolCall":{...}}).
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

`ion rpc` keeps a Session open for a subprocess client. It emits a `ready`
record, then accepts one LF-terminated JSON command per stdin line. Commands
can carry a string `id`; each response repeats it. For example, send
`{"id":"1","type":"prompt","message":"Inspect this project"}`. A successful
prompt response includes a Turn ID and confirms that the input entered the
Session. Keep reading progress records with that Turn ID until `turn_end`
reports `completed`, `cancelled` or `failed`; a response alone is not the
answer. `final` is the committed answer. Other commands are `steer`,
`follow_up`, `clear_queue`, `abort`, `get_state`, `inspect`, `list_sessions`, `list_turns`, `list_models`,
`list_resources`, `reload_resources`, `compact`, `set_model`, `new_session`,
`clone_session`, `fork`, `switch_session` and `set_name`.
`prompt`, `steer` and `follow_up` accept `images` as an array of local paths (relative to the
Session's working directory) or inline `{ "mime_type": "image/png", "data": "BASE64" }`
objects. Ion validates and normalizes both before accepting the input.
`steer` queues typed input for the active Turn. `follow_up` queues a separate
Turn while one is active; its acknowledgement means only that Ion holds the
prepared input. A later `follow_up_started` record gives its committed Turn ID.
`clear_queue` returns uncommitted steering and follow-ups; `abort` alone leaves
follow-ups queued. Steering and follow-ups share a process-local 32 MiB
allowance for encoded messages and retained correlation IDs. Each input must
also satisfy the active route's message-size and image-input limits before Ion
acknowledges it. Accepted input keeps its reservation until Session acceptance
or return to the editor/client, including while a follow-up waits to start.
This is a queued-input bound, not a process-memory or context-fit guarantee.
Closing stdin cancels active work and returns pending
follow-ups as `uncommitted_follow_up` records. Returned typed inputs can include
image payloads, so clients should handle them as their own input data.
Input/output errors also cancel and await active work before the process exits
with an error. If stdout is broken, no terminal record can be promised; reopen
or inspect the Session for committed outcomes. An operation panic terminates
the connection with an error rather than leaving it permanently busy;
uncommitted effects remain unknown and are not retried automatically.
Session, resource, model and manual-compaction changes require an idle Turn.
`compact` acknowledges that the operation started, can be cancelled with
`abort`, and later emits `compact_end` with completed/cancelled/failed status
and whether the model-context projection changed. `steer` and `follow_up`
remain valid only for a coding Turn. `set_model` saves the idle selection for that Session; `new_session` selects the current global
default, `clone_session` copies the current committed conversation into an
independent active Session, and `switch_session` restores the selected Session's model. Starting,
cloning, forking or switching Sessions refreshes project resources; use
`reload_resources` to refresh them within the current Session.
Malformed commands receive a failed response, commands over 8 MiB are
rejected, and an incomplete final line is left unexecuted with a framing
error. Stdout is reserved for protocol records, stderr for diagnostics.
Uncommitted steering is returned as a typed `input` message if a Turn ends
before the Session accepts it.
While a turn runs, the editor remains available: Enter steers the next model
step, Alt-Enter queues a separate follow-up turn, Alt-Up returns the most
recent queued follow-up to the editor, and Ctrl-C cancels. The terminal uses the
same shared queued-input allowance and route checks as RPC. Rejected input
remains in the editor with its attachments; recovered steering retains image
notes with their corresponding images. Queued resource commands expand before
admission, so later resource changes do not rewrite an accepted follow-up. Up and Down browse
earlier prompts when the cursor reaches the first or last editor line. Type
`@` to pick a project file, or use Tab after a partial `@path`; the picker
inserts a path reference for the model to read, not the file's contents.
Ctrl-O opens the whole conversation, including current work and provisional
assistant text, in a full-screen detail view. Left/Right browse earlier/newer
pages (32 messages, calls or shell records); Up/Down and Page-Up/Page-Down
scroll. Results retain their full stored content, including capture paths.
`/tools` lists calls, `/tool` opens the latest and `/tool N` selects one. Esc or Ctrl-O
closes details and restores the selected chat renderer. Input
that has not reached the model returns to the editor if the turn fails or is
cancelled.
Ctrl-G edits the current draft in `$VISUAL`, then `$EDITOR`, falling back to
`vi`; Ion keeps the original draft if the editor fails. Ctrl-X or `/copy`
copies the last committed assistant answer to the system clipboard when one
is available. On remote or displayless terminals, Ion sends an OSC 52
clipboard request, whose support depends on the terminal. `/export PATH`
saves a readable transcript to a new file; `ion --continue export` prints it
to stdout, and `ion --continue export PATH` saves it. Export includes prompts,
shell commands and tool output, so review it before sharing. Image inputs are
shown as markers rather than inline bytes; an existing target file is not
overwritten.
In the terminal, `!command` runs a shell command in the live working
directory and shares the command and its outcome with later model context.
`!!command` runs it without sharing the command or outcome with the model.
Both commands remain visible in the saved Session. The terminal shows bounded
stdout/stderr previews, observed exit/signal and cancellation/timeout status, capture notices
and the `!!` sharing choice; Ctrl-O inspects the full stored result.
Before dispatch, the Session commits the command and sharing choice, then holds
an exclusive direct-shell permit through observed-result commit. Ctrl-C requests
cancellation; a command that started can still have external effects. If Ion is
killed or the result cannot be committed, reopen, inspection and export show the
command with an unknown external effect, not an invented exit status. The next
explicit coding or shell admission closes that interruption atomically without
rerunning it. `!!` interruptions remain excluded from model context.

Ion loads `AGENTS.md` instructions found along the working directory's
ancestor path. A nested linked worktree's copy shadows the main checkout's
copy of the same file. `ion use` also accepts a custom model with
`--endpoint URL --wire chat-completions` or `--wire anthropic-messages`.
`URL` may be a compatible API base such as `http://desktop:8080/v1` or the
complete `http://desktop:8080/v1/chat/completions` request URL. Ion appends
the standard wire path only when the URL is a base.
For a custom OpenRouter model, use `--wire openrouter-chat`. This route retains
plain reasoning or ordered structured `reasoning_details` across tool calls;
the generic `chat-completions` route does not assume that contract. Qualify a
new model with a tool-using turn before relying on it for coding.
When switching models between Turns, Ion keeps the saved transcript and tool
results but omits opaque reasoning from earlier model epochs in later model
requests. Switching back does not revive those older blocks. Model requests
now carry an explicit logical selection, effective physical provider/model and
route reason. Current direct routes use the same logical and effective model;
assistant, compaction and cache-warm usage facts retain the effective execution
identity and any provider-returned model identifier. Effective physical model
changes advance replay epochs independently of logical selection, so a future
virtual route cannot switch A → B → A and accidentally revive stale opaque
reasoning. Ion does not yet provide a virtual-model registry or router.
OpenRouter requests include a persisted opaque conversation ID for best-effort
provider/cache affinity, independent of opening messages that resource reload or
compaction may change. Reopen retains it; clone/fork gets a fresh ID. Other
provider wires do not inherit OpenRouter's `session_id` field. Affinity does
not guarantee cache hits or pin an unavailable provider.
The catalog includes current Claude Fable 5.1, Opus 5.5 and Sonnet 5.5 on the
native Anthropic Messages route. Its signed thinking is retained across tool
and later Turn continuation. For reusable coding requests, these cataloged
native routes opt into Anthropic prompt caching and retain cache-read/cache-write
token counts separately from total input usage. Compatible tool-loadout changes
are encoded as native inline tool additions/removals/redefinitions so the
initial top-level tool prefix and signed-thinking prefix can remain stable;
instruction changes deliberately fall back to the latest leading context.
When a prefix cannot be preserved safely, Ion records the existing one-time
reasoning reset rather than altering signed history.

During a long active tool batch, current priced native Anthropic routes also
use economical streaming cache warming: shortly before the verified cache
lifetime expires, Ion may replay the exact last model request with a one-token
output ceiling when the expected avoided cache miss is at least $0.05. The
timer starts from that request's dispatch time, stops when the tool batch or
context advances, and is capped to one hour of active work. Refresh usage is
stored as a cache_warm Session accounting fact but never enters model context
or normal transcript/export output. Idle cache warming is not implemented.
Ion avoids compaction during a signed tool continuation; if its prefix changes
or cannot fit, the Turn fails without repeating a tool effect.
For a llama.cpp server whose model emits unreplayable reasoning, use
`--wire llama-cpp-no-thinking` to disable it on each request. Custom HTTP or
HTTPS endpoints can run without a key. Pass `--api-key-env NAME` or run
`ion login PROVIDER` when the endpoint needs one. An HTTP endpoint sends any
configured key in cleartext; use HTTPS when the endpoint offers it.

Attach JPEG, PNG, GIF or WebP files with `ion --image PATH run "PROMPT"` or
`ion --image PATH chat`; repeat `--image` for several images. In chat,
`/image PATH` attaches a file to the next prompt. Ctrl-V reads the clipboard
on the host running Ion: copied files enter as paths, copied image pixels
attach to the prompt, and otherwise text is pasted. Enter steers attached
images during a running Turn; Alt-Enter queues a separate follow-up. A terminal's
ordinary text paste still works. Relative paths resolve in
the Session's working directory. Ion decodes and checks the file, applies
image orientation and resizes large images before accepting the Turn. Source
files are limited to 32 MiB, and each inline image to 5 MiB within the
current 8 MiB request bound. A resize note gives the model the sent dimensions.
Image bytes are stored in the Session so follow-up requests can still see
them after the source file changes; `ion inspect` shows an image marker
instead of printing base64. A custom endpoint needs `ion use ... --images`
to declare that its model accepts image input.
The model's `read` tool can also open a workspace image and return it as a
typed attachment. It uses the same normalization and size bounds as a user
attachment. Tool images remain in Session history for replay; text-only model
routes receive an explicit tool error instead of an unseen image.

Ion also discovers Agent Skills from `~/.agents/skills/`,
`~/.config/ion/skills/` (or `$XDG_CONFIG_HOME/ion/skills/`) and project
`.agents/skills/` directories from the working directory up to its Git root.
Each skill is a directory with `SKILL.md` and valid Agent Skills frontmatter.
Only its name, description and path enter the standing model instructions;
the model can read the full file when relevant. Use `/skill:NAME [request]` to
load it explicitly. Prompt templates are direct `.md` files in
`~/.config/ion/prompts/` or project `.ion/prompts/`; `/NAME [arguments]`
expands one before submitting it. Templates support `$1`, `$@`,
`${1:-default}` and `${@:N:L}` argument forms; quote an argument containing
spaces. `ion resources` lists both, and the TUI has
`/skills`, `/prompts` and `/reload`. Starting, cloning, forking or switching a
Session refreshes resources; `/reload` refreshes the current Session. Personal
resources take precedence over same-named project resources; nearer project
directories take precedence over ancestors. Invalid resources are skipped with
a diagnostic. Project resources are treated as lower-trust repository text
under the same live-directory tool permissions as `AGENTS.md`; review
unfamiliar resources before using them.

Add local MCP tool servers explicitly with `ion mcp add NAME COMMAND [ARGS...]`.
Use `ion mcp add-http NAME URL` for a Streamable HTTP server; add
`--bearer-token-env VARIABLE` when it requires a bearer token already set in
the environment. `ion mcp list` and `ion mcp remove NAME` manage the saved
user configuration.
Ion starts configured servers when a coding client starts, discovers their
tools, and exposes each as `mcp__NAME__TOOL` alongside read, edit, write and
exec. MCP tool names that need normalization or shortening receive a stable
hash suffix in the model-facing name; Ion calls the server with its original
tool name. Headless, TUI and RPC use the same tool set. Local server processes inherit
the user's environment and permissions and run in the Session's working
directory. A repository file does not launch an MCP server merely because
Ion opened that directory. A failing server is reported at startup while
healthy servers and the coding client remain available; a failed
tool call becomes an error result visible to the model. Current MCP support
handles text, structured data and normalized image tool results; audio and
embedded resource content report an explicit unsupported-content error.
Large text or structured results include a bounded preview and a
`full_output_path` to a private JSON file containing the complete result;
`read` can inspect it in ranges.
When a server announces a changed tool list, Ion refreshes that server before
the next model request. A failed refresh reports a warning and keeps its last
known list. Remote MCP OAuth login is not yet supported.
For Rust embedders, `Host::agent_with_tools` takes the working directory,
selected route and a custom `CodingToolSource`; it composes that source with
the built-ins, and a same-name registration replaces that one built-in.
A source publishes `ToolRegistration` values that bind a `ToolDefinition` to a
`ToolExecutor`. Refresh publishes new registrations; an issued request retains
its original executor, including an MCP connection and remote tool name. This
freezes local dispatch identity, not the remote server's implementation.

Tools act directly in the working directory with the host user's permissions.
There is no implicit sandbox. If a process stops during a tool call, Ion
records its effect as unknown when the next prompt begins; it does not rerun
the call automatically. `ion --continue inspect` reads the existing log
without making that repair. `exec` retains the final 64 KiB observed from each
output stream. A complete truncated stream also has a private temporary file
at `stdout_full_path` or `stderr_full_path` so earlier output can be inspected
without rerunning the command. These files may expire across launches. Ion
reports omitted bytes when capture completes and marks a capture incomplete
if an inherited pipe remains open after output goes idle; an incomplete
capture has no full-output path.
Commands use Bash when available, then fall back to POSIX sh. Commands have
no default timeout; pass
`timeout_ms` when a deadline is needed. `read` uses byte offsets and returns a
UTF-8-safe `next_offset`. For files within its 8 MiB text-edit bound,
`read.base_digest` covers the full file and can guard a later `edit`;
`edit` takes an `edits` array of `{old_text, new_text}` replacements against one
original file snapshot. It rejects ambiguous or overlapping matches before
writing, accepts ordinary LF or CRLF text, and preserves the file's BOM and
unaffected line endings. A damaged Session file is skipped
by `sessions` and `--continue`, while opening its exact path reports the
error. Ion summarizes settled history when its request
nears a known model's context window or exceeds its transport bound, and can
retry one model request after a provider reports context overflow. The raw
conversation remains inspectable; `compact` and `/compact` also trigger this
explicitly. Longer saved histories are summarized in bounded steps when one
summary request cannot fit. Tool results that individually exceed the route's
request bound, or return images to a text-only route, give the model a bounded
explanation. Their observed output remains available in Session inspection,
export and tool detail, with a notice that it was not shared. The Session's
64 MiB encoded-entry limit still applies; a failed result commit leaves the
effect unknown rather than silently discarding data or replaying the tool.
A large prompt or accumulated context can still exceed the limit when no
settled group can be summarized. The MiMo adapter classifies a zero-output
length stop near a known context window as context pressure, independent of
the configured provider name.
Transient provider failures can trigger up to two cancellable retries before
stream output; retry events appear in the TUI and JSONL output. A response that
stops after producing partial output is not replayed silently.
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

## Optional Code Mode

`ion --code-mode run 'Read the files in parallel and return a summary'` adds
`code_mode` alongside direct tools. The same flag works with chat and RPC;
`Host::with_code_mode(true)` enables it for embedded hosts and later binding
changes. It is off by default. Its reserved tool name replaces a custom
`code_mode` registration when enabled.

The model supplies an async JavaScript **body**, for example:

```js
const results = await Promise.all(
  ['src/main.rs', 'Cargo.toml'].map(path => tools.call('read', {path}))
);
return results.map(r => ({characters: r.value.content.length, failed: r.is_error}));
```

`tools.call(name, arguments)` returns `{value, is_error, image_mime_types}`.
`tools.describe(query)` returns up to ten matching frozen definitions, including
callable deferred tools. Input schemas are included; typed result schemas are
not yet exposed. Images stay in saved inspection; the guest receives MIME
metadata, not image bytes. The selected JSON return value, call count and
failure/skip counts reach the model. Raw child arguments and observations remain
in the Session, export and nested Ctrl-O/tool detail, without automatic addition
to model context. A guest can explicitly select their content in its return.

Child intent commits before dispatch and output before guest consumption. A
normal return drains requests already transferred by the guest, even if
unawaited. Failure or cancellation closes admission and awaits started work.
A failed result commit blocks dependent consumption; interrupted children are
recorded as unknown before their parent is recovered. Reopening never reruns
the script. `Promise.all` does not roll back writes or external calls.

QuickJS runs on a separate blocking worker without ambient filesystem, network,
module or timer APIs. Default limits are a 30-second guest deadline (including
host waits), 64 bridge requests, four concurrent calls, a 64 MiB JS heap,
512 KiB stack, 64 KiB source and 1 MiB per JSON value. Cumulative guest replies
are limited to 8 MiB. A 32 MiB child-audit admission threshold stops further
dispatch; already-started outcomes still commit, so it can overshoot. Successful
guest return ends its deadline, not host settlement.

These limits are not an OS sandbox or a process-RSS/disk quota. Native and MCP
calls retain host permissions and their own capture/timeout contracts. Full
command captures can consume disk; there is no aggregate artifact quota or
retention guarantee. No comparative latency/token benefit is established yet.

## Current limits

The required CI gate now includes Linux PTY, RPC, MCP refresh/withdrawal,
selected-point fork and resource-reload workflows in addition to format, strict
Clippy and workspace tests. The terminal workflow exercises inline startup, grouped tool activity,
temporary full-screen views, persistent fullscreen startup, inline/fullscreen
mode switching, resize, Session/model controls, masked login, copy, clean
restoration and an explicit process panic after fullscreen ownership has begun.
This is automated qualification, not a substitute for manual checks in every
supported terminal.

Short live coding tasks have passed on macOS with direct DeepSeek and MiMo,
OpenRouter DeepSeek Flash, custom OpenRouter routes for
`stealth/space-bunny-alpha` and Gemini 3 Flash Preview, and a custom llama.cpp
route. OpenRouter and local llama.cpp tasks also passed on Linux. The cataloged
OpenRouter DeepSeek route passed user-image and workspace-image tasks,
resource use, MCP tool use and Session continuation. These checks cover
selected tasks and routes. A separate OpenRouter DeepSeek Flash task used one
two-replacement `edit` call with a `read.base_digest` guard and verified the
result with shell. Anthropic and
direct OpenAI have not been live-qualified.

Direct DeepSeek and MiMo and the qualified OpenRouter routes retain the
reasoning needed for tool-call continuation across saved Turns. The Gemini
route completed a signed tool continuation and another Turn after cross-process
resume. An offline headless Anthropic Messages check completed signed tool
continuation, cross-process resume and durable reasoning resets after
compaction and project instructions changed. The native route still needs a
live Anthropic credential and account qualification; thinking on custom
llama.cpp routes remains unsupported. Context pressure uses an approximate
token estimate; custom routes without a known context window use only the
encoded request bound. See [ARCHITECTURE.md](ARCHITECTURE.md) for the design
contract and [AGENTS.md](AGENTS.md) for repository checks.

For Rust embedding, `ion-host::Host` composes the same model catalog,
credentials, project instructions and Session discovery used by the CLI.
`ion-core::CodingAgent` and `CodingSession` own the coding loop and committed
conversation; `Host::agent_with_tools` accepts a custom `CodingToolSource`.
Host-created agents bind the selected logical model, transport and limits.
Core agent constructors also take a `ModelRef`; submission and compaction use
that bound model without another model argument. Construct a new agent to
change models. `Host::resources` loads project skills and prompt templates. For a long-lived
Rust client, `ion-host::SessionBinding` owns the active Session, model, agent
and resources and handles idle model and Session changes. `ion rpc` provides
long-lived subprocess control.

## License

[MIT](LICENSE)
