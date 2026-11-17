# Terminal chat

Run `ion` in a terminal, or explicitly use `ion chat`, after [configuring a model](../README.md#set-up-a-model). The default surface is inline native scrollback; `ion --tui-mode fullscreen` selects fullscreen conversation view. Both are experimental, not qualified for daily use.

`/help` lists commands, `/help COMMAND` shows usage and `/help keys` shows effective keybindings. The keys below are defaults.

## Input and orientation

Use `/file PATH` to attach a UTF-8 text file or image to the next input. An unquoted path can include spaces; a JSON-quoted path is also accepted, matching completion. Ion does not evaluate shell expressions or attach files merely mentioned in prose. See [saved input and attachments](sessions.md#saved-input-and-attachments) for path expansion and CLI input syntax.

Chat shows the bound model and selected reasoning preference. This is not the effort of an already-running request; `default` leaves the choice to the provider. Use `/reasoning` to change the preference. The cached `context ~input/window` display is a planning estimate, not recorded token usage or an exact tokenizer count; see [context and compaction](sessions.md#context-and-compaction).

## Cancellation and local controls

Ctrl-C immediately requests cancellation of foreground work, including unanswered questions, without confirmation. It does not promise immediate termination: Ion still joins started work and cleanup, and effects are not undone. Transferred background work has a [separate lifetime](delegation.md#lifetime-and-recovery). While [attached to a program](processes.md#attach-and-detach), Ctrl-C is forwarded to that program instead. External editors handle their own Ctrl-C.

A pending clipboard read in the composer is cancelled first. Esc dismisses local UI. Stopping work via Esc, clearing a draft, cancelling a question with Esc and quitting with a shortcut require confirmation.

Reported key repeats cannot submit input, accept completions or choices, toggle tool output or question/picker controls, open an external editor, or start another cancellation. Ordinary held-key editing and navigation still repeat. Legacy terminals cannot distinguish auto-repeat from another press.

## Local recovery

When input acceptance or a Session/file command outcome is unknown, Ion keeps the original source separately rather than restoring it for retry. `/recovery` lists these process-local records; `/recovery ID` shows the original Session UUID/path, cause, current receipt status, literal editor text, caret byte offset and attachment metadata. Inspection does not dump image base64.

Use `/recovery save ID PATH` to save a JSON inspection backup of the original sources and metadata. Owned `ImageBytes`, prepared `Image` and `Text` snapshots are included. `File` and `ImageFile` sources are literal path references only: saving does not read or preserve their file contents. A changed or deleted file cannot be reproduced from its path reference. The backup is private (mode 0600), must not already exist, and may contain sensitive input. Saving runs as owned I/O; failure leaves the original intact. A refusal before the backup file is opened returns the command text without claiming uncertain file effects; a post-open write/sync failure still requires inspection before retrying. The backup is not executable input, an import format or permission to replay. Paths in `/recovery save`, `/file` and `/export` are whole literal tails or JSON-quoted strings, resolved against the captured working directory with `~`/`~/` home expansion; they are not shell expressions. Tab completion replaces only the path tail.

`/recovery discard ID` irreversibly releases the local source, not its effects. For unresolved input, a small receipt/Session record remains and blocks new input in that Session until the receipt positively resolves. Unknown command sources can be removed, but that does not establish rollback or make retry safe. Other Sessions are not frozen.

Only the original input receipt can resolve quarantined input: a matching committed acknowledgement releases the source without replay and refreshes canonical history; confirmed nonacceptance returns the exact draft and attachments without submitting. If another Session is current, the nonaccepted source waits until its original Session is selected. If a picker, details view, question or pending clipboard read owns input, the returned draft waits in the recovered-draft queue instead of replacing the composer. Discarded sources are never resurrected. Pending, committing and unresolved receipts remain fenced; refreshed history, missing journal entries, saved backups and discard are not acceptance evidence.

Unsaved recovery sources and receipt fences are lost when the process exits. Save anything needed before quitting; Ion does not persist or automatically retry this quarantine. Shutdown still joins owned work, including a started recovery save.

## Inspect tool output

Ctrl-O expands or collapses tool output without leaving the composer. Inline chat temporarily uses the shared fullscreen conversation to inspect expanded output, then returns to native scrollback when collapsed; published history is not rewritten.

Mutation rows show recorded line additions/removals for that operation, not repository totals. Counts cover the snapshot diff even when patch capture is truncated; an uncaptured comparison is reported separately from write success. Expansion shows recorded directory entries, file-search results and remote-tool text or structured values. It cannot recover data omitted during capture.

Command rows distinguish nonzero exits from signal termination. Recorded wait failures show the unavailable status and retained cause. Use `/tools` to list recorded operations, `/tool N` for raw tool evidence or `/tool all` for the raw conversation inspector. For captures beyond the displayed result, see [retained evidence](sessions.md#retained-evidence-and-captures).

## External editors and terminal state

`/editor` opens the draft in an external editor. Editors share Ion's controlling-terminal input, handle their own Ctrl-C and return the original draft when cancelled. Ion preserves the caller's terminal input settings across editor failures and leaves an editor's unclosed alternate screen when the terminal reports it. On terminals without screen-state reporting (DECRQM 1049), editors must restore their own screen.

Chat temporarily enables grapheme clustering (mode 2027) when the terminal reports it as supported and currently off, then restores the prior setting when yielding the terminal. Without reported support, rendering stays best-effort; complex emoji alignment depends on the terminal and its Unicode width tables.

## CLI shutdown signals

In the coding clients, SIGTERM and SIGHUP request application-wide shutdown and join owned work before exiting with status 1. This also covers startup model/Session pickers, masked key entry and `ion login`. An external editor receives SIGTERM on its own PID; Ion waits for it before restoring the terminal, so an editor that ignores the request can delay exit. An admitted key/default save is finished, not undone.

Graceful exit can also wait for an unread final output pipe, including passive commands launched through `--resume`. Passive commands without the startup browser retain default OS signal handling. Stopping work is not rollback; see [Sessions and effects](sessions.md#files-and-effects-stay-current).
