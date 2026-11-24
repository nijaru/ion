# Sessions and alternative conversations

Ion saves conversations in SQLite. Reopening a Session restores history; it does not restart interrupted work or replay recorded tool calls.

## Find and reopen

When you leave a nonempty chat normally, Ion prints the command to reopen that exact Session, even from another directory.

From a terminal:

```sh
ion --continue       # latest Session in this directory
ion -r               # browse saved Sessions
ion sessions         # list saved Sessions
ion --session UUID   # reopen a particular Session
```

In chat, `/name Fix parser tests` names the current Session. `/resume` opens the saved-Session browser; search by name, opening or latest prompt preview, ID or directory. Quoted phrases and multiple search terms are supported. The browser marks the current Session and shows the full ID and recorded model in its details. Follow the displayed controls to filter named Sessions or cancel without switching.

The live `/resume` browser stays in the current working directory. To choose a Session from another directory, start with `ion -r` and use its all-folders control. `/session` shows the current Session and context details; `/new` starts a new conversation.

After an interruption, inspect unknown effects before asking to repeat a mutation. Send a new message to continue: it starts a distinct Turn under the current runtime, not a replay of the interrupted Turn. The previous Turn is recorded as interrupted, with missing results marked unknown rather than rerun.

Saved-Session CLI commands:

```sh
ion --continue turns       # recorded Turns
ion --continue usage       # recorded tokens, not account billing
ion --continue inspect     # committed history, no model or tool execution
ion --continue export      # text transcript on stdout
ion --continue compact     # model-backed summary; provider charges may apply
```

## Saved input and attachments

Attach UTF-8 text files or pictures with `ion run @notes.md @screenshot.png 'Explain this'`, or `/file PATH` in chat. Use `--image PATH` when the operand must be an image. These requests need a [configured model](../README.md#set-up-a-model) with image support when applicable.

CLI `@file` operands and `--image` attachments accompany the first prompt. Each quoted prompt operand is a separate Turn, not another word in one prompt. Use `--` before literal prompts that start with `@` or would be parsed as options or command names, for example `ion run -- '@literal text'`. Piped UTF-8 stdin is prepended to the first prompt; it is limited to 8 MiB.

CLI and chat attachment inputs resolve `~/…` from `HOME`. Use `./~/…` for a literal tilde directory. Chat accepts an unquoted path including spaces, or a JSON-quoted path; it does not evaluate shell expressions or read paths merely mentioned in prose. [RPC attachment paths](rpc.md#rpc-input-and-attachments) are literal and do not expand `~`.

Accepted messages save prepared text/image snapshots, so reopening does not reread source files. Invalid attachment preparation rejects the whole batch rather than submitting a partial prompt.

## Explore context without copying

Press Esc twice on an empty, idle composer or enter `/tree` to see recorded message boundaries, including alternative paths. Its initial **Current context · keep selection** choice closes the browser without changing context, rebinding the runtime or writing a navigation fact.

Search readable message text or metadata such as `entry 17`. `[path]` marks messages on the selected ancestry; `[alternative]` marks messages outside it. Selecting a message changes model context in the **same Session**:

- **edit before** selects the context before that input and restores the original input for editing. It is not submitted automatically.
- **continue after** selects the context through an assistant message.
- **after tool group** selects a completed call/result group, not a point in the middle of dispatch.

`/tree ENTRY` selects the indicated message boundary directly. Message boundaries need not equal the current journal head: later entries may record model choices, compaction or other metadata. Use the initial keep-selection choice when you want to leave the current context exactly as it is.

Navigation appends a selection; it does not delete the other paths. A stale selection is refused rather than silently discarding work recorded since the browser opened.

## Copy or fork

| Command | Result |
| --- | --- |
| `/clone` | Copy the current conversation into a new Session. |
| `/fork` | Choose a Turn, fork **before** it, and restore its input for editing. |
| `/fork-after` | Choose a recorded ended Turn and fork **after** it. |
| `/fork TURN` / `/fork-after TURN` | Select the boundary directly by Turn number. |

Fork choosers list recorded Turns oldest first and initially select the newest eligible Turn. `/fork-after` requires a recorded end, which may be completed, cancelled or failed; it does not imply success. Candidates include Turns on alternative paths, not just the currently displayed ancestry.

A fork or clone has a new Session ID. Copying and restoring input do not submit it to the model. Saved history and referenced observed captures are copied; these are not filesystem snapshots or running-program copies.

## Files and effects stay current

**Reopening, navigating, cloning and forking do not roll back workspace files or external effects.** A conversation can describe an older file state while the files on disk remain newer. Inspect the workspace before continuing from an earlier point.

Recorded outcomes are evidence, not permission to replay effects. Missing outcomes can remain unknown. Live programs belong to the application owner and their original Session scope; see [Long-running programs](processes.md). New messages use the active runtime's credentials and tool configuration, not historical credentials or executors.

## Context and compaction

Terminal chat shows a cached `context ~input/window` estimate when space allows; `?` means the model's window is unknown. It is hidden during active Turns and unsettled Session changes. `/session` shows the full estimate from Core's request planner, separately from recorded token usage.

The estimate uses the current model, instructions and loaded tools, excluding drafts and queued input. Compatible process-local provider observations calibrate it; compaction, reopening or a changed binding can require a cold byte-based estimate. Neither estimate is an exact tokenizer count or the budget of an in-flight request. Active Turns may retain earlier instructions and a frozen loadout; refresh or compaction can change the next request. Retained request bytes are a planning bound, not HTTP wire size.

Use `/compact [instructions]` in chat or `ion --continue compact 'summary focus'` from the CLI to request a model-backed summary. Compaction summarizes bounded excerpts and retains the raw log; it does not roll back effects or delete retained captures.

Automatic compaction for extra output room is best-effort: a provider failure or unusable summary produces a warning and leaves context intact, so a request that still fits can continue with a smaller output budget. Manual compaction and compaction required to fit a request remain strict. Cancellation and storage/accounting failures still stop work.

## Retained evidence and captures

Original result envelopes and retained captures remain in the Session's SQLite database after compaction. Native command overflow/raw bytes, omitted web snapshots (converted text, not raw HTML), and retained MCP responses/resources use typed artifact receipts. Capture completeness is separate from command or server success. Partial, unfinished and unclaimed captures are retained conservatively, not automatically deleted or replayed.

The coding `history` tool searches recorded evidence without rerunning effects. It can retrieve committed child outcomes after task removal or reopening; see [delegation recovery](delegation.md#lifetime-and-recovery). Search/read JSON omits thinking, provider replay, configuration, private `!!` shell records and typed image bytes.

### Read an artifact

To save a capture without running a model or reopening the Session for writing:

```sh
ion --session ID artifact 42 0 > command-output.log
```

In `ion --session ID inspect`, count `entries` from 1 to find the result that contains the capture. Count its `artifacts` from 0. For shell commands, use the settlement entry, not the admission entry. The export writes raw bytes, so redirect stdout rather than displaying it in a terminal. Ion warns on stderr if the capture is incomplete. Discard the output file if the command fails.

The model can call `history` with `{"action":"artifact","entry":N,"artifact":0}` to read a referenced capture by one-based source entry and zero-based artifact index. For composed child calls, Code Mode uses `inspectSaved({kind:'artifact',parent,child,artifact:0})`, including committed children of the current invocation.

Pages return readable UTF-8 text where possible, otherwise exact base64 bytes. Check `encoding`: `data_bytes` counts the page's raw bytes, while `total_bytes` is the full capture size. Consume the page's data even when `next_offset` is null (EOF); otherwise follow `next_offset`. Offsets and the maximum 16 KiB page limit count raw bytes. Text pages preserve character boundaries without replacing malformed bytes.

`history` reads enforce the issuing request's frozen prefix and exclude private `!!` shell results. Code Mode inspection is limited to settled parents from the issuing prefix or committed children of its current invocation. An ID, hash or look-alike JSON reference grants no access. Code Mode has its own [saved-child recovery query](tool-control.md#code-mode-recovery).

### Revisit an image

To revisit an image after compaction, call `history` directly with `{"action":"image","entry":N,"image":0}`. The source entry is one-based; the image index is zero-based among its typed images, not text parts. This returns one retained image from a user input, steering message or observed top-level tool result, without reopening a file or rerunning the original tool. Normal image and request limits apply. Code Mode receives only metadata, so viewing the image requires a direct call.

### Copy, backup and storage limits

Conversation clones, forks and delegated forks copy their referenced observed captures before publishing new history; they do not copy unclaimed forensic data. A consistent whole-database backup can retain that data as well. Chunk commits add SQLite/WAL work and disk retention; there is no automatic artifact eviction.

During early unstable development, incompatible Session databases are rejected rather than migrated. Keep backups if retained evidence matters; saved history is not a backup of workspace files.
