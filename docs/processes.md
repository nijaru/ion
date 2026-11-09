# Long-running programs

The coding application's `process` tool starts programs that can outlive a Turn. Ask the agent to start a server, watch command or interactive program with this tool rather than waiting for an ordinary shell command to finish.

For example:

> Start `python3 -i` with the process tool's PTY backend. Return its handle and leave it running.

The default backend is `pipe`, with separate stdout and stderr. Choose `pty` for programs that need a controlling terminal or human input. A successful start means the shell spawned, not that a server is ready; ask the agent to check its output or readiness endpoint.

## Attach and detach

In terminal chat, enter:

```text
/attach
```

With one running PTY program in the current Session, Ion attaches to it automatically. With several, use `/attach UUID`, using the handle returned by the tool. Pipe-backed programs cannot be attached.

While attached:

- **Ctrl-]** detaches without requesting a stop. The program may continue or end on its own.
- **Ctrl-C** is sent to the program, not used to stop the agent's Turn. Its effect depends on the program and its terminal settings.
- Other supported keys and pasted text go to the program. Bracketed paste is used when the program enables it; large pastes remain one paste region across bounded writes.

Detaching restores the composer and releases human input custody before returning. The agent can then write or resize the program again. While attached, model writes and resize are refused; model observations and lifecycle controls remain available.

When the program ends, Ion closes the attachment and reports **Program ended**. This does not imply a successful exit. Ask the agent to inspect the exit result and retained output.

## Inspect, stop and release

Ask the agent to use the existing process actions:

| Action | Purpose |
| --- | --- |
| `list` / `inspect` | Observe handles and lifecycle state in this Session and working directory. |
| `read` / `screen` | Read retained output or the PTY screen projection. |
| `write` / `resize` | Send input or resize a live program when no human is attached. |
| `wait` | Wait for the real driver to join without stopping the program; a timeout is only an observation. |
| `close` | Request a process-group stop and join the driver. |
| `acknowledge` | Release a retained slot after it has joined. |

For example: “Stop that server with `process.close`, inspect its result, then acknowledge its handle.” Detaching alone does none of these actions. PTY EOF is terminal input: Ctrl-D may signal EOF depending on the program; the `eof` action is for pipes only.

## Limits and lifetime

Programs belong to the live application owner, invoking Session and captured working directory. Handles are opaque UUIDs, not PIDs. Switching away from a context or shutting down requests stop and join for its programs; delegated coding jobs close their own programs before settling. Reopening a saved conversation does **not** resurrect programs or make old handles controllable by a new owner.

Commands run with your host permissions, not in a sandbox. Stopping a process group is not containment of daemonized descendants and does not undo effects. Input writes report acknowledged bytes and may be partial or cancelled; an incomplete paste cannot be rolled back or safely replayed automatically.

Output capture is a rolling 64 KiB window per stream, not a complete durable log. PTY attachment uses a bounded text screen projection with cursor and input-mode metadata, not transparent forwarding of every terminal feature or style. Keep important logs in files and inspect gaps, exit state and capture completeness separately.
