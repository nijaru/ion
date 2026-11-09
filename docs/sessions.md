# Sessions and alternative conversations

Ion saves conversations in SQLite. Reopening a Session restores history; it does not restart interrupted work or replay recorded tool calls.

## Find and reopen

From a terminal:

```sh
ion --continue       # latest Session in this directory
ion -r               # browse saved Sessions
ion sessions         # list saved Sessions
ion --session UUID   # reopen a particular Session
```

In chat, `/name Fix parser tests` names the current Session. `/resume` opens the saved-Session browser; search by name, prompt preview, ID or directory. Quoted phrases and multiple search terms are supported. The browser marks the current Session and shows the full ID and recorded model in its details. Follow the displayed controls to filter named Sessions or cancel without switching.

The live `/resume` browser stays in the current working directory. To choose a Session from another directory, start with `ion -r` and use its all-folders control. `/session` shows the current Session and context details; `/new` starts a new conversation.

After an interruption, inspect unknown effects before asking to repeat a mutation. Send a new message to continue: it starts a distinct Turn under the current runtime, not a replay of the interrupted Turn.

## Explore context without copying

`/tree` shows recorded message boundaries, including alternative paths. Its initial **Current context · keep selection** choice closes the browser without changing context, rebinding the runtime or writing a navigation fact.

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
