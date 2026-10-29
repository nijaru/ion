# Ion

Ion is an unreleased Rust coding agent for a local working directory. It has
one coding loop for terminal chat, headless prompts and Rust library hosts. The
agent can read, edit and write files and run a native shell command. Sessions
are saved in SQLite and continue across launches. Ion is experimental;
everyday coding reliability is not yet qualified. Coding is the first application
of the shared engine; [architecture](ARCHITECTURE.md) defines the broader targets
for embedded applications, durable services and managed agents.

## Start

Build with the latest stable Rust toolchain:

```sh
rustup update stable
cargo build --locked -p ion
target/debug/ion models
```

Use `target/debug/ion` in the commands below, or install the binary on your
`PATH` with `cargo install --locked --path crates/ion-app`.

Run `ion` from a terminal to begin. If no model is configured or authenticated,
Ion opens a searchable model catalog. Choose a model, enter its API key at the
hidden prompt, and continue into chat. That choice becomes the default. Esc or
Ctrl-C closes the picker without creating a Session; Ctrl-C cancels key entry
and returns to the picker. Configured keyless endpoints need no key entry.
Setup does not issue a model request or start MCP servers. Credentials and model
choices already saved are not rolled back if later application setup fails.

Ion picks a catalog model automatically when its provider key is present in
`DEEPSEEK_API_KEY`, `XIAOMI_API_KEY`, `OPENROUTER_API_KEY`, `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`. The default preference starts with DeepSeek Flash, then
MiMo Flash, then DeepSeek Flash through OpenRouter. To select one explicitly,
run `ion use PROVIDER MODEL` with an ID or name substring from `ion models`.
A selected or reopened model with a missing key opens the same hidden prompt in
interactive chat. `ion login PROVIDER` also accepts a key at a hidden terminal
prompt. Headless, RPC and redirected launches never open setup UI; configure a
model and credential first. Invalid routes remain errors rather than falling
back to a different model. `ion auth` shows which credential source is
active, and `ion logout PROVIDER` removes a saved key. Environment keys take
precedence.
With the existing Pi FNOX profile, for example:

```sh
fnox -c ~/.config/fnox/config.toml --profile pi exec -- target/debug/ion --model openrouter/deepseek/deepseek-v4.1-flash
```

From the project directory:

```sh
ion                        # terminal chat
ion 'Inspect and fix the failing test' # start a prompt, then keep chatting
ion chat 'Summarize this project'      # explicit terminal mode
ion run 'Inspect and fix the failing test'
ion -p 'Summarize the changes'
ion -p @notes.md @src/lib.rs 'Review these files'
ion chat @diagram.png 'Explain this image'
ion --json run 'Inspect and fix the failing test' > events.jsonl
ion rpc                    # persistent JSONL control on stdin/stdout
git diff | ion -p 'Review this change'
git diff | ion -p           # stdin-only headless input
printf 'Explain this project' | ion run
ion --continue             # reopen the latest session in this directory
ion -r                     # choose a saved Session; Tab browses all folders
ion sessions               # list saved sessions and their IDs
ion doctor                 # offline, read-only configuration checks
ion --session ID inspect   # committed history for a selected session as JSON
ion --continue usage       # recorded token usage, without a model request
ion --continue clone       # copy the latest conversation to a new session
ion --continue turns       # list Turn numbers and prompt previews
ion --continue fork 2      # new session before Turn 2, preserving the source
ion --continue fork 2 --after # new session after settled Turn 2
ion --continue compact     # summarize old context; retain the raw Session
ion --continue compact 'Preserve API decisions and unresolved failures'
```

Without a command, Ion opens chat when stdin and stdout are terminals; piped or
redirected I/O selects headless execution. Explicit `chat` and `rpc` keep their
own input modes. `-p` selects headless mode independently of the prompt, so
`ion -p --model PROVIDER/MODEL 'PROMPT'` also works. Use `--` before a prompt
that starts with a dash, starts with `@`, or matches a command name. For example,
`ion -- @literal` sends literal text rather than reading a file.

Each prompt argument is a separate ordered Turn. Quote words that belong to one
prompt; Ion does not join extra arguments with spaces. This is the current
Pi-aligned default, not a permanent compatibility promise:

```sh
ion run 'Review the changes' 'Now suggest focused tests'
ion @notes.md 'Review this context' 'Summarize the findings'
ion run -- '@literal first prompt' 'usage'
```

Terminal launch submits the batch once after model setup or Session selection,
then stays interactive. Skills/templates expand separately for each prompt, but
terminal built-ins and `!`/`!!` syntax are model input here, not local commands.
Each interactive prompt is limited to the editor's 64 KiB recovery capacity.
Refused batches retain the first literal prompt/cursor/attachments in the editor
and the remaining prompts in existing recovered-draft custody. Stop or a failed
operation cancels unstarted inputs; nothing automatically retries or continues an
interrupted Turn. The queue is local and volatile, not a durable batch transaction.
Headless output keeps one final last answer on success and reports any failure,
even after earlier successful Turns. Files, `--image` and literal stdin accompany
only the first prompt; without a prompt or `@file`, images stay unsent.

Bare launch, `run` and `chat` accept multiple explicit `@file` operands alongside
prompts, or files alone. Relative files resolve from the selected Session's
working directory; `@~/...` uses HOME. Ion reads each regular file (or symlink to
one) once: UTF-8 text becomes literal JSON-framed context, and supported images
use normal image normalization and model-capability checks. Only the prompt
expands resources, not file contents or stdin. Empty files are retained; a leading
UTF-8 BOM is stripped. Invalid UTF-8, binary NUL bytes and special files are
rejected, not silently converted or truncated. Sources are bounded to 32 MiB each,
and each complete encoded input must fit the 8 MiB request bound. Headless launch
also checks the whole batch before Session/MCP startup: at most 64 inputs and the
existing 32 MiB retained-input budget, including originals, prepared messages and
metadata. Interactive template and batch-budget errors use normal recoverable
admission after first draw. File snapshots survive source edits and rejected input;
committed history stores the complete context, not live paths. History too large
for the 64 KiB editor is visibly refused without replacing the current draft.
Typing or completing `@path` inside chat does **not** import a file.

`--model MODEL` selects a known catalog or configured custom route for this
invocation. Use an exact ID or a case-insensitive ID/display-name substring;
`--model openai/mini` and `--provider openai --model mini` constrain the match to
OpenAI. An explicit provider never falls through to another provider. With
`--provider`, the complete model text stays inside that provider, including
namespace slashes. Colon suffixes remain literal IDs; set effort with `--reasoning`.

Exact matches win. Otherwise Ion prefers undated aliases over dated snapshots,
then the highest model identifier. Bare patterns spanning providers prefer locally
usable routes; a tied ID on several providers is an error with canonical choices.
A unique model can be selected before its key is configured. Resolution does not
probe account access, invent routes or change the saved default. `ion use PROVIDER
MODEL` uses the same matching rules and saves the canonical default. Reopened
Session models and saved defaults remain exact, not patterns that can drift.
Options may appear before or after a command, but must apply to it: metadata
commands do not accept model or execution overrides. Use `ion COMMAND --help`
for that command's options.

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

`--tools LIST` limits the model-visible tools to comma-separated exact names or
`*` patterns; `--exclude-tools LIST` removes matches after inclusion. Exclusions
win, and both apply to native tools, MCP tools and Code Mode's callable namespace.
`--read-only` is an allowlist preset for `read`, `ls`, `find`, `grep`, `web_fetch`, `web_search`, `history`,
MCP resource inspection and Code Mode when enabled; it excludes native mutations, direct shell
execution and managed children. These flags are model-loadout policies for the invocation, not
OS sandboxes or credential boundaries: user `!` shell commands and host-side
application effects remain separate. A managed child inherits the initiating
application's selection. `tool_search` is added only when the selection allows it
and selected deferred tools need discovery. `--read-only` cannot be combined with
`--tools` or `--exclude-tools`. Terminal and RPC coding also include
`ask_user_question` through their live responder; one-shot headless coding has no
answer channel and omits it. Unattended RPC clients can use
`--exclude-tools ask_user_question`.

`--cwd PATH` chooses a working directory. By default, a run starts a new
session; `--continue` reopens the most recently active one in that directory.
`-r` or `--resume` opens a searchable launch-time chooser before model setup or
Session/MCP admission. It starts in the current folder; Tab switches to all coding
folders and shows each Session's recorded directory. Ctrl-N toggles named Sessions
only, retaining the search query and folder scope. Esc or Ctrl-C exits without
creating a Session or changing the default model. Empty results still allow scope
changes. The chooser requires terminal stdin/stdout, is unavailable in RPC or with
`--json`, and conflicts with `--session`/`--continue`; use those explicit selectors
for scripts. An explicit `--cwd` must still match the chosen Session. If its saved
file vanishes during selection or model setup, reopening fails rather than
creating a replacement.
`--session ID` selects a listed session, and `--session PATH` can create or
open an exact SQLite path for scripts. Session paths are canonicalized so
symbolic links share the same writer lock. Hard-linked databases are rejected;
use clone or fork to copy a conversation. Opening and quitting an empty chat does
not displace the latest conversation in `--continue` or `sessions`. An
existing session uses its recorded directory, and an explicit `--cwd` must
match it. Headless runs print the
session ID to stderr. In the TUI, `/new`, `/clone`, `/tree`, `/fork`, `/fork-after`, `/resume`, `/name`, `/session`,
`/model` and `/compact` manage the conversation; `/login` chooses an API-key
provider, and `/login PROVIDER` or `/logout PROVIDER` manages its saved key.

`/session` also shows recorded token usage; `ion --continue usage` or
`ion --session ID usage` prints the same accounting without model or MCP setup.
It includes assistant, compaction and cache-refresh observations from all saved
history, including inactive paths and copied history—not just current context.
Cache reads/writes are subsets of input, not extra tokens. Missing measurements
stay unknown; partial totals show reported subtotals and unreported observation
counts. Logical/effective routes and provider-returned model names retain their
saved attribution. In-flight/unrecorded requests and other Sessions are excluded.
Historical prices and charges are not stored, so cost remains unknown; this is
not a bill, provider quota or spending budget.

`/tasks` opens a searchable list of application-owned background tasks. Select a
row for progress/details, a readable result, stop or removal. Live metadata shows
the captured model, content-free activity, elapsed time and last-observed activity;
progress inspection adds the bounded, literal provisional preview and error cause.
Thinking content and tool arguments/results stay hidden. Previews are not final
answers or committed facts. Reading or closing a view never consumes a result or
stops a task. Esc closes the picker; Ctrl-C still stops foreground work while busy.
Stop requests cancellation, not rollback or completed cleanup. Results and removal
require actual joining. Removing a finished task releases its retained result and
any returned guidance, shows them once more and leaves saved Session facts intact;
uncommitted guidance is not automatically saved or replayed. Attachment counts
are shown without decoding their contents. Explicit `/tasks status ID`,
`/tasks result ID`, `/tasks cancel ID` and `/tasks acknowledge ID` remain available.

The TUI model picker searches catalog and configured
custom routes, marks the current model and preselects it. Typed `/model MODEL`
uses the same shorthand matcher as `--model` and reports the canonical choice.
Ctrl-L opens that picker
without replacing the draft or its cursor/attachments. Choosing a native model
with a missing key opens the same hidden credential reader as first-use setup;
Ctrl-C cancels key entry without changing the current model. Model selection never
changes the saved default. Saving a key is local storage, not remote authentication;
an explicitly saved key remains saved if later binding preflight fails. Environment
keys take precedence over saved keys. A resumed session restores its model;
cancelling its key prompt and choosing a replacement model repairs that binding
without changing the global default. `ion use` sets the
default for new sessions. Custom routes and that default are published together
in `~/.ion/models.json`, with concurrent saves serialized. Ion keeps user
configuration, credentials, resources and saved Sessions under `~/.ion`; set
`ION_HOME` to choose another directory for all of them. Existing XDG data is
not automatically imported or migrated.

Run `ion doctor` to check local setup without starting a coding client. Use
`ion doctor --cwd DIR` to inspect another workspace. It checks the default or
environment-selected model, credential presence, shell/search/tool-veto settings, resources,
command/MCP manifests and keybindings. Independent checks continue after a failure.
Output rows use `ok`, `warn` or `fail`; any failed check exits 1. Omitted resources
and skipped/invalid optional manifests can warn while exiting 0. Unsupported options
such as `--model`, `--session` and `--json` are invocation errors (exit 2).

Doctor makes no network request, runs no shell setup/tool/command/MCP server,
opens no Session and writes no files or trust records. It withholds credential
values, endpoint URLs and configuration bodies; resource paths remain visible.
A present key is **not authenticated**, and success is not a Session-integrity,
remote-access, writability or complete runtime-health guarantee. There is no
implicit repair or cleanup. MCP configuration/trust and keybinding sources must
be regular files or symlinks to them, not FIFOs or devices.

`/resume` opens a searchable saved-Session browser within the active workspace
and marks the current Session. Unlike the launch-time chooser, it does not switch
the live binding's working directory. Ctrl-N toggles named Sessions only here too;
empty filtered results remain reversible. The filter resets on close and does not
change any saved Session. Rebind or disable `app.session.toggleNamedFilter` in
`keybindings.json` alongside the other controls.
Both browsers search names, full IDs, folders and first-prompt previews, ignoring
case and keeping recent order. Separate words can match different fields; quote
`"a phrase"` to match it within one field. Unquoted name/folder words also allow
fuzzy character matching. IDs and previews use literal text. This searches captured
metadata, not full conversation text or regular expressions.
The selected conversation shows its full ID, Turn count, recorded model and file
activity timestamp/age. Activity is a discovery-time snapshot, not execution
status. Discovery runs outside the input loop; Esc dismisses the browser while
Ion retains and joins its scan before starting another scan. An in-flight
filesystem/metadata call cannot be interrupted. Dismissed results cannot overwrite
a newer picker. Browsing does not submit input or repair history;
confirming a selection switches through idle shared control. This is a flat
catalog, not inferred fork ancestry; `/tree` explores context paths within a Session.

Use `--reasoning high` for headless/chat/RPC startup or manual compaction,
`/reasoning` for searchable, adapter-supported choices with the current effort
preselected, `/reasoning high` in an idle terminal, or RPC
`{"id":"r","type":"set_reasoning","effort":"high"}`. Values are `default`,
`off`, `low`, `medium`, `high` and `budget:TOKENS`. The preference is
Session-owned: reopen and clone retain it, forks restore the selected Turn's
preference, and new Sessions start at `default`. Coding and compaction freeze
the same preference. Changes require no live operation. Reopened interrupted
history may remain unresolved: changing a future preference does not settle its
Turn, rewrite its original model/reasoning, or recover unfinished effects.
Logical-Turn continuation still requires matching authority and configuration. Unsupported adapter controls refuse before saving
the preference or making a model request. Model changes also preflight the
retained preference rather than silently resetting it. Explicit effort selection
saves the bound model and effort together, including an invocation-only model override.

Support is route-specific, not proof of remote model support or entitlement.
Messages currently accepts only `default`; llama.cpp's no-thinking route
accepts `default` and `off`, both disabling thinking. Other Chat routes accept
the five non-budget choices. DeepSeek maps medium/high to its high setting;
MiMo maps all three levels to enabled thinking. Every current HTTP route refuses
exact token budgets. Effort affects future generation, not retained historical
reasoning or signed continuation. `/settings thinking show|hide` remains display-only.

Terminal chat is inline-first. Settled transcript rows are appended once to
native terminal scrollback; Ion keeps only the active composer/progress region
mutable. Resuming or switching to a saved Session bootstraps at most the latest
six turns into scrollback and labels omitted earlier history as retained; the
complete Session remains available through inspect/export and tool detail.
Publication uses available terminal rows before scrolling. Ordinary redraw and
resize do not publish live rows; terminal-native reflow remains emulator-owned.
`/help` groups commands by purpose without dumping their argument grammars.
Use `/help model` (or another built-in command name) for usage and `/help keys`
for effective keyboard controls, or [configure keybindings](#keybindings).
Type `/` for searchable command/resource discovery.
Command responses (`/help`, `/skills`, `/tools`, `/tasks result`, etc.) and
client diagnostics also publish their complete wrapped content, not a clipped
composer preview. They remain in a terminal-local display log interleaved with
Session history for fullscreen scrolling. This log lasts until Session replacement
or client exit; it is not durable Session history or model input. Explicit repeated
commands produce fresh responses; identical consecutive diagnostics at the same
history boundary coalesce. Unfinished recalled or resynchronized facts stay in a
mutable preview rather than being irreversibly published.
A hidden inline caret, including a collapsed question, reserves one blank
cursor-parking row. At a one-row terminal height, only that blank anchor fits;
question reopen, cancellation and Stop controls remain active. A visible composer
caret needs no extra parking row.
Inline paint waits 300 ms after resize notifications while input and coding work
remain live, then measures the restored primary cursor before reclaiming mutable
rows. It cannot remove live text the emulator has already moved into native history;
ambiguous cursor evidence stops the client rather than authorizing guessed erasure.
The active operation and cancellation status remain visible alongside steering,
queue and detail-close notices when there is room beyond the editable cursor line. On terminal input or rendering failure during a
Turn, shell or compaction, Ion cancels and awaits started work, then exits without
admitting queued prompts. Inspect the Session for saved outcomes when the
terminal can no longer display them.
During long Turns, the inline region keeps queued/running/exception counts,
the latest exception and a rooted preview of current work. Execution-start
progress changes only the corresponding committed call; later admitted calls
remain queued. Overflow selects whole actions, favoring running work rather
than a flattened tail. Individual rows may be omitted; Ctrl-O retains the full
current conversation. Missing saved results are unknown, not still running.
After settled history is published, an expanded live band shrinks back to the
rows the active composer/status actually need. Short viewports select a
cursor-containing composer window within their actual row budget, reserving a
status/control row when space allows. Related tool calls are rendered
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
Pipe tables retain inline styles and column alignment, wrapping cells to fit.
When columns cannot remain legible, rows become header/value fields instead of
clipping their values. Already-published inline tables follow the emulator's native
reflow rather than being rebuilt. Fenced code with a recognized language label
uses bundled syntax grammars (including Rust, Python, shell, JavaScript,
TypeScript/TSX, JSON and TOML). Only the first word of the label selects a grammar;
there is no language inference, file lookup or download. Multiline comments and
strings retain their syntax state within each block. Highlighting uses terminal
palette colors, without backgrounds or faint text. Unlabeled, unknown and explicit
`text`/`plain` blocks remain literal code. Blocks over 32 KiB or with a line over
2 KiB also remain literal to limit synchronous grammar work; no text is omitted.
Syntax styles use the same control-safe grapheme wrapping and publication path
as other assistant text, in inline and fullscreen views. `ion syntax-licenses`
prints bundled grammar notices without initializing model or Session services.
Other Markdown extensions are not implemented.

Successful native edits and writes record a unified patch from the previous
text and the published text, alongside both digests where applicable. A created
file is diffed against empty content. The patch capture is limited to 64 KiB and
explicitly marked when truncated; it is not a later filesystem diff. If a write's
previous content is unreadable, not UTF-8 or exceeds the native file bound, the
result reports `diff_omitted` instead of inventing a patch. Compact output
previews up to eight patch rows with addition/removal styles; long lines and
additional rows are omitted with an ellipsis. Ctrl-O or `/tool N` shows the full
recorded capture, including truncation, without interpreting its contents as
Markdown.

`/settings` opens a searchable list showing theme, tool-output and thinking
presentation and the receiving Session's completion-delivery policy. Unlike the display rows,
completion delivery can authorize additional model work; see [Completion delivery](#completion-delivery).
Enter (or Space with an empty search) cycles the selected value immediately; the
list stays open until Esc. `/settings compact` (the default) or `/settings expanded`
also changes output detail without changing model behavior.
Compact shows the first three read-content lines and the last three lines of
both command streams (four for direct shell), with explicit omission notices.
Expanded shows all recorded read/command output and edit capture, and separates
successful observations instead of coalescing them. Neither mode fetches omitted
file ranges or capture artifacts; capture truncation stays visible. Tool text is
literal and control-safe, not Markdown. The choice lasts for this terminal only
and applies to fullscreen and future inline publication. Existing native history
is not rewritten or republished; the bounded inline progress view stays compact.
Ctrl-O always retains source inspection.

`/settings thinking show` or `/settings thinking hide` controls provider-supplied
human thinking separately from output detail and generation effort. Thinking is
hidden by default and shown as labeled, subdued literal text when enabled. The
choice is terminal-local; it affects live/fullscreen display and future native
publication, never rewrites prior scrollback. Inspection/export retain the safe
human text, while final-answer copy excludes it. Adapters project known readable
text/summary fields only; signatures, encrypted/redacted blocks and opaque replay
never enter presentation. Replay custody is unchanged, and human thinking does
not become answer text or standalone model context.

### Themes

`/settings` → Theme (or `/settings theme`) opens a searchable theme picker.
Moving the selection previews its colors, including a small semantic sample;
Enter saves the choice; Esc restores the active theme. Both return to the parent
settings list, or to the composer when opened directly with `/settings theme`.
Drafts, attachments and model settings do not change. Failure before publication leaves the choice
available for retry or cancellation. If publication succeeds but the final directory
sync fails, the selection applies with a crash-durability warning. The preference
is user-only `~/.ion/settings.json`:

```json
{"theme": "system"}
```

`system` is the default. It queries the terminal's foreground, background and ANSI
palette with a bounded initial wait. Readable reported colors are retained;
low-contrast semantic text moves toward black or white to reach 4.5:1 against the
reported background. Missing background replies use terminal-default text and
ANSI colors without faint text. Supported appearance notifications trigger a
refresh; Ion preserves the original notification-mode state. It never changes
the emulator's palette or appearance preferences.

`dark` and `light` are fixed palettes. Choose Automatic in the picker to select
light and dark names separately, then Apply to save the pair. Esc from either name list returns without changing that
slot; Esc from the automatic menu cancels the whole preview. A value such as
`"light/dark"` selects the first name on a light terminal and the second on a dark
one; custom names also work in pairs. Reported background/foreground take
precedence over appearance notifications, then `COLORFGBG`, then dark. `ion chat --use-theme NAME` overrides
the preference for that invocation without saving it.

Put custom files in `~/.ion/themes/NAME.json` (or `$ION_HOME/themes/NAME.json`):

```json
{
  "name": "my-theme",
  "base": "system",
  "vars": {"accent": "#9ad"},
  "colors": {"user_input": "accent", "error": "#e45757", "added": 2}
}
```

The name must match the filename; `system`, `dark` and `light` are reserved.
`base` defaults to `system` and may also be `dark` or `light`. Missing roles inherit
that base. Colors are `#RGB`, `#RRGGBB`, ANSI indices 0–255, `""` for the terminal
default, or references to `vars` (at most 32 levels). Colors set foregrounds;
semantic bold/italic and selected-row emphasis remain intact. Available roles:
`text`, `heading`, `secondary`, `selected`, `user_input`, `error`, `warning`,
`running`, `code`, `link`, `added`, `removed`, `hunk`, `syntax_keyword`,
`syntax_string`, `syntax_number`, `syntax_function`, `syntax_type`, `syntax_comment`.
Files are bounded to 64 KiB; unknown fields/roles, invalid values and cyclic or
missing variables are errors. Optional `$schema` metadata is accepted. This is
Ion's native format, not a Pi or VS Code theme importer.

The active custom file is checked for edits once a second. An invalid edit keeps
the last valid palette and shows an error. Reopen the picker to discover new files;
`/reload` rereads the saved preference together with theme files, keys, resources
and commands, preserving the current presentation if preparation fails. Project
and package themes are not discovered by this user-file workflow.

Changes repaint mutable inline/fullscreen views and affect future publication.
Already-published native history is neither rewritten nor replayed. Literal tool
observations, attached programs, source inspection, captures and copied/exported
text are not recolored. Explicit custom colors remain the author's choice; the
system palette's contrast correction does not override them.

### Terminal views and status

Inline remains the default, but persistent fullscreen is also available with
`--tui-mode fullscreen`; use `/tui inline` or `/tui fullscreen` to switch
inside chat. The interactive terminal publishes an OSC 7501 root record with
`app=ion` on state changes: `idle`, `working`, `done`, `error`, or `blocked`
with `kind=auth` while the masked credential prompt is waiting. `done` and
`error` remain until replaced by a later outcome. Live foreground control,
including input preparation, temporarily reports `working`; preparation that
returns a draft without starting an operation restores the retained outcome
(or `idle` if none exists). Credential entry is also a temporary overlay.
An observed failed shell command reports `error`; cancellation reports `idle`.
`working` is not re-sent as a heartbeat. The sequence is emitted only by the interactive terminal's physical output owner,
never by headless, RPC or redirected output. Reports omit optional task messages,
titles and percentages. A displayed question reports `blocked` with
`kind=question`, without its text; closing the dialog restores the underlying
operation/outcome status. Per-call approval is not implemented, so Ion does not
invent an approval state. Feature detection is optional in the protocol; Ion
sends reports without adding a startup query.
Fullscreen owns the transcript viewport and scrolling while using
the same Session, agent loop and semantic transcript projection. File/model/
session pickers and Ctrl-O conversation detail use alternate-screen views in either
mode. The live footer identifies the workspace, Session name (or short ID), current
`PROVIDER/MODEL` and `/help`. It follows the active binding even before the first
Turn and yields space to editing, completion and stop controls on small terminals.
It is cleared when the client releases its inline surface; `/session` exposes
full Session/context detail on demand.
Cloning copies committed conversation and context into a new session with
independent future turns. Both sessions still use the same live working
directory; cloning does not copy or restore files.
`/tree` opens a recent-message picker, marking the selected path and preserved
alternatives; `/tree ENTRY` selects directly. User points restore editable input
and images without sending; assistant and settled tool-group points continue after
their observations. Selection uses idle shared control, keeps the same Session ID,
and survives reopening without another prompt. It does not undo files or generate
a summary. `inspect` retains all raw facts; conversation export follows the selected path.
`/fork` opens a searchable Turn picker and restores the selected prompt and
images, including each image's coordinate note, in a new Session;
`/fork TURN` selects directly. Oversized prompts or multipart input that the
editor cannot represent report a notice and leave the current draft intact;
the selected input remains in the source Session. `/fork-after` opens a picker
of Turns with recorded ends, including cancelled or failed Turns; `/fork-after
TURN` selects directly. It copies through that end without submitting input or
clearing the existing draft and images. CLI `fork` prints the new Session ID.
The source retains all later history. These operations copy
conversation facts, not working files, and an unfinished Turn cannot be an
after-Turn point. RPC clients can use `list_turns` and `fork` with `turn` and
optional `after: true`.
Session IDs are immutable UUIDs stored in the header, independent of filenames
and provider affinity. Renaming a closed database does not change its ID; clone
and fork create new identities without inheriting external delivery receipts or continuation grants.
This unreleased branch uses Session format 18; earlier development Session
files are rejected, not automatically migrated or replayed. Tool activity classification used by the transcript is
stored with each assistant tool-call batch, so resumed history is not
reinterpreted through the currently installed tool catalog.
For headless integrations, `--json` works with `run`, `--print` or an implicit
piped invocation. Argument text is optional when stdin supplies the prompt.
Headless input must contain nonblank text, file context or an image. Ion reads and
validates piped UTF-8 input, up to 8 MiB, before creating a Session or starting MCP
servers.
Stdin alone is preserved exactly, including surrounding whitespace. With both
stdin and an argument, Ion expands resources in the argument, then prepends the
literal stdin and one newline; stdin never selects a template or client command.
The selected model's context window and Ion's encoded request bound can reject
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

`ion rpc` keeps a Session open for a subprocess client. It emits a `ready`
record, then accepts one LF-terminated JSON command per stdin line. Commands
can carry a string `id`; each response repeats it. For example, send
`{"id":"1","type":"prompt","message":"Inspect this project"}`. A successful
prompt response includes a Turn ID and confirms that the input entered the
Session. Keep reading progress records with that Turn ID until `turn_end`
reports `completed`, `cancelled` or `failed`; a response alone is not the
answer. `final` is the committed answer. Other commands are `steer`,
`follow_up`, `submit_request`, `get_request`, `get_question`, `answer_question`,
`cancel_question`, `clear_queue`, `abort`, `get_state`,
`inspect`, `get_context`, `navigate_context`, `list_sessions`, `list_turns`, `list_models`,
`list_resources`, `reload_resources`, `shell`, `compact`, `set_model`, `new_session`,
`clone_session`, `fork`, `switch_session`, `set_reasoning`, `get_background`,
`cancel_background`, `acknowledge_background`, `set_completion_delivery` and `set_name`.
`set_name.name` must be a string; null or omission clears it. `fork.after` must
be a boolean when present and defaults to false when omitted. Wrong field types
are rejected before mutation. `compact` accepts optional string `instructions`,
for example `{"type":"compact","instructions":"Preserve API decisions"}`;
null and non-string values are rejected before admission.
`get_context` returns a `position` token and message points with stable `entry`
and selectable `head_entry` coordinates. `navigate_context` requires that token
as `expected` and a `head_entry`; stale previews fail rather than discard newer
work. It changes context without submitting input or changing files.
`get_state.reasoning` reports the active Session's generation preference.
`get_state.completion_delivery` reports `inbox`, `continue` or `wake`;
`{"type":"set_completion_delivery","delivery":"wake"}` changes that receiving
policy and revokes existing delivery grants, without upgrading earlier jobs.
Idle wakes emit `automated_turn_started` with operation/Turn coordinates, not a
client ID or prompt acknowledgement. Their `turn_end` has `automated: true` and no
client ID. Admission failure emits `operation_failed` with `automated: true`, not
a started Turn. These automatic lifecycle records are bounded, lossy observations:
reconcile gaps with `inspect` and `get_background`. Client-command replies retain
their reliable reply credits; unsolicited work cannot acquire those credits.
`prompt`, `steer` and `follow_up` accept `images` as an array of local paths (relative to the
Session's working directory) or inline `{ "mime_type": "image/png", "data": "BASE64" }`
objects. Ion validates and normalizes both before accepting the input.
Image preparation occupies one pending-input slot, not an accepted queue entry.
`get_state` reports shared control activity, including `preparing_input`. Image
preparation remains owned and joinable while controls and other queued input stay
available; active decoders cannot be forcibly interrupted. A preparation error
returns the original typed input. Returned `input` uses the tagged Host shape
`{"Prompt":{"text":"...","images":[],"files":[]}}`; RPC text/image commands leave
`files` empty. Steering preparation cannot attach to a later Turn when its target
ends.

`steer` targets the active coding Turn. Ready steering queued during automatic
compaction joins the rebuilt request; it does not rewrite an issued request or
interrupt a running tool. `follow_up` queues a separate Turn behind
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
owner's retained `runs` snapshot. `cancel_background` and
`acknowledge_background` take `run: "<run-uuid>"`, not a conversation UUID; the
former requests stop and the latter releases a joined report's capacity without
deleting its saved conversation. The
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
before acknowledgement. Execution completion does not release unread settlement
metadata; it stays reserved until handoff, as does returned input. These are custody bounds, not
context-fit or process-memory guarantees.

Output pressure alone never cancels work. `ready.publication` advertises a
32-command reply window; two control slots and one inspection slot are separate.
History, navigation and question queries, including `get_context` and
`get_question`, share the inspection slot. Question answers and batch cancellation
use the reserved control slots, so bulk replies cannot consume their admission
capacity; exhausted control slots refuse them without applying the decision.
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
While a turn runs, the editor remains available: Enter steers the next model
step, Alt-Enter queues a separate follow-up turn, Alt-Up returns the most
recent queued follow-up to the editor, and Ctrl-C cancels. After stopped work settles, unaccepted input is returned rather than starting
model work. An occupied editor stays separate: Alt-Up cycles retained drafts
without submitting them. Enter during shell or manual compaction queues a
follow-up. An ordinary nonzero shell exit does not withdraw an intentional follow-up. The terminal uses the same shared queued-input allowance
and route checks as RPC. Rejected input remains in the editor with its attachments; recovered steering retains image
notes with their corresponding images. Queued resource commands expand before
admission, so later resource changes do not rewrite an accepted follow-up.
The prepared prompt stays separate from the literal command, arguments and
editor cursor: dequeue or cancellation restores the literal, not its expansion.
Recovery keeps each literal, cursor and attachment group separate; it never
concatenates independent prompts. Composer layout retains its cursor-containing
window rather than every wrapped row.
Both the model message and retained editor metadata count against the shared
pending-input allowance. Clipboard preparation finishes before input admission.
Terminal queries share the key reader and use a 500 ms response deadline;
type-ahead survives startup and external-editor return. A resize during a cursor
query consumes the outstanding reply, defers painting and returns to client
controls before retrying; it does not by itself cancel coding work. Login deliberately
discards queued input before and after secret entry. CLI and chat login keep echo
disabled before showing the prompt; Ctrl-C cancels without saving a partial key.
Secrets must be valid single-line UTF-8 of at most 4096 bytes. Rejected or cancelled
unfinished pastes remain discarded through their closing marker. Bracketed
paste is limited to 64 KiB of valid UTF-8. Malformed, oversized or unfinished
paste is discarded without submitting a prefix or replacing the current draft.
### Keybindings

The shortcuts in this guide are defaults. Create `~/.ion/keybindings.json`
(or `$ION_HOME/keybindings.json`) to change ordinary terminal controls:

```json
{
  "app.model.select": ["alt+m", "f6"],
  "tui.input.submit": "ctrl+s",
  "app.message.copy": []
}
```

Run `/help keys` for supported action IDs and their effective bindings. A missing
action keeps its defaults; a string or array replaces them; `[]` disables it.
In this example, Ctrl-L no longer opens models and Enter no longer submits the
composer. Enter still confirms a visible completion or picker because that is a
separate action. Keys may repeat in separate focused contexts, but collisions
within one context—including unchanged defaults—are rejected. Mapped Stop and
suspend retain priority over ordinary pickers, details and questions.

Use `ctrl+`, `alt+` and `shift+` in any order, with a character or a named key:
`enter`/`return`, `escape`/`esc`, `tab`, `backtab`, `space`, `backspace`,
`delete`, `insert`, `home`, `end`, `up`, `down`, `left`, `right`,
`pageup`, `pagedown` or function keys such as `f6`. ASCII names are
case-insensitive; write `shift+a`, not `A`, for an uppercase chord. Symbols
can be literal (`!`, `ctrl++`); Shift-Tab and BackTab are equivalent.
Ctrl-minus and Ctrl-underscore share one binding. Other modifiers must match
exactly; unrelated extra modifiers do not activate a shortcut.

Legacy terminals cannot distinguish every chord: Ctrl-I is Tab, Ctrl-J/M is
Enter and Ctrl-H is Backspace. Use an enhanced keyboard protocol or another key
when those distinctions matter. Configuration cannot recover information the
terminal never sends.

The file is limited to 64 KiB. Unknown/duplicate action IDs, invalid keys, bad JSON,
unreadable files and collisions fail explicitly; invalid startup configuration is
rejected before terminal setup or Session/MCP admission. Edit the file and run
`/reload` to apply it alongside resources and commands. Failed reload keeps the
old bindings; successful reload preserves the draft and refreshes hints. There is
no watcher or project keymap. Headless, RPC, metadata commands and standalone
`ion login` ignore this terminal-only file.

Masked credential entry keeps physical Ctrl-C cancellation and its secret-editing
controls. Attached programs receive their own input, with physical Ctrl-] to detach;
ordinary application bindings do not intercept either lease.

### Default editing controls

Home/End or Ctrl-A/E move within the current logical line; Ctrl-B/F move by
grapheme. Ctrl/Alt-Left/Right or Alt-B/F move by Unicode word boundaries.
Ctrl-U/K delete to the line start/end, joining adjacent lines when already at
the boundary. Ctrl-W or Alt-Backspace deletes the preceding word; Alt-D/Delete
deletes the next word. Ctrl-D deletes forward in a nonempty draft and exits when
the draft is empty.

When Ion is a foreground Unix shell job, Ctrl-Z in chat returns to the shell;
run `fg` to resume the same chat,
including its draft, cursor, images and open view. The handoff restores terminal
modes and rechecks geometry on return. A `bg` resume stops again until `fg`
restores foreground ownership. It does not cancel or replay a running operation,
pause deadlines, or stop tools in independent process groups. The terminal status
record is cleared while yielding and republished on return. Masked credential
entry and attached programs retain their separate input handling.

Unsent prompt text has undo/redo: `Ctrl-minus` or `Ctrl-_` undoes; adding Alt redoes.
Typing groups at whitespace boundaries; paste, completion and successful external
editor replacement are atomic edits. Ctrl-C draft clear is undoable too. Ctrl-Y
yanks the latest killed text; Alt-Y cycles older kills immediately after yank or
yank-pop. Consecutive word/line kills accumulate in cursor order; ordinary
Backspace/Delete does not enter the kill ring. Moving the cursor or taking another
input action ends yank-pop affinity. Over-limit edits leave text and recorded
recovery entries unchanged.

Undo/redo retains at most 100 deltas and 1 MiB of text; the terminal-local kill
ring retains at most 32 entries and 256 KiB. Oldest entries are evicted first.
Submission or queue handoff, history/returned-draft replacement, Session
replacement and attachment changes start a fresh undo baseline. Same-Session
progress does not. Text undo neither restores attachments nor changes admitted
work, and never submits input. Kill text can cross drafts only through explicit
yank; neither history nor the kill ring is durable.

These editing keys remain available during work without
cancelling it; Ctrl-C still cancels the active operation.
Up and Down browse earlier prompts when the cursor reaches the first or last
editor line. Returning from history restores the unsent draft and its cursor,
even if a running Turn refreshed history. Type `@` or a partial `@path` at a token
boundary to see project-file suggestions without leaving the editor. Ordinary
`@` typing, including email addresses, stays literal. Up/Down selects a visible
suggestion; Tab or Enter completes its path without sending the prompt. Directories
end in `/` so you can keep completing their contents. Paths needing quotes are
escaped automatically. Esc hides suggestions and retains the current draft and
cursor, including edits made while browsing. Tab reopens dismissed suggestions.
Tab also opens completion for an ordinary filename, including an empty token;
keep typing to narrow it. Paths containing `/`, dot-prefixed paths, `~` and quoted
paths offer suggestions automatically. Relative, absolute and `~/` spellings are
preserved; `~/` uses HOME, not ION_HOME. An unquoted leading `/` is reserved for
terminal commands. Complete an absolute path after prose (`Read /tmp/pa`), with
`@/tmp/pa`, or inside quotes (`"/tmp/pa`). URLs and ordinary prose do not open
file suggestions automatically.
Choosing a file inserts a path reference for the model to read, not its contents.
Discovery runs off the client event loop. Fuzzy `@path` search uses the project
snapshot, honoring ignore rules without following symlinks. Ordinary paths and
explicit `@./`, `@../`, `@/` or `@~/` paths list only their requested directory,
including hidden and ignored entries; directory symlinks can be entered explicitly.
Each snapshot is bounded to 20,000 paths or 100,000 entries. Changing directory
loads a new snapshot; dismiss and reopen to refresh the same directory.
Limits and traversal errors are reported rather than hidden. Dismissal cancels
further traversal and prevents stale results from reopening suggestions; exit
still waits for any filesystem call already underway.
Restoring or replacing a draft also closes its old suggestions. Returned
input keeps its cursor and attachments; a displaced unsent draft remains
recoverable with Alt-Up.
Type `/` to discover terminal commands, prompt templates, `/skill:NAME` and
configured `/command:NAME` user commands.
Up/Down selects a suggestion; Tab or Enter inserts it without executing it.
Submit the completed draft separately. Esc closes discovery without discarding
input. Suggestions stay in the selected terminal surface, including during work;
terminal controls cannot be queued as follow-up prompts.
Ctrl-O opens the whole conversation, including current work and provisional
assistant text, in a full-screen detail view. Left/Right browse earlier/newer
pages (32 messages, calls or shell records); Up/Down and Page-Up/Page-Down
scroll. Results retain their full stored content, including capture paths.
`/tools` lists calls, `/tool` opens the latest and `/tool N` selects one. In a
selected tool view, Ctrl-X copies the whole recorded call: arguments, result,
capture/delivery notices and child activity, without viewport wrapping. It does
not read referenced capture files or copy image bytes. Copy feedback appears in
the inspector, which stays open; the whole-conversation view has no selected tool
to copy. Esc or Ctrl-O closes details and restores the selected chat renderer.
Input not yet accepted
by the Session returns to the editor if the turn fails or is cancelled.
If history cannot be read, Ion displays the error and retains the last readable
view. Shared control continues: follow-ups may already be running and are not
returned to the editor. Normal stop/failure rules still return unaccepted input.
Do not resubmit a prompt to repair the display; inspect the Session and effects
first.
Ctrl-G edits the current draft in `$VISUAL`, then `$EDITOR`, falling back to
`vi`; Ion keeps the original draft if the editor fails. The native editor owns
keyboard input while open; close it to return to Ion’s shortcuts. Outside detail views,
Ctrl-X or `/copy` copies the last committed assistant answer to the system
clipboard when one is available. On remote or displayless terminals, Ion sends an OSC 52
clipboard request, whose support depends on the terminal.

`/export` saves an offline HTML snapshot to a new `ion-session-UUID.html` file
in the Session working directory. `/export "shared transcript.html"` chooses a
path; `/export transcript.txt` saves plain text instead. From the CLI,
`ion --continue export transcript.html` saves HTML and `ion --continue export`
prints plain text to stdout. File extensions `.html` and `.htm` select HTML
(case-insensitive); other extensions select text. Relative paths use the saved
Session working directory. Existing files and symlinks are never overwritten;
repeated pathless terminal exports create separate snapshots.

Both formats contain only committed facts on the selected context path, not
hidden alternative branches or provisional streaming text. HTML renders assistant
Markdown and retains its literal source, embeds saved input/tool images, and
keeps thinking in expandable sections. User text, tool arguments and results
remain literal. Raw HTML is inert; the document runs no scripts and loads no
remote resources. Markdown image URLs are shown as text, not fetched; ordinary
links remain user-initiated navigation. Plain text retains original Markdown and
image markers without image bytes. Export includes prompts, thinking, paths,
shell commands, tool output and, in HTML, saved images—even output withheld from
model context. Review the artifact before sharing it. Export is a local snapshot,
not an upload or a restorable Session backup.
In the terminal, `!command` runs a shell command in the live working
directory and shares the command and its outcome with later model context.
`!!command` runs it without sharing the command or outcome with the model.
Both commands remain visible in the saved Session. Shell input refused during
another operation stays in the editor with its literal syntax and cursor intact.
The terminal shows bounded
stdout/stderr previews, observed exit/signal and cancellation/timeout status, capture notices
and the `!!` sharing choice; Ctrl-O inspects the full stored result.
Before dispatch, the Session commits the command and sharing choice, then holds
an exclusive direct-shell permit through observed-result commit. Ctrl-C requests
cancellation; a command that started can still have external effects. If Ion is
killed or the result cannot be committed, reopen, inspection and export show the
command with an unknown external effect, not an invented exit status. The next
explicit coding or shell admission closes that interruption atomically without
rerunning it. `!!` interruptions remain excluded from model context.

Ion loads global `AGENTS.md` defaults from its configuration root
(`ION_HOME`, normally `~/.ion`), then rules along the working
directory's filesystem ancestor path. Standing rules refresh on resource reload
or reopen, without changing already admitted inputs. Native file/directory tools
also discover rules for their explicit physical targets. New, changed or removed
scoped rules defer native writes/edits until a later model request can see them;
reading inside Code Mode without selecting the instruction context is not model
delivery. Shell strings and custom tool arguments are not inspected for paths.
These rules are guidance, not a sandbox or atomic isolation from external changes.
Unreadable, invalid UTF-8 and oversized files produce source-specific omissions.
Files are included whole, up to 64 KiB each, within a 128 KiB standing/scoped
instruction context including bounded omission notices. Closest files get budget
priority; included files render global-to-local. A nested linked worktree's copy
shadows the main checkout's project copy of the same file.
`ion use` also accepts a custom model with
`--endpoint URL --wire chat-completions`, `--wire anthropic-messages`, or
`--wire openai-responses`. With `--endpoint`, the model ID is literal, even when
it resembles shorthand for an existing route.
Custom provider identifiers contain only lowercase ASCII letters, digits and
hyphens, with 1–64 bytes; the same identifier is used for provider login.
`URL` may be a compatible API base such as `http://desktop:8080/v1` or the
complete `http://desktop:8080/v1/chat/completions` request URL. Ion appends
the standard wire path only when the URL is a base.
When configuring a custom route, add `--context-window 65536` and
`--max-output-tokens 4096` to declare its capacity and output ceiling. These
positive token limits are saved with the route and restored on model selection
and Session reopen; catalog limits cannot be overridden. An omitted custom
context window stays unknown; omitted output keeps the 8192-token ceiling.
The shared agent clamps output to remaining estimated capacity. Its 8192-token
headroom reserve is capped at half the declared window, so small windows can admit
small requests rather than losing their entire capacity to the reserve. Instructions
and history can still make a request too large; the full coding loadout is not
guaranteed to fit a small window. Unknown keys in saved route configuration are
rejected rather than silently ignored.
For a custom OpenRouter model, use `--wire openrouter-chat`. This route retains
plain reasoning or ordered structured `reasoning_details` across tool calls;
the generic `chat-completions` route does not assume that contract. Qualify a
new model with a tool-using turn before relying on it for coding.
Built-in OpenAI models use the Responses API without custom route configuration:

```sh
ion use openai gpt-5.5
```

The catalog supplies the endpoint, image support and token limits. Use
`OPENAI_API_KEY` or `ion login openai` for API-key access; ChatGPT/Codex subscription
login is not implemented. Custom endpoints can still select `--wire openai-responses`
or `--wire chat-completions` under a custom provider identity.

Saved OpenAI selections also use Responses on reopen. Ordinary Chat Completions
history is projected into Responses input; incompatible native replay is rejected,
not silently discarded. Explicitly compact or start a new Session if that occurs.
Responses requests send full history with `store:false`; no server-side
conversation is required. Native message phases, refusals and encrypted reasoning
survive tool continuation and reopen. Readable reasoning is distinct from opaque
replay. Calls execute only after a terminal response; failed or truncated calls
cannot authorize a tool effect. Function tools retain their existing optional-field
semantics (`strict:false`); namespaced/hosted tools are not mapped to local tools.
Responses requires at least 16 output tokens and permits bounded full-output SSE
frames up to 8 MiB; other wires retain their 256 KiB frame limit. Independent decoded
output/replay and stream-work bounds still apply. This route is qualified against a
local protocol fixture, not a live OpenAI account.

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
Anthropic and Responses encoding alias unsigned migrated tool IDs when their
format or a collision requires it, keeping each call paired with its result. Aliases are
wire-only: committed IDs, other chat wires and retained native replay are
unchanged. Appending history does not rebind an earlier encoded prefix; a collision
that would require renaming retained replay reports an incompatibility instead.
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
context advances, and is capped to one hour of active work. Policies without a
positive refresh point inside that hour—including cache lifetimes of ten seconds
or less—do not warm. Expired cache windows and elapsed active horizons never
restart warming after a delayed response. Refresh usage is stored as a cache_warm
Session accounting fact but never enters model context or normal transcript/export
output. Idle cache warming is not implemented.
Ion avoids compaction during a signed tool continuation; if its prefix changes
or cannot fit, the Turn fails without repeating a tool effect. Context-window
estimates fall back to neutral content counted once, without duplicate opaque
replay. Later requests can use total input usage from a compatible committed
assistant exchange to refine capacity estimates. Samples are Session-local and
require the same live model service, effective model, instructions, tools and
reasoning; images must match as well. Context/model changes, compaction, replay
reset, reopen and fork/clone discard the sample. Missing usage never means zero.
Known input plus its assistant's reported output establishes a context baseline;
only later messages are estimated. This avoids treating duplicate replay JSON as
provider input. Incomplete usage creates no new baseline; later opaque messages
without a known baseline use the fallback. Summary/cache-warm usage does not
calibrate normal requests. New image sets retain the conservative first-contact
image allowance. The independent encoded request bound still includes retained
replay. Neither estimate is an exact provider-token count or transport-wire limit.
Provider SSE frames are limited to 256 KiB. Newly retained decoded output,
including opaque replay, has an 8 MiB admission budget. Independent transport
bounds allow 64 MiB per response and 8 MiB without retained-state progress.
These are resource guards, not token limits: extreme fragmentation can still
reach the transport bound before the model's output allowance.
For a llama.cpp server whose model emits unreplayable reasoning, use
`--wire llama-cpp-no-thinking` to disable it on each request. Custom HTTP or
HTTPS endpoints can run without a key. Pass `--api-key-env NAME` or run
`ion login PROVIDER` when the endpoint needs one. An HTTP endpoint sends any
configured key in cleartext; use HTTPS when the endpoint offers it.

Attach JPEG, PNG, GIF or WebP files with `ion --image PATH run "PROMPT"` or
`ion --image PATH chat`; repeat `--image` for several images. In chat,
`/image PATH` prepares a file for the next prompt without locking the editor.
Wait for attachment completion before sending; Ctrl-C cancels preparation while
retaining typed input. File preparation uses an owned blocking worker. Cancellation
and exit join it, but cannot interrupt an already-running decoder.
Ctrl-V reads the clipboard on the host running Ion: copied files enter as paths,
copied image pixels attach to the prompt, and otherwise text is pasted.
Clipboard reads and normalization run in a helper process with a three-second
deadline. Ion stops and reaps it on timeout or terminal exit. Helper transfers
are limited to 32 MiB before editor/input limits apply; this is not a process-memory bound.
Enter steers attached images during a running Turn; Alt-Enter queues a separate follow-up. A terminal's
ordinary text paste still works. Relative paths resolve in
the Session's working directory. Ion decodes and checks the file, applies
image orientation and resizes large images before accepting the Turn. Source
files are limited to 32 MiB, and each inline image to 5 MiB within the
current 8 MiB request bound. A resize note gives the model the sent dimensions.
Image bytes are stored in the Session so follow-up requests can still see
them after the source file changes. CLI and RPC `inspect` replace typed image
payloads with omission markers. Arbitrary JSON in tool arguments/results, schemas
and provider metadata stays intact, even when its keys resemble image fields;
inspection is not a general-purpose secret scrubber. A custom endpoint needs
`ion use ... --images` to declare that its model accepts image input.
The model's `read` tool can also open a workspace image and return it as a
typed attachment. It uses the same normalization and size bounds as a user
attachment. Tool images remain in Session history for replay. Text-only routes
receive an explicit withholding notice; that does not change the saved tool's
observed success or discard its attachments.

Ion also discovers Agent Skills from `~/.agents/skills/`,
`~/.ion/skills/` (under `ION_HOME` when overridden) and project
`.agents/skills/` directories from the working directory up to its Git root.
Each skill is a directory with `SKILL.md` and valid Agent Skills frontmatter.
Only its name, description and path enter the standing model instructions;
the model can read the full file when relevant. Use `/skill:NAME [request]` to
load it explicitly. Prompt templates are direct `.md` files in
`~/.ion/prompts/` (under `ION_HOME` when overridden) or project `.ion/prompts/`; `/NAME [arguments]`
expands one before submitting it. Templates support `$1`, `$@`,
`${1:-default}` and `${@:N:L}` argument forms; quote an argument containing
spaces. `ion resources` lists both, and the TUI has
`/skills`, `/prompts` and `/reload`. Slash-command discovery uses this same
loaded inventory; `/reload` refreshes suggestions. Native terminal command names
take precedence over same-named templates in the terminal client. Starting,
cloning, forking or switching a
Session refreshes resources; `/reload` refreshes the current Session. Personal
resources take precedence over same-named project resources; nearer project
directories take precedence over ancestors. Invalid resources are skipped with
a diagnostic. Recognized resource arguments and expanded text must fit the
current request byte limit; expansion refuses oversize input instead of truncating
it. The terminal retains the literal draft and attachments; headless/RPC clients
receive an error before a Turn is accepted. Core still checks the complete
encoded message and attachments separately. Project resources are lower-trust repository text
under the same live-directory tool permissions as `AGENTS.md`; review
unfamiliar resources before using them.

Executable user commands are separate from skills and prompt templates. Put a
`commands.json` in the user configuration directory, or `.ion/commands.json` at
the nearest Git root (the working directory when there is no Git ancestor):

```json
{
  "commands": {
    "check": {
      "description": "Run the workspace tests",
      "program": "cargo",
      "args": ["test", "--locked"],
      "share_with_model": false
    }
  }
}
```

`share_with_model` is required. In the terminal, `/commands` lists enabled names,
descriptions and origins; `/command:check [arguments]` runs one. Discovery and Tab
completion do not execute it. Quotes and escapes group literal arguments, without
variable, glob or command substitution. Fixed manifest arguments precede user
arguments. Execution uses the Session's working directory and existing user-shell
admission, capture, cancellation and recovery. It records the actual launched
command and outcome, not a fabricated model tool call or user prompt. With sharing
false, both stay out of later model context; true shares them with the next model
request, without starting a request itself.

Project commands remain disabled until `ion commands trust` approves their current
manifest bytes and canonical root. `ion commands list` works without model setup;
`ion commands untrust` revokes only command trust, independently of `ion trust`
for MCP. Trusted project commands override user entries with the same name.
Manifests are strict JSON, limited to 1 MiB; duplicate command names are rejected. Editing a loaded source, deleting it
or revoking project trust rejects new launches; review, trust project changes and
`/reload` before running the new definition. Already admitted work keeps its
captured route. Reloading, listing or opening a repository never launches a command.

Commands use host permissions, not a sandbox: the manifest digest does not freeze
scripts or transitive executable code, and a program can interpret its own argv.
Read-only **model** tool selection does not revoke explicit user-shell/command
authority. Custom applications opt in through `Application::commands` (empty by
default) and user-shell authority; neither alone permits execution.
Commands cannot be queued as follow-ups. Headless/RPC prompt submission does not
execute terminal commands; embedded clients can explicitly use shared control's
`command` method. This is named execution, not arbitrary extension handlers,
lifecycle hooks or UI plugins.

Add local MCP tool servers explicitly with `ion mcp add NAME COMMAND [ARGS...]`.
Server arguments can start with flags, as in `python3 -c '...'`. Use
`COMMAND -- ARGS...` when a server's first option overlaps an Ion option such
as `--cwd`.
Local servers can use their own working directory and environment overrides:

```sh
ion mcp add docs --server-cwd ../docs --env 'DOCS_TOKEN=${DOCS_TOKEN}' docs-mcp
```

These persist as `cwd` and an `env` name-to-string map in the server entry.
Relative directories resolve against the Session directory, not the manifest's
location; absolute paths and `~` / `~/...` use the parent's HOME. Without `cwd`,
the server runs in the Session directory. Environment overrides supplement the
inherited environment; repeated `--env` names use the last value. Quote templates
to keep your shell from expanding them before Ion saves the configuration.
`${NAME}` reads the parent environment; `${NAME:-fallback}` supplies literal text
only when the variable is unset (a present empty value stays empty). Overrides
cannot reference each other, and inserted values are not expanded again.
`$NAME` and `!command` remain literal: no shell commands are evaluated. Command
and argument strings also remain literal. The combined override names and
expanded values are limited to 1 MiB. Bad references or an unusable directory
disable that server with a diagnostic; other servers can still start.
`ion mcp list` shows the configured directory and environment names, never their
values. Prefer references for secrets rather than saving literal credentials.
Changes apply when a new coding client starts and require renewed project trust.

Use `ion mcp add-http NAME URL` for a Streamable HTTP server; add
`--bearer-token-env VARIABLE` when it requires a bearer token already set in
the environment. For other credentials or metadata, use repeatable headers:

```sh
ion mcp add-http remote-docs https://example.com/mcp \
  --header 'X-API-Key=${DOCS_KEY}' --header 'X-Tenant=${DOCS_TENANT:-default}'
```

These persist in a `headers` name-to-string map. Values use the same non-recursive
environment references as local overrides, captured once before connecting.
`Authorization` is supported, but cannot be combined with `--bearer-token-env`.
Repeated flag names use the last value, ignoring case; manifest keys differing
only in case are rejected. Empty values are allowed. Header names plus resolved
values (including bearer credentials) share a 1 MiB limit. Invalid or missing
values reject that server before network access. HTTP framing and MCP-owned
session/protocol/routing headers cannot be overridden, including `Host`,
`Content-Type`, `Accept`, `Mcp-Session-Id` and `Mcp-Protocol-Version`.
`ion mcp list` shows header names only. Headers reach POST, GET and DELETE, and
Ion refuses redirects rather than forwarding credentials to another destination.
This does not add OAuth or command-backed secret providers.

Add `--exposure direct|deferred|hidden` to either command.
Project servers live in `<repository root>/.ion/mcp.json`; write them with
`ion mcp add --project ...` and remove them with `ion mcp remove --project NAME`.
Project configuration is inert until `ion trust` records its canonical project
root and current file digest. Editing the file invalidates trust; untrusted
entries are never connected. `ion mcp list` reports the project path and an
untrusted diagnostic. Trusted project entries override user entries with the
same name. A project entry without `command` or `url` can override only `enabled`,
`exposure` and `tool_exposure` of a valid user server, without copying its launch
settings or credentials. Other project entries define complete servers.
`ion untrust` removes the trust record. Trust is an explicit launch
decision, not process isolation or a sandbox.
`direct` is the default and declares the server's tools. `deferred` keeps them
callable but out of the initial model loadout until `tool_search` activates a
match for a later request; Code Mode can also call them. `hidden` exposes no
model-visible tools and omits the server's resources from the MCP resource
tools. Hidden is model exposure, not process isolation or authority: the server
is still configured and inherits the host environment if started. Exposure
changes apply to future requests, never an already-issued request. `ion mcp list`
and `ion mcp remove NAME` manage the saved user configuration; concurrent
mutations serialize across Ion processes.

To keep a server configured without starting it, use `ion mcp disable NAME`;
`ion mcp enable NAME` restores startup. Add `--project` to edit a project entry
or create a policy override for a user server, then re-trust changed contents. Both transports also accept `"enabled": false`
in the manifest; omission means enabled. Listing marks disabled entries while
retaining their exposure and tool rules. Disabled entries remain validated but
are skipped before environment/credential resolution, process creation or HTTP
access. A trusted disabled project entry still overrides an enabled user entry.
These commands affect new clients only, not active connections or issued calls.
Project overrides retain explicit `true` as well as `false`. Repeating the same
saved setting leaves the manifest and its trust digest unchanged.

To select individual tools, edit the server entry in `~/.ion/mcp.json` (or the
trusted project file). `tool_exposure` overrides the default for names or `*`
patterns against the original remote name, not Ion's normalized aliases:

```json
{
  "servers": {
    "docs": {
      "command": "docs-mcp",
      "exposure": "hidden",
      "tool_exposure": {
        "read.internal*": "hidden",
        "read.*": "direct",
        "search": "deferred"
      }
    }
  }
}
```

Here `read.*` tools are direct except for the earlier `read.internal*` rule;
`search` is deferred and other tools stay hidden. Values are `direct`, `deferred`
or `hidden`. An exact name always wins over patterns; otherwise the first matching
pattern in file order wins, not the most specific pattern. Only `*` is special:
it matches any sequence of characters, while other punctuation stays literal.
Unmatched tools inherit the server default, including tools added by a later
listing. These choices are captured at connection startup; edit and start
a new coding client to replace them, and re-trust an edited project file.
`ion mcp list` shows the server default followed by rules in their configured
order. MCP add/remove and enable/disable commands preserve that order.
Tool overrides do not expose a hidden server's resources or widen `--tools` /
`--exclude-tools` selection. They do not stop the server from running.

Ion starts enabled configured servers when a coding client starts, discovers their
tools, and exposes each as `mcp__LENGTH_NAME__TOOL` alongside read, edit, write and
exec. `LENGTH` is the ASCII server-name length: `demo` uses `mcp__4_demo__TOOL`.
This delimiter keeps server/tool pairs distinct even when names contain `__`.
MCP tool names that need normalization or shortening receive a stable
hash suffix in the model-facing name; Ion calls the server with its original
tool name. Headless, TUI and RPC use the same tool set. Local server processes inherit
the user's environment and permissions, with configured environment/directory
overrides described above. A repository file does not launch an MCP server merely because
Ion opened that directory. A failing server is reported at startup while
healthy servers and the coding client remain available; a failed
tool call becomes an error result visible to the model. On controlled exit,
including failed startup or client preflight, Ion joins MCP services and HTTP
workers. It closes local server stdin, waits up to three seconds for the root
process to exit, then kills and reaps that root if needed. Service join and
local process cleanup errors are reported. The HTTP SDK still logs remote
session-deletion failures rather than returning them. Cleanup does not guarantee
termination of descendants or remote work, and abrupt process loss cannot
guarantee joined cleanup. Rust embedders must await `McpTools::shutdown` before
dropping their runtime. Cancelling that wait leaves the retained close running;
a later shutdown joins the same task instead of reporting an already-taken
connection as clean. Incoming messages are bounded before decoding: one stdio
JSON line or HTTP JSON/error body may be at most 64 MiB, and an SSE event stays
within the transport's 16 MiB default and that 64 MiB ceiling. Oversize input is
rejected rather than truncated into a different observation. Tool and resource
errors preserve the framing reason, including during concurrent shutdown. A rejected stdio
line or SSE event retires that connection, so Ion cannot resume and replay it;
an oversize HTTP JSON/error body fails that request. Neither case reconnects and
repeats a possibly executed call, and lost transport errors surface after one
bounded retry instead of retrying forever. Local server
stderr goes to a private `0600` temporary file retaining the first 64 KiB while
the rest of the stream is drained so logging cannot block the protocol. Nonempty
captures appear as `server_stderr` metadata with the path, byte counts and any
capture error; startup diagnostics name servers that logged, and nothing is
written directly to the terminal. The capture stops and joins with the root
process, so descendants holding the pipe cannot stall shutdown.

MCP tool results deliver text, structured data and normalized images, including
supplied embedded resources. Embedded text and text-like blobs become text;
embedded images use the same validation and normalization as inline images.
Other blobs are saved to private temporary files, with URI, MIME type, byte size
and `savedPath` metadata in the result's text. Resource links expose their known
metadata but are never fetched or treated as local filenames. Embedded content
is supplied data, not an implicit resource read or an App UI runtime.

An audio block, invalid image or undeliverable blob is omitted without discarding
the rest: its index and the conversion reason appear under `conversion_notices`
(bounded, with `retained_blocks` and `omitted_blocks` counts). The server's own `isError` stays
the outcome flag, so a successful response is not rewritten as a failed call,
and no rejected image enters model context. When anything is omitted, the
original response is written to a private `ion-mcp-raw-result-*.json` named by
`raw_result_path`; a write failure reports `raw_result_error` instead.

Servers that advertise resources also expose `list_mcp_resources`,
`list_mcp_resource_templates` and `read_mcp_resource`; these tools are added
when at least one configured server advertises resource capability.
Omitting `server` lists every resource-capable server with at most 64 pages per
server, while a `server` argument returns one page and an opaque `cursor`
continues it. MCP App UI resources (`ui://` or `mcp-app` profile HTML) are
omitted from listings. A read returns text and normalized images; text-like
blobs (`text/*`, JSON, `+json` or `+xml`) become text, and other binary content
is saved to a private temporary file whose path is returned. Resource
observations use the same bounded inline result and private full-output snapshot
as tool results. Tool and resource conversion, including private-file writes,
runs off async workers and remains joined if cancellation arrives after the
response. Resource operations are read-only and are never retried; a cancellation
that cannot flush within three seconds retires that connection.
Large text or structured results include a bounded preview and a
`full_output_path` to a private JSON file containing the complete result;
oversized structured content is omitted from the inline preview and marked
`structured_content_omitted`; `read` can inspect the full output in ranges.
Each invocation sends one `tools/call`; `input_required` continuations and task
results report an unsupported-result error rather than issuing hidden follow-up
calls or task polling. Cancellation notifies the original request and joins
local issuance. If that cannot flush within three seconds, Ion retires the
connection, joins its local work and removes its tools from later catalogs.
Other pending calls on that connection fail rather than being rerun; restarting
the client reconnects configured servers. Neither cancellation nor connection
closure proves that remote effects have stopped.
When a server announces a changed tool list, Ion refreshes that server before
the next model request. A failed refresh reports a warning and keeps its last
known list. Remote MCP OAuth login is not yet supported.
For Rust embedders, `CodingApplication::new(host, tools)` adds an optional custom
`CodingToolSource` to the coding tools; a same-name registration replaces that
built-in. Restricted applications instead supply a complete `Application` recipe.
A source publishes `ToolRegistration` values that bind a `ToolDefinition` to a
`ToolExecutor`. Refresh publishes new registrations; an issued request retains
its original executor, including an MCP connection and remote tool name. This
freezes local dispatch identity, not the remote server's implementation.

Executors receive a `ToolInvocation`: the original call, stop token and optional
`ToolOrigin`. Engine-issued origins identify the logical Session, Turn and
committed assistant/tool occurrence; Code Mode children also carry their committed
child index. Reused provider call IDs and model arguments do not supply this
identity. Fields are read-only. Core does not inject provenance into guest or
MCP arguments; applications control what their tool outputs disclose. `ToolCatalog::execute_untracked` and
`ToolInvocation::untracked` explicitly omit admission and origin. Provenance is
not a live lease, capability grant, replay permission or evidence of settlement.

Rust applications can prepare a child with Host's
`SessionDelegation` and request it through `ToolInvocation::delegate`. Its
private live admission port is separate from the historical origin. The complete
child application, authority assertion and environment are explicit.
`recipe.prepare(input, &stop)?` consumes the recipe and returns a retained
`DelegationPreparation` without blocking the control executor. Await its
`join().await?` to obtain `PreparedDelegation`: pass `task` for foreground work,
retaining a clone of `steering` when another caller needs to guide it. Its
`steer(&request_key, &message, &stop).await` waits for readiness without starting
work itself, then queues literal input through the child's shared control.
`startup_timeout_ms: Some(ms)` sets a positive budget from preparation start for
workspace checks and resource/runtime capture; `None` leaves policy to the
application. Expiry requests cancellation and joins started preparation, even if
a blocking application hook outlives the budget. Local `request_stop()` and Drop
do not cancel the parent token. Cancelling a borrowed join retains the worker;
resume joining to retrieve its result. Stop before result publication revokes a
retained cold task, including one whose preparation has already completed.
`DelegationPreparationError::TimedOut` is a pre-admission failure, not a child
execution timeout: no managed child is admitted, and any late failure remains in
the error chain. Trusted application panics remain panics. A startup budget does
not roll back effects performed by an application hook.
`execution_timeout_ms: Some(ms)` sets a positive runtime budget from linked child
request submission, after materialization/preflight; `None` leaves that policy to
the application. Progress and parent waits do not refresh it. Expiry requests
ordinary stop, then joins control and native process cleanup; cleanup may outlive
the budget. The joined relationship records `execution_timed_out` independently
of execution or infrastructure errors, without rewriting the child's Turn end or
inferring timeout from owner loss. Custom `DelegationTask` callbacks return
`DelegationCompletion` and must attest/enforce the same lifecycle contract.
Choose `DelegationTarget::Fresh { directory }` or `Fork { directory, point }`,
with `ForkPoint::BeforeTurn(n)` or `ForkPoint::AfterTurn(n)` selecting the actual
parent's committed prefix. An unfinished Turn is not an after-Turn boundary;
an invalid choice is refused before intent or child files. To reuse a settled
conversation, use `DelegationTarget::resume(path)?` with a new explicit recipe
and its original cwd. This resolves a locator/identity, not a continuation grant;
Core acquires its exclusive writer and rechecks identity, workspace and settlement
before committing new intent. Active Turns, pending shells or unresolved owned
background work require explicit reconciliation first.

Forking copies historical conversation/context facts, including instructions,
tool output and opaque provider material. Applications must authorize that
disclosure; a fork is not secret scrubbing or isolation. Current instructions,
model/reasoning, callable tools, cwd and credentials still come from the explicit
child recipe, not the copied configuration. Matching deferred definitions in that
recipe can regain exposure from copied context; parent executors are never added.
Entry coordinates and complete compaction cuts are preserved. The new child link
and explicit preferences append after the prefix in the header's initialization
transaction; inherited delivery keys, continuation grants and child ownership
are not transferred.

Core records a distinct `DelegationRecord.run` for each managed request and its
stable `child` conversation UUID before materialization. Resume appends a new
binding and request to the same Session; old receipts, request keys, context
alternatives and outcomes remain intact. Current instructions, routes, credentials
and grants come from the new recipe, never saved ancestry. The latest
`parent_delegation()` is historical request provenance, not live authority.
Tasks default to foreground: parent stop propagates, and supervision joins shared
control and started work before parent settlement—even if the tool abandons its
wait. Exact repeated specifications reconcile; different work, ownership mode or
owner epoch at that invocation conflicts. `SessionView::delegations()` and
`parent_delegation()` expose typed relationships without parsing tool prose.

For bounded foreground joining, retain a Host `BackgroundOwner` and use
`prepared.in_background_after(&owner, Duration::from_secs(60))` after joining
preparation.
Core reserves capacity before intent, then joins until completion or that budget.
Full completion wins a ready expiry race and releases the reservation without a
background report. At expiry the same pinned execution transfers, retaining its
run/request, live control and original execution deadline. Parent stop before
transfer still stops and joins it; abandoning the result waiter retains foreground
joining, not an implicit handoff. A refused/failed transfer also stops and joins
already-started work before release. This budget includes child setup after live
admission, not separately budgeted recipe preflight; zero selects immediate
transfer.

For background work, retain a Host `BackgroundOwner` and choose its live
destination: `prepared.in_background(&owner)` after joining preparation. The
coding application owns one owner for its lifetime, shared across runtime reload
and binding replacement; a child recipe does not capture it. Core reconciles
exact repeats first, then reserves Host custody before admitting new work. Even a
full or closed owner permits reconciliation of an existing transfer. The
destination installs a retained operation and Core commits
`DelegationTransferred` before the parent receives its transfer receipt. That
receipt is not child settlement or liveness. Cancellation observed at transfer
admission refuses cold work; a racing cancellation may lose to that admission.
After transfer, parent cancellation and parent-Session shutdown do not stop the
child. Explicit `cancel` stops one child; application shutdown closes admission,
stops its children and joins their full Host controls. Drop requests stop; finish
`join_shutdown().await` before destroying the runtime, which returns the retained
infrastructure error. Cancelled maintenance/shutdown waits retain tasks and
results. `maintain()` joins without consuming; `wait(&ids, timeout)` returns
all selected statuses when any job joins or the shared budget expires, without
acknowledging them. The `BackgroundWait` result includes `wait_expired` and
`runs` in requested order; zero duration polls already-retained joins.
An empty, duplicate, unknown or acknowledged selection fails, even alongside a
ready result. `run(id)`/`jobs()` inspect retained status by run ID;
only `take(id)` releases a joined result's capacity, returning its complete
final snapshot atomically, including steering reports. An infrastructure error
returns `BackgroundRunFailure` with both its `run` snapshot and the typed cause.
The receipt retains any committed joined outcome, including timeout, runtime and
partial output, even alongside an infrastructure error. If parent settlement
failed, it remains unconfirmed. Acknowledgement does not reopen a failed owner.
Capacity bounds reservations, running jobs and unread results together (1–64 slots), not history,
encoded bytes or RSS. Changes are published through a revision plus notification;
clients still reconcile snapshots rather than treating notification as delivery.
Snapshots are privileged custody views, not an authenticated remote service or
atomic Core/Host snapshots.

For an explicitly authorized parent or peer receiver, choose a live policy with
`SessionSetup::with_completion_delivery` or `SessionControl::set_completion_delivery`,
then obtain `receiver.completion_target(CompletionDelivery::Wake)?` (or a weaker
mode). Attach it using `PreparedDelegation::with_completion_target` before either
background helper, or `BackgroundDestination::with_completion` for manual assembly.
The target retains no Session writer and cannot be reconstructed from saved IDs.
A restricted receiver keeps its own tools/model/authority; it does not inherit the
producer's coding recipe. Drive its ordinary shared control and select the
read-only `completion_changes().changed(revision)` invalidation while idle, as in
the [service example](crates/ion-app/examples/service.rs). Do not start a second
loop or fabricate user input. Coding's first consumer targets the initiating
parent automatically; arbitrary peer registration/transport is not built into
CLI/RPC, and no cross-process broker is supplied.

`owner.steer(id, &request_key, &message, &stop).await` uses the grant captured
with that actual run, checked against both its Session and request identity.
An old run's stop or control grant cannot follow a later run in the same Session. Live request keys reconcile identical input within that
child's retained controller; conflicting input fails. Keys are not authority or
durable replay permission after owner loss. Status snapshots include `steering`
reports with local input IDs and `queued`, `committed`, `returned` or `unknown`
states. Committed means saved Session input, not proof a model request consumed or
understood it. Returned reports preserve original literal input and its reason;
control lost before joining stays unknown. Reports share the Host's 64 input/result
allowance, including the root, rather than adding another input queue. Queue and
status changes invalidate the owner without retaining a borrowed waiter.

For another shared-control consumer, `SessionControl::steering()` returns a
`TurnSteering` grant for its current model operation. `submit(message, metadata)`
uses ordinary custody and returns a `SteeringReceipt` with `status()` and `changed()`.
It cannot start another Turn or follow the control into a replacement binding.
Dropping a receipt never withdraws admitted input; the controlling owner still
drives transfer, stop, return and joining.

A foreground child's actual original answer is retained with its joined
relationship only after full Host/Core joining and a completed requested Turn.
The Host callback returns that observed text; Core does not reopen the child
database or reconstruct a report. Execution, cleanup or storage failure clears
the answer rather than presenting it as success. A background transfer
acknowledgement is not a result; use wait/status and the same retained outcome.
While a transferred job retains its parent writer, an application rebinding the
same Session reuses that retained handle rather than opening a competing writer.
Unread results do not pin the writer after settlement.

Relationship storage failure blocks dependent execution before release, including
a Code Mode guest that catches the error. Later infrastructure failure or panic
closes the destination owner and requests its other children's stop, independently
of output readers. It cannot retroactively change an acknowledged parent result.
Joined ordinary child failures remain available in receipts. Panic and process
loss do not prove effects stopped.

After owner loss, inspection is passive. The next explicit new-work admission,
direct shell or authorized parent-Turn continuation closes orphan relationships
as unknown before dependent work—even if the original parent Turn already ended.
A surviving transfer task retains the parent writer lease and is not closed by
new parent work. Copies retain foreign history but acquire no child ownership.
Recovery neither restarts the old child nor rewrites its missing observations;
an owner UUID is not authority to adopt or resume it. New model-issued delegation
is new work, not an exactly-once effect guarantee. Live-model delegation
qualification remains open.

The coding preset includes `web_fetch` for selected HTTP(S) URLs, including local
and internal sites. It returns untrusted UTF-8 text or HTML converted to readable
text, with final URL, redirect chain and source digest. Truncated text has a private
temporary `full_path` for `read`; this is not a retention guarantee. Downloads are
bounded; unsupported content and failures are explicit. No browser cookies,
provider credentials or ambient proxies are imported. It also works through Code
Mode and the read-only loadout; GET does not imply replay safety.

`web_search` prefers the active provider: OpenAI Responses search for an OpenAI
profile, Anthropic Messages search, or DeepSeek's Search API. It preserves the
profile's endpoint prefix and credential resolver. Model-mediated search uses the
selected model; DeepSeek Search is standalone. Other routes use the
first configured Exa, DeepSeek or Brave fallback, in that default order; this is
an operator-reorderable tie-break, not a quality ranking. Set `EXA_API_KEY`,
`DEEPSEEK_API_KEY` or `BRAVE_API_KEY`, or use `ion login PROVIDER`. Search sends
only the explicit query, not conversation history or files. Results include
source URLs, excerpts where supplied, response digest and reported usage. Native
search may bill model tokens and search fees; its usage is in the tool result,
not the main model's token totals. There are no post-dispatch retries, redirects
or provider fallbacks, and no ambient proxies. Unsupported search routes and
missing credentials fail explicitly.

User-only `~/.ion/settings.json` accepts `search.native` (default `true`) and
`search.fallbacks`, for example `[{"provider":"deepseek"},{"provider":"exa"}]`.
An empty list disables direct fallbacks. Each fallback may set `endpoint` and
`api_key_env`; these are operator settings, never model arguments. Search works
through Code Mode and read-only selection; `--exclude-tools web_search` removes
it from both direct calls and the composed namespace.

Filesystem and command tools act directly in the working directory with the host
user's permissions. There is no implicit sandbox. Settlements distinguish observed host output,
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
POSIX sh, with closed stdin. The coding binding captures the launching environment;
it does not reload login startup and overwrite an activated toolchain/virtualenv.
Optional user-only `~/.ion/settings.json` (under `ION_HOME` when overridden)
configures a Bash-compatible executable and explicit per-command setup:

```json
{"shell":{"path":"/bin/bash","setup":"export CI=1"}}
```

Setup runs in the command's shell, before the requested command; nonzero setup
stops that command. Setup stderr is suppressed to avoid recording private code
in evaluator errors; failures expose status and a generic diagnostic. Inherited
tracing is suspended for setup and restored for the payload. Setup stdout is
ordinary command output. It may perform effects, so use trusted, noninteractive,
Bash-compatible code. The same captured policy serves `exec`, `!`/`!!`, named
commands, managed pipe/PTY programs and coding children. Each invocation is a fresh
shell; exports from one invocation do not change another. Changed settings apply
to newly prepared bindings, not active/queued work or existing programs; restart
to refresh an existing binding. Resource reload alone does not reload shell settings.
Libraries/services can supply `CommandEnvironment` explicitly through
`with_command_environment` or the native tools' `with_environment` constructors,
without reading user shell settings. This is not a durable environment snapshot
or an OS isolation boundary; recovery still requires Host authority attestation.
Use `process` for managed background, pipe-interactive or PTY-backed programs,
including bounded terminal screen inspection. Managed subagents are separate agent
Sessions, not process-session handles. Foreground commands have no default
timeout; pass `timeout_ms` when a deadline is needed. The deadline
covers output capture even after the direct shell exits. On timeout, Ion requests
process-group stop and stops acquiring output, then awaits started capture I/O.
This is not a hard wall-clock or all-descendant termination guarantee.
Omit optional native input fields to use their defaults; explicit JSON `null`
is not a valid integer or string option. Output metadata may still contain `null`.
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

A damaged Session file is skipped by `sessions` and `--continue`, while opening its exact path reports the
error. Reopen and catalogue/startup metadata validate facts one at a time without
retaining all raw child output. Metadata inspection also releases historical model
messages instead of retaining the full conversation. Writable Sessions still retain
model messages; full inspection, detailed terminal history, clone and fork
materialize raw history. Large records and these retained histories can require
substantial memory. There is no aggregate Session memory quota.

Ion summarizes settled history when its request
nears a known model's context window or exceeds its transport bound, and can
retry one model request after a provider reports context overflow. Classification
uses admitted upstream error text even when its displayed diagnostic is clipped.
The raw conversation remains inspectable; `compact` and `/compact` also trigger this
explicitly. Add literal focus with `/compact Preserve API decisions and failures`
or `ion --continue compact 'Preserve API decisions and failures'`. Focus applies
to every chunk and retry of that manual operation, not later automatic compaction.
It is not a new User Turn or execution permission, and `@paths`, `/commands` and
quoted text are not expanded. Empty or whitespace-only focus behaves like the
bare command. Focus shares retained-input and summary-request byte bounds; Ion
rejects oversized input rather than truncating it. Only the generated summary
and covered coordinates are committed as compaction facts, not a separate copy
of the focus. A refused chat command stays in the editor; after admission it is
consumed, even on Stop or failure. Use explicit editor recall to retry it.
Longer saved histories are summarized in bounded steps when one
summary request cannot fit. If a provider rejects a summary for context size before
any generated output, Ion retries strictly smaller settled prefixes on the same
model. `context_compaction_retry` reports the rejected `through_entry` and smaller
`next_through_entry` in JSONL; the TUI shows the same progress. Recovery stops if
the smallest cut is rejected or on cancellation. Failed attempts are not retried
after any generated output, including opaque reasoning. Each successful chunk
remains committed if a later chunk fails.

Summary requests frame the selected prefix as historical data and place the current
summary task after its records and images. They exclude opaque replay and human
thinking, and carry images as typed attachments when the summary model supports them. Text-only summary
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
A stop already pending when a model request would open prevents that dispatch,
including retries, summaries and cache warming; already-issued requests are not
rolled back. Retry events appear in the TUI and JSONL output. A response that stops
after producing partial output is not replayed silently.
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

## Tool veto

An optional operator program can allow or deny each admitted model tool call,
including `history`, deferred discovery, Code Mode parents and their tool children.
Configure it in user-only `~/.ion/settings.json` (or `$ION_HOME/settings.json`):

```json
{
  "tool_veto": {
    "program": "/usr/bin/python3",
    "args": ["/absolute/path/to/veto.py"],
    "timeout_ms": 30000
  }
}
```

The program receives one JSON object on stdin followed by EOF. Fields are
`tool_name`, original `arguments`, `call_id`, Core `origin` (Session, Turn,
occurrence and optional composed child), and canonical `cwd`. For example:

```python
import json, sys
call = json.load(sys.stdin)
reply = {"decision": "allow"}
if call["tool_name"] == "write":
    reply = {"decision": "deny", "reason": "Writes are disabled by operator policy"}
print(json.dumps(reply))
```

Exit successfully with exactly one allow/deny JSON reply on stdout. Extra fields,
invalid JSON, nonzero exit, I/O failure, Stop and expiry fail closed. The timeout
must be positive; omitted timeout defaults to 30 seconds. Replies are limited to
64 KiB; denial reasons must be nonblank and at most 4096 bytes. Reasons are visible
to the model and in saved facts; stderr is drained and discarded, not disclosed.
The captured shell/setup also runs for this program: keep setup stdout silent.
Ion ends the one-shot process-group scope and joins the direct child before
publishing a decision. Cleanup may outlast the timeout.

Policy cannot rewrite approved arguments/results, add capabilities or grant
replay. Denied calls persist as `not_dispatched`, including composed children.
The program itself runs with host permissions and may have effects: this is not
a sandbox, rollback or a guarantee about escaped descendants. Explicit operator
shell commands and named commands are separate authority, not model tool calls.

Each runtime captures settings and command environment; coding children inherit
that capture with their own cwd. Restart or replace the runtime binding to apply
changed settings; resource `/reload` alone does not replace them. Program and
transitive executable bytes are not frozen. `ion doctor` validates settings
without running the program or proving it can execute. Rust applications can use
`ToolPolicy` with `ToolSet::with_policy`, or explicitly opt into `ToolVeto`;
restricted/service recipes do not implicitly inherit coding policy. Interactive
approval dialogs, project hook discovery and extension packaging are not provided
by this consumer.

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

Terminal and bidirectional RPC coding offer `ask_user_question` within the current Turn.
A request contains `questions`: 1–4 questions, each with question text and 2–6
`{label, description?, preview?}` options. An optional `header` gives a short
context label. Set `multi_select: true` when multiple choices are valid; omission
defaults to single-select. The dialog adds **Type something.** for a custom answer
in either mode and **Next** for multi-select confirmation; do not author those
labels or **Other** as options.

For concrete single-select comparisons, an option's `preview` can contain Markdown,
code, diagrams or configuration. At 100 columns or wider, previews sit beside the
choices; narrower views stack them below. PageUp/PageDown scroll the focused
preview using the configured bindings. Changing choices returns to the top; a
choice without a preview remains available. Custom entry regains the full width.
Previews fetch nothing and execute nothing; terminal controls are displayed safely.
If the complete selected label or even one preview row cannot fit, resize before
confirming. Headers and previews are request metadata, not extra answer fields
or permission.

In the terminal, use Up/Down and Enter to choose. Multi-select uses Space or Enter
to toggle checkboxes without submitting; choose **Next** to confirm, including an
explicit empty selection. A custom answer replaces all checkbox choices and
supports Unicode editing and paste; Enter accepts it and Shift-Enter adds a
newline. A single question completes directly.

While entering custom text, Ctrl-G opens the same configured native editor.
Returning replaces only that answer’s draft; Enter still confirms it. Failure,
invalid UTF-8 or output exceeding the 4096-byte answer limit retains the original
text and cursor. Other answers and the composer are unchanged. The editor owns
keyboard input until it exits; Ion then restores the question at the current
terminal size. Configure or disable this shortcut as `app.editor.external`.
It does not open from choices, Review or a hidden question.

For multiple questions, Tab/Right and Shift-Tab/Left cycle through question tabs
and **Review**. Confirming the last answer opens Review rather than submitting.
Revisit any question to correct it; choices and custom text are retained. Enter
on **Type something.** reopens its text. Checkbox changes require **Next** again;
unanswered or edited questions cannot be submitted as a partial batch. On Review,
use Up/Down and Enter to choose **Submit answers** or **Cancel**; PageUp/PageDown
scroll the answers. Tab and cursor keys still edit text while custom entry is
open; Enter confirms it before tab navigation resumes. These navigation bindings
are configurable as `tui.question.next` and `tui.question.previous`.

Ctrl-] hides/reopens the question so you can read the conversation without
answering or cancelling. Drafts, cursor, choices and Review position are retained.
While hidden, typing, paste and Enter cannot edit the composer or submit answers;
PageUp/PageDown scroll the fullscreen conversation, and inline native scrollback
is available again. Esc still cancels the batch and Ctrl-C stops the Turn.
Configure or disable the toggle as `tui.question.toggleCollapsed` in keybindings.
Reopening requires the choices to be painted again before confirmation.

Esc cancels the whole questionnaire with an explicit cancellation observation;
the model may continue without an answer.
Ctrl-C stops the Turn. The dialog preserves the composer draft and waits for an
existing picker, detail view or attached PTY to release input rather than stealing
keystrokes or revoking a terminal lease.

Answers become ordinary committed tool observations, not steering, new user
messages, approval or permission. An answered result carries an `answers` array:
question ordinals are one-based, option indices are zero-based, and selected
labels come from the original request. Multi-select results use `kind: "options"`
with an `options` array of `{option, label}` entries in request order; an empty
array means none, not cancellation. A live responder lease and request nonce reject
stale/duplicate replies; requests carry their Core Session/Turn/tool occurrence.
Losing the responder while a question waits stops that Turn before releasing the
waiter. Shutdown revokes requests even if a caller retains its responder handle.
Pending questions are live-only: reopening a Session does not restart a dialog or
reinterpret an old request as authorization.

Requests are bounded to 64 KiB of serialized arguments. Question text is limited
to 4096 UTF-8 bytes, labels to 120 bytes and descriptions to 1024 bytes. Headers
are nonblank single-line strings of at most 16 Unicode scalar values before outer
whitespace is trimmed. A preview preserves its exact whitespace, must be nonblank
and is limited to 16 KiB of UTF-8; previews on multi-select questions are rejected.
These limits do not replace the aggregate 64 KiB bound. Custom answers are
nonempty and at most 4096 UTF-8 bytes each; the serialized answer batch is bounded to 48 KiB. Invalid answers leave the request available for a
corrected reply. Only one batch can wait per control; concurrent asks fail
explicitly. Without a connected responder, an explicitly installed question tool
returns unavailability rather than waiting or auto-answering. The tool also
requires a Turn admitted through shared control: a genuine Core origin alone does
not supply its owning Turn's stop authority, including inside Code Mode.

RPC emits `question_changed` with `resync: "get_question"` when the live view
changes. This is a coalesced invalidation, not the request itself: call
`get_question` to retrieve `data.question`, either null or an object with
`request` (a UUID nonce), `questions` (including any header/preview metadata) and
Core `origin` provenance. A notification can reflect an already closed request. The connection retains one pending
invalidation under output pressure; keep handling it independently of provisional
model progress or `publication_gap` records.

For a live request, substitute its nonce and submit one answer per question:

```json
{"type":"get_question","id":"q"}
{"type":"answer_question","id":"a","request":"QUESTION_UUID","answers":[{"kind":"option","index":1}]}
{"type":"cancel_question","id":"c","request":"QUESTION_UUID"}
```

An answer is `{"kind":"option","index":N}` for single-select,
`{"kind":"options","indices":[2,0]}` for multi-select, or
`{"kind":"text","text":"custom answer"}` for either mode. Indices are zero-based;
multi-select accepts an empty array but rejects duplicates and out-of-range values.
Do not send labels or other extra fields. `answer_question` and `cancel_question` return `disposition: "submitted"`:
the decision was handed to the live Host waiter, not necessarily committed yet.
Continue reading ordinary tool observations and `turn_end`. Cancellation dismisses
only the batch and may allow dependent work; use `abort` to stop the Turn.
Malformed, incomplete, stale and duplicate decisions fail without replacing a
valid pending request. EOF or broken I/O stops/joins the Turn; a new connection
cannot answer a historical request. Neither the nonce alone nor copied `origin`
fields grant authority without the connection's live responder lease.

One-shot headless execution and managed coding children omit this capability.
Rust applications can configure the same typed Host port and responder through
shared control. Terminal and RPC are consumers of it, not separate agent runtimes.
Explicit tool selection applies to direct calls and Code Mode as usual.

## Coding subagents

The main coding preset includes `subagent({...})` in run, chat and RPC. For
example, `ion run 'Delegate a self-contained coding task, then review it'` needs
no enabling flag. `CodingApplication` retains this policy through reload and
binding replacement; supplied external registrations can override coding defaults.
Restricted applications still choose their own complete `Application` recipe.

The tool has explicit actions: `start`, `list`, `status`, `wait`, `steer`, `cancel` and
`acknowledge`. `start` accepts a self-contained `task` of at most 64 KiB UTF-8,
optional `mode` (`foreground`, the default, or `background`), optional `cwd`
(defaulting to the parent workspace), and either `context` or `resume`.
Fresh context is the default; `{"type":"fork"}` copies the settled
committed prefix before the initiating Turn, not the active conversation, and an
explicit `point` may select an earlier `before_turn` or `after_turn` boundary.
Core validates and captures that cut in the admission transaction. The child
defaults to the initiating request's captured model route, including invocation-local
overrides, and its reasoning. Optional `model` selects an exact `provider/model`
from the known catalog or configured routes; optional `reasoning` accepts the same
values as `--reasoning`. For example, `{"action":"start","task":"Review the change",
"model":"deepseek/deepseek-flash","reasoning":"low"}` creates an independently
routed reviewer. It uses the chosen route's credential policy and may incur its
charges. There is no extra child-route allowlist. Omitted reasoning inherits even
with another model; unsupported effort or unavailable routes fail before child
admission, without fallback. Selecting the parent's exact identity retains its
captured route, not a later model-store edit. Overrides do not change the parent
or saved configuration. Start/status/wait receipts include actual model and effort.

A child uses the parent workspace unless `cwd` explicitly selects another
directory, absolute or relative to the parent. Project/home resources are loaded
for that workspace. Selected native and supplied external tools (MCP for the CLI),
the captured command environment and the application's Code Mode setting remain
inherited. It does not automatically enable another built-in subagent. Model
arguments cannot choose tools, endpoints, credentials or arbitrary authority.

Foreground `start` joins for up to `join_timeout_ms` (default 60000; 1–600000 ms).
It reserves application capacity before starting and includes child setup after
live admission, not separately budgeted startup preflight. Full completion returns
the joined outcome with `join_expired: false`, without transfer or a retained slot.
If still joining at expiry, it transfers the **same run**, returns `join_expired:
true` and `joined: false`, and continues under the returned `id`. Use wait/status;
never relaunch it because the join expired. This does not stop work or reset its
execution deadline. Before transfer, parent stop still stops and fully joins it;
cleanup may outlast the join budget. Background `start` rejects `join_timeout_ms`
and transfers immediately. Either transfer is acknowledged only after committed
responsibility passes to the coding application's owner; it is not a
completion claim and survives parent stop, parent-Session switch and reload, but
not application shutdown. `list` and `status` inspect retained application jobs;
`cancel` explicitly stops one child; `acknowledge` releases one joined result's
capacity without deleting its committed facts.

Every receipt has an `id` for the run, a stable `child_session` conversation UUID
and its `path`. `status`, `steer`, `cancel` and `acknowledge` take `id`; waits take
`ids`. Several runs can share one conversation while retaining independent
reports and controls. Each unread run still occupies its own custody slot.

Use `{"action":"start","task":"Check the remaining edge case","resume":"/absolute/child.sqlite","cwd":"/original/workspace","mode":"background"}`
to start a new run in a saved conversation, including after acknowledgement or an
application restart. `resume` and `context` are mutually exclusive; relative paths
resolve against the parent workspace. `resume` and `cwd` accept at most 4096 UTF-8
bytes. Resume requires the original workspace, a free exclusive writer and settled
history; active Turns, pending shells or unresolved owned background work are
refused before new intent. It does not recover an unfinished child or replay old
effects. Existing child Sessions are already durable; there is no persistence flag.
Model, reasoning, resources and grants come from the current call's captured recipe,
not saved preferences or ancestry. No old run is overwritten or implicitly
acknowledged, and its controller cannot steer the new run.

Use `{"action":"steer","id":"<run-id>","message":"Focus on the parser"}`
to guide an owned running background child. Guidance is literal, nonblank text of
at most 64 KiB UTF-8; it does not expand commands/resources, change the captured
route or authority, interrupt current tools, reset a deadline or start another
Turn. It joins the existing safe model-step boundary. The reply's `steering.status`
is initially `queued`, not committed delivery; `status` and `wait` retain its input
ID and subsequent `committed`, `returned` or `unknown` status. A returned report
contains the original uncommitted input. Guidance cannot be recalled after
admission, even if the parent stops waiting. Cancelling readiness leaves the child
running. New guidance to a stopped, settled, unknown or acknowledged run fails;
it never implicitly resumes it. Named persistent conversations and unfinished-child
recovery remain unfinished.

`start` accepts `startup_timeout_ms`, defaulting to 30000 (30 seconds), with
1–300000 milliseconds allowed. One budget covers model-route lookup, resume
locator resolution, workspace checks and resource/runtime capture before child
admission. It is separate from foreground joining and child execution; those
budgets have not started yet. Expiry requests cancellation and fully joins
started preparation before returning `startup_timed_out: true`, the applied
budget and observed `elapsed_ms`, without a child run ID. A late preparation
failure remains in the diagnostic; a blocking filesystem or application call can
outlive the budget. Stop before publication refuses the cold task, and delayed
executor polling cannot make preparation completed after its deadline eligible.
Successful starts report their applied startup budget. No automatic retry or
rollback is implied.

`start` also accepts `execution_timeout_ms`, defaulting to 1800000 (30 minutes), with
1–7200000 milliseconds allowed. This bounds child model/tool execution from task
submission after preflight, not how long a parent waits. For example,
`{"action":"start","task":"Inspect the parser","mode":"background","execution_timeout_ms":120000}`
starts a child with a two-minute execution budget. Its receipt includes the applied
budget. Expiry requests stop and fully joins before reporting
`outcome.execution_timed_out: true`; the child's ordinary Turn end remains visible
alongside it. Explicit cancellation is not relabelled as a timeout. Cleanup can
outlive the budget: this is neither a hard cleanup deadline nor rollback, proof of
remote quiescence or permission to repeat uncertain effects. A timed-out child has
no successful answer, even if an independent cleanup failure also occurs.

Joined outcomes include `elapsed_ms`, observed from linked task submission through
stop/settlement, excluding later cleanup. It is null when the callback did not
observe runtime; wait expiry does not set it or imply timeout. Timeout diagnostics
name the observed runtime and recorded execution budget.

Failures can retain `outcome.partial_output` with `text`, `truncated` and
`observation_gap`. This is the latest visible assistant response, possibly
uncommitted, not a final answer or proof of completed work. Its UTF-8 prefix is
limited to 50 KiB; no thinking, tool arguments/results or images are included.
Discarded retry text is not combined with a new response, and missing progress
is flagged instead of joining an orphan suffix. The same failed report remains
available in status, wait, acknowledgement and committed relationship inspection.
Successful runs have `partial_output: null` and their independently validated
answer. Startup failures occur before these child-run reports exist.

Owned `status`, `list` and `wait` reports include ephemeral `progress` once
the actual child starts: `child_session`/`request`, live `elapsed_ms`, observed UTC
`last_activity_at_ms`, `activity` (`model`, `thinking`, `text`, `tool` or null),
`current_tool` (name only, at most 256 UTF-8 bytes), `stopping`, and
`provisional_output` under the same 50 KiB/gap/truncation rules. Thinking activity
does not expose thought text; tool activity does not expose arguments/results.
Status reads and quiet waits do not change last activity or refresh execution
deadlines. Silence alone is not evidence of a stall. Elapsed freezes at observed
stop/settlement even when cleanup remains unjoined. Only `joined` attests full
joining; preview text is not a successful answer or a committed fact.

SDK callers can retain `PreparedDelegation.progress` without retaining a child
writer or task. The standard background helpers attach it to the actual owner;
manual destination assembly uses `with_progress` alongside `with_steering`.
RPC `get_background` exposes the same view and successful
`acknowledge_background` returns its final progress with the receipt/steering.
Live progress is not persisted or recoverable after process loss.

Use `{"action":"wait","ids":["<run-id>","<another-id>"],"timeout_ms":300000}`
to wait until **any** selected run joins. `ids` requires 1–64 unique owned
run IDs. The optional shared wait budget defaults to five minutes and accepts 1–600000
milliseconds. The result is `{wait_expired, runs}`: every selected run's
receipt/status in requested order, including still-running jobs. A joined failure
also wakes the wait and marks its output as an error; expiry alone is not an error.
An unknown or acknowledged ID rejects the whole selection, even if another is ready.

Expiry or cancellation stops only this wait; it neither cancels children nor sets
an execution deadline. Reports remain readable until explicit acknowledgement.
Wait again only on still-running IDs; including an already-joined job returns immediately. Other
waiters keep their independent subscriptions and may read the same joined report.

A joined answer is the child's actual original final visible text, retained after
full Host/Core joining of the requested completed Turn. It is not reconstructed
from storage, and execution, cleanup or storage failure clears it. Treat it as
the child's claim, not verification of its work. Raw child conversation,
thinking and intermediate tool output do not automatically enter parent context.

### Completion delivery

Choose how this receiving Session handles joined child readiness:

| Policy | Extra model work |
| --- | --- |
| `inbox` (default) | None on its own. Ready handles join the next natural model request. |
| `continue` | May extend the initiating active Turn at a complete boundary; never starts an idle Turn. |
| `wake` | Also permits a new automated Turn after the parent has settled, without a keypress or client prompt. |

For example, `ion --completion-delivery wake chat` enables autonomous handling for
new managed jobs. The flag also works with `run` and `rpc`, not metadata commands.
In chat, `/settings completion inbox|continue|wake` or the completion row in
`/settings` changes the live receiving policy. RPC uses `set_completion_delivery`.
Continue and Wake can incur additional model charges; neither is the default.
Policy changes revoke old grants rather than upgrading already-started jobs.

Readiness contains only source Session, run and child Session IDs. It does not
insert child output, fabricate a human instruction, select the child or acknowledge
its report. The model may retrieve the retained report with `status`/`wait`.
`/tasks` and `get_background` expose `completion`: `waiting`, `queued`,
`offered` (with the receiving Turn), or `revoked` (with a reason). Offered means
saved into context, not proof of provider receipt or model read. Reading/waiting
never releases a report; explicit acknowledgement does, and also withdraws a
still-queued notice. Already committed notices remain history.

Stop permanently revokes old grants, including after a fresh Start. Successful
Session/context/model/reasoning/resource changes also revoke them. The transferred
child can still finish and retain its report under the application owner; a late
result cannot silently restart the stopped or changed recipient. Ready wakes wait
behind admitted human input and unread reliable results. Intake bounds 64
reservations/queued notices across producers; a full inbox refuses new delegated
execution rather than silently dropping readiness.

These are live grants, not durable delivery or restart scheduling. A stopped,
closed or lost receiver does not promise later automatic handling. A saved UUID
cannot reauthorize it. Headless retains ownership through authorized follow-on
work and emits one final result; interrupt/EOF/quit still stop and join owned work.
The [Rust SDK](#rust-embedding) also supports explicitly authorized peer
targets; CLI/RPC currently use the parent, without a cross-process broker.

Managed files live under the workspace catalog's `children/`, outside automatic
`--continue` root selection; use the returned path to inspect or explicitly open
one. The same workspace is not conflict isolation, and native tools retain host
permissions. Parent stop and foreground client EOF stop/join their owned children;
background children stop on their execution deadline, explicit cancel or
application shutdown. Cancellation
does not roll back effects. Normal headless completion lets delegated agents
finish, then stops and joins managed programs. Interruption, RPC EOF and terminal
quit stop and join both. These are
offline lifecycle guarantees; live-model adherence, service scale and human
daily-driver acceptance remain open.

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
frozen catalogue and returns
`{name, description, input_schema, result_schema, declaration}`, or explicit
`null` if missing.
`result_schema` is `null` when unknown, otherwise `{path, schema}`: a key path from
the actual `{value, is_error, images}` reply and a standalone JSON Schema for the
expected successful data there. Local schema references stay unchanged. Rust
registrations can use `ToolDefinition::with_result_schema(schema)` for the whole
`value` (`path: ['value']`), or `ToolResultSchema.value_path` for a nested value.
An advertised MCP output schema points to `['value', 'structured_content']`;
content, errors, conversion notices, capture paths and images are not unwrapped
or discarded. Hints do not validate or convert observations. Check `is_error` and
truncation/delivery markers before interpreting data; discovery fetches no schema
references.

Both include callable deferred tools and exclude intrinsic `tool_search` and
`code_mode`. Discovery is read-only: it neither activates tools nor records child
effects, and shares existing bridge, JSON, reply, cancellation and guest budgets.

`declaration` is an advisory TypeScript call signature for the exact tool name and
its input schema, with the actual `{value, is_error, images}` reply envelope.
`value` stays `unknown`: an expected result schema does not validate an observation.
The signature quotes names without creating aliases. Types omit constraints that
TypeScript cannot express; the original `input_schema` remains the argument
contract. Unversioned and explicit JSON Schema 2020-12 inputs can supply types
through local JSON Pointer references. Other explicit dialects, external references
and nested schema-resource scopes stay `unknown`. Excessive type size, depth or
expansion produces a complete `unknown` argument type with a limitation notice,
not a partial declaration. The new field shares normal reply budgets.

Inspect a declaration without dispatching a tool:

```js
const definition = await describeTool('read');
if (!definition) throw new Error('read is unavailable');
text(definition.declaration);
```

The VM still accepts JavaScript function bodies, not TypeScript declarations.
Value-conversion helpers and persistent script state remain separate capabilities.

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

## Current limits

Restricted Rust applications can use shared control, but the durable/service
contracts are not all implemented:

| Boundary | Implemented today | Not yet implemented |
| --- | --- | --- |
| Application setup | Caller-owned model/tool/instruction configuration, workspace-free Sessions and explicit shell/local-image authority. Opt-in continuation checks a saved authority reference and declarations. | Changed-authority reconciliation. Applications must reconstruct and attest live bindings on reopen; the reference is not a credential or sandbox. |
| Durability | SQLite conversation/effect history, conservative recovery, restart-safe direct admission through the Rust SDK and keyed RPC literal text; explicit same-Turn continuation through the Rust SDK. | Durable pending queues, wire-protocol delivery IDs and automatic restart/continuation. Coding CLI/TUI/RPC submissions do not opt in to continuation. |
| Delegation | Main coding preset exposes fresh/fork foreground and background Sessions with inherited route/reasoning, application-owned retained results, wait/status/cancel/acknowledge, joined original answers and receiver-owned Inbox/Continue/Wake delivery with optional SDK peer targets. SDK children use the same stable identity, complete environments and conservative recovery; Code Mode nested tools remain distinct. | Live-model/service-scale delegation qualification and native-app visual acceptance remain open. Model arguments never grant execution authority. |
| Native programs | Scoped pipe/PTY programs, stdin interaction, bounded raw capture and screen pages, terminal resize, retained wait/close/release and shared application cleanup; live human attachment lease with a Ctrl-] terminal view; child-job scope reclaimed before answer release. | Concurrent model/human input, shell environment capture, graphical terminal protocols and restart attachment. |
| Controls and trust | Model-loadout include/exclude selection, read-only model tools, MCP direct/deferred/hidden exposure, digest-bound project MCP/command trust, explicit named user commands and host-permission execution. | Per-call approval/consent, OS sandboxing, arbitrary extension handlers/hooks/UI contributions, MCP OAuth and per-tool exposure patterns. Loadout selection and trust are not security boundaries. |
| Remote use | Foreground stdio RPC, lossy progress and complete pull snapshots through shared control. A service embedding retains/drives the owner independently of connections. | No built-in authenticated network server, reliable activity replay or durable queue/metadata recovery. RPC EOF still stops/joins work. |

Common coding workflows are implemented, but Pi-level daily-driver reliability
and terminal usability are not yet qualified. Selected live coding tasks have
succeeded; longer workflows have also produced false summaries, inaccurate final
reports and fresh duplicate mutations. Mechanical compaction and absence of
harness replay do not establish truthful recall or once-only model behavior.
Check committed outcomes, workspace changes and native tests, not a recap alone.

CI exercises built headless, embedded, RPC and PTY workflows, including storage
faults, cancellation, recovery, resources and MCP. These checks do not establish
sustained repository work, representative native-emulator stability, clipboard
backend coverage or human visual usability. An earlier isolated Ghostty
qualification failure (exit 101, no useful trace) remains unresolved; PTY success
does not close it. Harness latency and memory bounds
need representative measurements; no comparative performance gain is claimed.

Direct Anthropic and OpenAI still need live account qualification; thinking on
custom llama.cpp routes is unsupported. Historical route success does not imply
current availability. Ion has no native ChatGPT subscription integration. Context
pressure uses approximate estimates, refined by compatible observed input usage
when available. Unknown-window routes rely on the encoded request bound. New image
sets still use a conservative visual-token allowance; calibration cannot prevent
an initial image request from exceeding that estimate.

Chat-compatible tool messages encode
`{"is_error": bool, "result": original_payload}`; inspection keeps raw payloads
and status separate. Saved OpenRouter structured reasoning from older tool-message
or withheld-output encodings is incompatible: compact that Session or start a new
one. Ion never silently rewrites a potentially signed prefix or discards reasoning.
See [ARCHITECTURE.md](ARCHITECTURE.md) for contracts and [AGENTS.md](AGENTS.md)
for repository checks.

## Rust embedding

The runnable [embedded example](crates/ion-app/examples/embedded.rs) submits one
prompt through shared Host session control:

```sh
cargo run --locked -p ion --example embedded -- --help
cargo run --locked -p ion --example embedded -- \
  --cwd . --session /tmp/ion-embedded.sqlite \
  --provider xiaomi --model mimo-v2.6-flash 'Explain this repository'
```

Choose an available route with matching credentials in the normal Ion config
or environment; provider usage may be billed. Reuse the same Session path and
working directory to continue. Ctrl-C requests stop and joins the operation;
all post-Session errors pass through shutdown before the runtime exits. The
example prints only the committed answer using fallible output, restores the
saved reasoning preference, and connects no MCP servers. It is an SDK lifecycle
sample, not a replacement for the terminal or RPC client.

`CodingApplication::new(host, tools).setup(session, selected, reasoning, questions, resources)`
supplies the coding preset. Prepare `resources` once from `Application::resources`
for the selected cwd (or `host.resources(cwd)` for the default coding recipe).
The same snapshot serves initial-input preflight and binding; CLI preflight runs
before Session creation and MCP startup. The recipe supplies native tools; the
optional `tools` source adds or overrides registrations, not a complete allowlist.
`with_tool_selection(ToolSelection::include(...).with_exclude(...))` narrows the
model-visible loadout; `with_read_only_tools()` selects the read-only preset.
`ToolSelection::include([])` selects no tools; `ToolSelection::all()` leaves
inclusion unrestricted. Selection filters model declarations, discovery and the
Code Mode namespace, and
managed children inherit the parent selection. They are application loadout
policies, not OS sandboxes or authority grants.
Consume the setup with `into_control::<M>()` to create `SessionControl<M>`, the same
owner used by terminal, headless, RPC and this example—not another model/tool loop.
It handles preparation, steering, follow-ups, settlement and idle replacement.

For restricted/non-coding embedding, implement `Application`: `resources(cwd)`
returns explicit resources, and `runtime(cwd, model)` returns the complete agent
configuration. `Resources::new(instructions)` does no filesystem discovery;
`ApplicationRuntime::new(agent)` grants neither direct shell nor local-image-path
access. Encoded/normalized images still require the model's image capability.
Native input is an explicit grant, not a sandbox or path allowlist.

Construct `SessionSetup::new(session, catalog, application, model, reasoning, questions)`.
Pass `None` for no question interaction, or an owned `QuestionCustody::new()` in
`Some` for a real consumer. `Application::runtime` receives that optional port as
an `Arc`; custom recipes explicitly register `QuestionTools` if they offer it.
`SessionControl::attach_question_responder()` returns the non-clone consumer
lease. Pull its pending request, then call `answer` or `cancel` with the exact
request ID. Host invalidations wake shared control; complete authorized snapshots
also expose the current pending request, not answer authority. Idle model/Session
replacement retains the consumer while invalidating old request IDs.
Omitted model/reasoning restore the saved preferences; a fresh Session uses the
application's model default. The recipe remains attached through reload and
replacement. Clone retains the resolved runtime rather than resolving a route
name again. Explicit model selection and reopening resolve through the recipe;
unavailable requested identities fail rather than silently selecting a default.
Create a workspace-free Session with `CodingSession::create(path, None)` and its
catalog with `SessionCatalog::new(state_root, None)`—no dummy directory is needed.
The coding executable rejects workspace-free Sessions for coding operations;
inspection can still represent their `cwd` as null.

The credential-free [restricted example](crates/ion-app/examples/restricted.rs)
uses a synthetic model and one custom counting tool through this same API:

```sh
cargo run --locked -p ion --example restricted -- "$(mktemp -d)"
```

It checks actual requests, one custom-tool effect and delivery reconciliation
after reopen. After building the example, `python3 scripts/smoke_recovery.py` also
kills an owned instance after its effect and explicitly continues the original
Turn with the missing observation still unknown. These are synthetic SDK checks,
not live-provider or service qualification. Applications must authenticate callers
and authorize control; selecting tools does not isolate tenants or host effects.

The credential-free [delegation example](crates/ion-app/examples/delegation.rs)
uses separate restricted parent and child applications through shared control:

```sh
cargo run --locked -p ion --example delegation -- "$(mktemp -d)"
cargo run --locked -p ion --example delegation -- "$(mktemp -d)" --fork
cargo run --locked -p ion --example delegation -- "$(mktemp -d)" --background --fork
```

After building it, `python3 scripts/smoke_delegation.py` also kills an owned
process after a synced child host effect, for fresh/forked foreground children
and background children whose parent has already joined. Explicit parent-Turn
continuation (foreground) and fresh-work recovery preserve the original child
identity and unknown effects without replay or a replacement child. Focused faults
cover transfer/relationship commits, both cancellation scopes, unread custody,
concurrent joins, reader-independent failure/panic and Code Mode error catching.
These qualify bounded offline SDK ownership/recovery, not live-model delegation,
service scale or remote-effect quiescence.

Use `submit(Delivery, UserInput, metadata)` for input. `Prompt` expands only its
`text` once, normalizes images and composes literal `TextFile` snapshots from
`files`; `prompt_files::prepare_files` performs explicit bounded file loading.
An already-prepared `Message` stays literal. `submit_batch(Vec<(UserInput, M)>)`
admits one Start and ordered FollowUps together or returns all originals and metadata
in `RejectedBatch`. It reserves the existing shared capacity/byte budget before
starting work; later preparation and execution retain ordinary Stop/failure behavior.
For literal unit-metadata launch batches, `preflight_messages` checks count, message
validity and conservative retained-byte accounting before application setup.
Refusal returns the original input and metadata. The returned `InputId` means
Host custody, not durable acceptance. Client metadata requires `Serialize + Send`
but not `Clone`; it never enters model content or Session records.

For durable direct delivery, use `submit_request(&RequestKey, Message, metadata)`.
Create the key with `RequestKey::new(scope, id)`; both opaque, case-sensitive
components are nonblank, at most 256 bytes and contain no control characters.
The application authorizes the namespace—keys are not authentication tokens.
`RequestAdmission::Started { id, receipt }` is returned only after the key and
literal input commit together with Turn admission. Normal `Accepted`/`Settled`
updates still carry this attempt's metadata. Redelivery of the same key and exact
message returns `Existing { receipt, metadata }` without a task, model request or
tool dispatch, including while busy, after shutdown or after reopen. Different
input is an error; changing current configuration does not retarget accepted work.

`session.request_receipt(&key)` reads acceptance without reconstructing an
application recipe. A receipt names the logical Session and Turn, with its latest
committed end reason. `end: None` does not prove work is running; lost observations
remain unknown. Reconciliation is passive. New explicit work closes interrupted
history conservatively; it does not rerun the old request. Prepare templates and
local files before constructing this literal message. Pending queues, client
metadata and live services are not made durable. Ordinary CLI/TUI submits and RPC
wire command IDs remain unkeyed; RPC `submit_request` uses a separate explicit key
for literal text. Request deduplication is not exactly-once tool execution.

For explicit interrupted-Turn continuation, configure the application's agent with
`with_recovery_scope(AuthorityScope::new(versioned_reference)?)` **before admission**.
The reference identifies the actual application/principal, environment, model
route and grants. It is a trusted application assertion—not proof that equal
names, schemas or hashes imply equal credentials or executors. Do not put secrets
in it. On reopen, resolve and authorize that reference and supply the corresponding
`Application` recipe; Ion does not reconstruct live credentials or executors.

`session.recovery_state()` passively returns the saved authority and a
`RecoveryPoint { session, turn, checkpoint }` for an opted-in unfinished Turn.
It does not prove liveness or that old effects stopped. Explicitly choose whether
to proceed despite uncertainty, then call `control.resume_turn(point, metadata)`.
Core checks the exact checkpoint, scope, model preference and declaration binding
before committing continuation and unknown settlements atomically. The same loop
starts its next model step; it never reruns old calls or reconstructs a guest.
Changed instructions, controls, callable definitions or scope are rejected. The
opt-in mode pins executable routes for the whole Turn, while deferred discovery
can expand model exposure. Prepare dynamic sources before admission; refreshes
and changed authority require new work, not a silent rebind.

`RecoveryAdmission` returns a local operation ID and its committed checkpoint.
No new input or Turn is accepted; settlement returns this attempt's metadata in
`Settlement.metadata`, separately from accepted steering. Reusing an old point is
stale, not another dispatch. After another process loss, a fresh continuation
requires an explicit decision against the new checkpoint. Committed-ended Turns
cannot be resumed, and context copies inherit no continuation grant. Redelivering
a request key still only reconciles—it never resumes work. Reconcile committed
history on `TurnResumed`; it is not a fresh-input event. A later model-issued call
can still duplicate an earlier uncertain effect: this is not exactly-once execution.

Drive `next()` while handling client I/O. Reliable updates distinguish local
`Dispatched` correlation, durably `Accepted` input with a Turn ID, `Returned`
originals and `Settled` outcomes/metadata. `state()` derives activity and queue
counts from this owner. Borrowing/cancelling a `next()` wait does not stop work.
Keep polling to advance preparation and queued follow-ups; do not implement
another preparation queue or infer acceptance from rendered progress. `next()`
delivers an operation's remaining buffered activity before its `Settled` update,
including failed outcomes. Real overflow still produces a gap; maintenance and
shutdown may discard provisional activity.

`request_stop()` is not completion. Await `shutdown()` before destroying the
runtime, including on client errors: it closes admission, stops and joins
preparation/Core work and returns remaining updates and cleanup errors. Cancelling
that wait leaves it closed and joinable; Drop can request stop but cannot join.
Drain returned inputs and settlements before replacing a Session or model.

A coding setup also retains an `ApplicationOwner`, available through
`control.owner()`. Closing one Session does not close this longer-lived owner:
request its shutdown, join control, then join the application before releasing
MCP or other captured services and before potentially blocking final output.
It closes both delegated-agent and native-process admission paths before joining
either family. `Application::owner()` must return the same retained owner, not a
new owner on each lookup. Restricted recipes default to no application owner;
`ApplicationRuntime::new` contains no process supervisor. A recipe installing
`ProcessTools` must explicitly retain its supervisor in the runtime's `processes`
field for delegated-job cleanup. Session history, cloned tool declarations and
saved IDs cannot reconstruct these live resources.

`observe()` is passive. Lag produces an explicit gap: discard the partial stream
and reconcile committed Session facts, rather than joining a surviving suffix to
old deltas. Reader detachment does not cancel execution; a service can retain
control while clients detach. This is a progress stream, not a complete
snapshot API by itself. Foreground clients explicitly stop/join on I/O loss.
MCP connection cleanup remains a separate `McpTools::shutdown()` obligation;
await it to completion. It retires all peers before joining services and owned
root processes, so unanswered calls do not require caller cancellation to close.
Unanswered effects remain unknown, never replayed. Already observed results remain
owned by their calling operations, including queued conversion work; connection
shutdown does not replace the operation stop/join obligation.

For a service, retain `SessionControl` outside connection scope and drive
`maintain()` while busy. It advances the same preparation, joining and follow-ups
as `next()`, but keeps reliable updates and their capacity reservations in Host
custody. Passive readers cannot acknowledge or evict results. `take_update()` (or
`next()`) explicitly transfers originals/metadata and settlements to the controlling
application and releases capacity; it is not a transport acknowledgement. If that
application promises later recovery, it must retain or persist the transfer.
Full custody refuses new admission, not stop/join. There is no receipt ring that
silently expires returned originals, nor unlimited/cross-restart metadata retention.

After authorizing inspection, call `attach(project_metadata)` for a complete
`ControlSnapshot` and a passive observer, registered before the read. `snapshot()`
refreshes the view without consuming results. It includes committed Session facts,
retained operation/preparation/queue state, pending withdrawal/acknowledgement,
reliable returns/settlements and cleanup failures. Projection borrows metadata;
`M` needs neither Clone, Sync nor Deserialize. These privileged views include raw
images, opaque replay and private shell context: **do not serialize them blindly
to a network**. Authorize the full Session/queue scope and select disclosure.

`SnapshotCursor` identifies the live owner and Session, orders replacement reads
and records their committed fact count. A new owner has a new epoch, even after
reopening the same Session. Custody is sampled before committed facts: all Host
acknowledgements are covered, but facts may be ahead of callback/join processing.
An in-flight input is not proof of absent admission; a retained task is not proof
it is physically running. This is a causal view, not an atomic Core/Host transaction.
Replace the entire snapshot; its cursor does not order activity deltas. Lag,
reconnect or an owner/Session change requires a fresh view, never suffix-splicing.
Bound concurrent full-view requests: history/materialization has no aggregate
memory quota. Keep transport writes out of the owner driver.

`request_shutdown()` closes admission and requests stop synchronously.
`join_shutdown()` finishes cleanup but retains results/failures for inspection;
`state().shutdown_complete` marks completion of that protocol, not error-free
cleanup or remote quiescence. Cancelling its wait retains custody. `shutdown()`
also transfers the remaining updates/errors to its caller.

The credential-free [service example](crates/ion-app/examples/service.rs) exercises
late join, permission denial, slow/dropped readers, gaps, reconnect, explicit result
retention, stop and joined shutdown through bounded in-process transport requests:

```sh
cargo run --locked -p ion --example service -- "$(mktemp -d)"
```

It is synthetic SDK qualification, not a network authentication implementation,
a mandatory daemon or evidence of service scale or live-model adherence.

For lower-level integration, `ion-core::CodingAgent` and `CodingSession` expose
retained `Operation` handles and abstract model/tool contracts;
`Host::agent(selected, tool_set)` combines a selected HTTP route with a complete
caller-owned `ToolSet`, without adding coding tools or Code Mode. Custom `ModelService`
and tool sources can be assembled inside an `Application` and use shared control. Starting a Core operation reserves execution
immediately and rejects concurrent starts. Retain/join each handle:
dropping an owning wait requests stop, while timing out a borrowed `&mut operation`
only stops waiting. `CodingSession::shutdown()` joins retained work and reports
unread failures. A real operation-task panic blocks live owner reuse and leaves
effects unknown. Custom `ModelService` implementations must provide pure
`validate_controls` to support explicit reasoning; the default refuses it.

## License

[MIT](LICENSE)
