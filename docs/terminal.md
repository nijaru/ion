# Terminal use

Implemented interactive behavior. The terminal uses the same Session and shared control as headless and embedded clients.

[Start](../README.md) · [Terminal](terminal.md) · [Configuration](configuration.md) · [Tools](tools.md) · [Automation](automation.md) · [Rust embedding](embedding.md)

## Sessions and navigation

```sh
ion --continue                    # reopen the latest Session here
ion --continue turns              # list Turn numbers and prompt previews
ion --continue clone              # independent conversation copy
ion --continue fork 2             # new Session before Turn 2
ion --continue fork 2 --after      # new Session after settled Turn 2
ion --continue compact            # summarize context, retaining raw facts
```

`--cwd PATH` chooses a working directory. By default, a run starts a new
session; `--continue` reopens the most recently active one in that directory.
`--session ID` selects a listed session, and `--session PATH` can create or
open an exact SQLite path for scripts. Session paths are canonicalized so
symbolic links share the same writer lock. Hard-linked databases are rejected;
use clone or fork to copy a conversation. Opening and quitting an empty chat does
not displace the latest conversation in `--continue` or `sessions`. An
existing session uses its recorded directory, and an explicit `--cwd` must
match it. Headless runs print the
session ID to stderr. In the TUI, `/new`, `/clone`, `/fork`, `/fork-after`, `/resume`, `/name`, `/session`,
`/model` and `/compact` manage the conversation; `/login PROVIDER` and `/logout PROVIDER`
manage saved keys. `/tasks` lists application-owned background Sessions; use
`/tasks result ID`, `/tasks cancel ID` and `/tasks acknowledge ID` to inspect,
stop or release joined custody without deleting committed facts. The TUI model picker searches catalog and configured
custom routes. A resumed session restores its model; `ion use` sets the
default for new sessions. Custom routes and that default are published together
in `$XDG_CONFIG_HOME/ion/models.json` (or `~/.config/ion/models.json`), with
concurrent saves serialized. After updating from a development build with the
older split preferences, select your default and configure custom routes again.

`/resume` opens a searchable saved-Session browser and marks the current Session.
The selected conversation shows its full ID, Turn count, recorded model and file
activity timestamp/age. Activity is a discovery-time snapshot, not execution
status. Discovery runs outside the input loop; Esc dismisses the browser while
Ion retains and joins its scan before starting another scan. An in-flight
filesystem/metadata call cannot be interrupted. Dismissed results cannot overwrite
a newer picker. Browsing does not submit input or repair history;
confirming a selection switches through idle shared control. This is a flat
catalog, not inferred fork ancestry or Pi's in-file branch tree.

## Transcript and presentation

Terminal chat is inline-first. Settled transcript rows are appended once to
native terminal scrollback; Ion keeps only the active composer/progress region
mutable. Resuming or switching to a saved Session bootstraps at most the latest
six turns into scrollback and labels omitted earlier history as retained; the
complete Session remains available through inspect/export and tool detail.
Publication uses available terminal rows before scrolling. Ordinary redraw and
resize do not publish live rows; terminal-native reflow remains emulator-owned.
Command responses (`/help`, `/skills`, `/tools`, `/tasks result`, etc.) and
client diagnostics also publish their complete wrapped content, not a clipped
composer preview. They remain in a terminal-local display log interleaved with
Session history for fullscreen scrolling. This log lasts until Session replacement
or client exit; it is not durable Session history or model input. Explicit repeated
commands produce fresh responses; identical consecutive diagnostics at the same
history boundary coalesce. Unfinished recalled or resynchronized facts stay in a
mutable preview rather than being irreversibly published.
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
Tables, syntax highlighting and other Markdown extensions are not implemented.

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

### Viewports and terminal status

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
mode. Working-directory, Session, model and context metadata are no longer
permanent footer rows; `/session` exposes Session/context detail on demand.
Cloning copies committed conversation and context into a new session with
independent future turns. Both sessions still use the same live working
directory; cloning does not copy or restore files.
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
### Saved identity and development format

Session IDs are immutable UUIDs stored in the header, independent of filenames
and provider affinity. Renaming a closed database does not change its ID; clone
and fork create new identities without inheriting external delivery receipts or continuation grants.
This unreleased branch uses Session format 16; earlier development Session
files are not reopened. Tool activity classification used by the transcript is
stored with each assistant tool-call batch, so resumed history is not
reinterpreted through the currently installed tool catalog.

## Input, steering and recovery

While a turn runs, the editor remains available: Enter steers the next model
step, Alt-Enter queues a separate follow-up turn, Alt-Up returns the most
recent queued follow-up to the editor, and Ctrl-C cancels. After stopped work settles, unaccepted input is returned rather than starting
model work. An occupied editor stays separate: Alt-Up cycles retained drafts
without submitting them. Enter during shell/compaction queues a follow-up. An ordinary nonzero shell exit does not withdraw an
intentional follow-up. The terminal uses the same shared queued-input allowance
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
Home/End or Ctrl-A/E move within the current logical line; Ctrl-B/F move by
grapheme. Ctrl/Alt-Left/Right or Alt-B/F move by Unicode word boundaries.
Ctrl-U/K delete to the line start/end, joining adjacent lines when already at
the boundary. Ctrl-W or Alt-Backspace deletes the preceding word; Alt-D/Delete
deletes the next word. Ctrl-D deletes forward in a nonempty draft and exits when
the draft is empty.

### Undo, redo and kill/yank

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
even if a running Turn refreshed history. Type
`@` to pick a project file, or use Tab after a partial `@path`; the picker
inserts a path reference for the model to read, not the file's contents.
Discovery runs off the client event loop, so you can filter or dismiss the picker
while it scans. Limits and traversal errors are reported rather than hidden.
Closing the picker cancels further traversal and prevents stale results from
reopening it; exit still waits for any filesystem call already underway.
Restoring or replacing a draft also closes its old completion picker. Returned
input keeps its cursor and attachments; a displaced unsent draft remains
recoverable with Alt-Up.
### Command discovery and inspection

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
### External editor, copy and export

Ctrl-G edits the current draft in `$VISUAL`, then `$EDITOR`, falling back to
`vi`; Ion keeps the original draft if the editor fails. Outside detail views,
Ctrl-X or `/copy` copies the last committed assistant answer to the system
clipboard when one is available. On remote or displayless terminals, Ion sends an OSC 52
clipboard request, whose support depends on the terminal. `/export PATH`
saves a readable transcript to a new file; `ion --continue export` prints it
to stdout, and `ion --continue export PATH` saves it. Export includes prompts,
shell commands and tool output, so review it before sharing. Image inputs are
shown as markers rather than inline bytes; an existing target file is not
overwritten.
### Direct shell commands

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

## Images and clipboard input

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

See [typed questions](tools.md#typed-questions) for dialogs and [managed programs](tools.md#managed-programs) for `/attach`. Model effort, resources, executable commands and MCP setup are in [configuration](configuration.md).
