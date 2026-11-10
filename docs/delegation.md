# Delegating coding work

Ask Ion to delegate a self-contained task, for example:

> Start a background subagent to review the parser's error handling without editing files. Return its task handle. Keep working on the caller tests here.

The coding `subagent` tool starts another Session using the same engine. The child inherits the captured parent model route, reasoning setting, workspace and selected tools unless supported options explicitly override them. It does not automatically get a separate worktree or sandbox. Give concurrent writers separate workspaces; a read-only request is an instruction, not filesystem isolation.

A child cannot recursively invoke the built-in subagent tool. If `subagent` is excluded from the parent's tools, the model cannot use it.

## Inspect a background task

Open `/tasks` in terminal chat. Search by the full task UUID, its shortened prefix, or displayed model/state text, then press Enter. The selected row shows the full task handle, parent Session and saved child Session path. List summaries show activity metadata, not task prompts, reasoning or provisional answers.

The action view offers:

- **Show progress and details**: current observation, cleanup state and any provisional text. Silence is not proof of a stall; provisional text is not a successful final answer.
- **Stop task**: request cancellation. Effects are not rolled back, and cleanup still has to finish.
- **Show result**: available after cleanup joins. Read the final answer, failure or unknown outcome without removing it.
- **Remove finished task from list**: explicitly release the retained result and any returned, uncommitted guidance. They are shown once more; save guidance you still need before removal. Committed Session facts remain on disk.

Esc dismisses the picker without stopping or removing work. A task marked **Stopping** has not necessarily finished cleanup. **Finished** means joined, not necessarily successful. Inspect the outcome before depending on it.

You can also use the full task UUID directly; replace `UUID` below:

```text
/tasks status UUID
/tasks result UUID
/tasks cancel UUID
/tasks acknowledge UUID
```

The task handle identifies a run. It is not the child's Session UUID or a saved Session filename. Several runs can continue the same saved child conversation while retaining separate outcomes.

Browsing tasks does not send a model message. Inspecting a report does not prove the parent model has read it. To have the model use a result, ask it to inspect that task explicitly.

## Model-side controls

The `subagent` tool supports `start`, `list`, `status`, `wait`, `steer`, `cancel` and `acknowledge`:

- `start` defaults to foreground mode. If its join budget expires, the **same run** transfers to the application owner and returns a handle; do not launch a replacement. Background mode transfers immediately.
- `wait` waits on selected handles without consuming results. Its wait budget is separate from execution timeout; expiry or cancelling a wait leaves transferred children running.
- `steer` queues literal guidance at the child's safe model-step boundary. Queued does not mean committed delivery. Status retains committed, returned or unknown guidance states; late guidance does not implicitly restart a child.
- `cancel` requests stop. `acknowledge` releases a joined result, not its saved conversation.

Startup and execution have separate budgets. Execution timeout requests stop and joins cleanup; it is not a hard cleanup deadline or rollback. A completed answer is still a child's claim: verify changed files and tests independently.

A fresh child starts without copied conversation history. An explicit fork copies a settled historical prefix, not the parent's unfinished Turn. An explicit `resume` uses a settled child Session file, its original workspace and a new run handle, with the current call's configuration. Saved history does not grant execution authority or replay unfinished effects. See [Sessions and alternative conversations](sessions.md).

## Lifetime and recovery

Transferred work survives parent cancellation and Session navigation within the live application. Quitting or interrupting the application requests stop and joins owned work. Retained handles belong to that application owner; reopening a saved conversation does not resurrect background jobs or their controls.

Saved child Sessions and committed relationship/outcome facts survive removing a finished task. Uncommitted guidance is not a saved Session fact. After an interruption, inspect unknown effects before requesting another mutation; a saved handle is not permission to replay them.
