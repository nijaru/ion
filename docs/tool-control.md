# Tool selection and operator vetoes

Ion normally runs coding tools with your host permissions. It does not ask for approval before every tool call. Use tool selection to narrow what the model can call, or an optional operator-owned program to refuse individual calls. Neither mechanism is an OS sandbox.

## Select the model's tools

```sh
ion --read-only
ion --tools read,ls,find,grep,exec
ion --exclude-tools exec,process
```

Run `ion --help` for supported selection options. A veto cannot restore an excluded or undeclared tool. `--read-only` also disables direct shell execution in the coding client. These controls do not isolate the filesystem, credentials or arbitrary trusted host code.

## Configure a veto program

Add `tool_veto` to your operator settings file, `~/.ion/settings.json` or `$ION_HOME/settings.json`:

```json
{
  "tool_veto": {
    "program": "/absolute/path/to/python3",
    "args": ["/absolute/path/to/ion-policy.py"],
    "timeout_ms": 30000
  }
}
```

Use your own absolute paths. This is operator configuration, not a workspace `.ion` policy file. Keep the program and settings under your control. `args` defaults to `[]`; the default timeout is 30 seconds. Omit `tool_veto` or set it to `null` to disable it for a new runtime.

`ion doctor` validates settings without executing the program. It does not prove that the program can start or return a valid decision; those failures refuse the attempted tool call.

### Request and reply

Before dispatching a selected model tool, Ion starts a one-shot program in the captured working directory, using the runtime's captured command environment. It sends one UTF-8 JSON object followed by a newline and closes stdin. The object contains:

- `tool_name` and the original `arguments` object;
- `call_id`, the provider's call identifier;
- `origin`, with Session UUID, Turn, tool occurrence and optional composed-child index;
- `cwd`, the captured canonical working directory.

Core checks the call envelope and frozen tool catalog before invoking the policy, but does not validate every tool's full argument schema at this point. Validate any fields your policy uses. Origin is provenance, not a credential, execution lease or permission to replay an effect. Do not interpolate model arguments into a shell command.

Exit zero and write exactly one JSON decision to stdout:

```json
{"decision":"allow"}
```

or:

```json
{"decision":"deny","reason":"This operation is disabled by the operator."}
```

Denial reasons must be nonblank and at most 4,096 UTF-8 bytes. They are shown to the user and model and retained in the Session; do not put secrets or private diagnostics in them. Extra fields, malformed JSON, replies over 65,536 bytes, nonzero exit, timeout and cancellation refuse dispatch. Stderr is drained but not exposed to the model or retained as a Session artifact. This is a non-interactive stdin/stdout protocol, not a terminal approval prompt.

For example, this diagnostic policy refuses the `write` tool and allows other calls:

```python
import json
import sys

request = json.load(sys.stdin)
if request["tool_name"] == "write":
    reply = {"decision": "deny", "reason": "The operator disabled write."}
else:
    reply = {"decision": "allow"}
print(json.dumps(reply))
```

This example is **not a read-only or filesystem security policy**: other tools, shell commands, MCP servers or trusted host code may mutate files. It demonstrates the protocol, not a complete protection strategy.

## Scope, changes and failures

The same policy gates model-issued direct calls, Code Mode itself and its nested calls, and tools in managed coding children. It can also refuse intrinsic routes such as tool discovery or history. A denial records `not_dispatched` for that call; native chat shows **Skipped** with the reason. Compact previews may shorten long reasons; use the displayed details shortcut to inspect the full recorded message. A denial does not claim that earlier calls had no effects or undo anything already performed.

Where enabled, explicit user shell execution, user commands and human input to an attached PTY are separate application actions, not model tool calls governed by this hook. The veto program itself runs with host permissions. Its timeout covers exchange and exit, not a hard cleanup deadline; Ion requests termination and joins the one-shot program scope before returning. Daemonized descendants are not contained.

Program/argv settings are captured when the coding runtime is created. Editing settings or running resource-only `/reload` does not replace the active policy. Restart Ion to apply changed settings; explicit runtime replacements, including model selection, also capture current operator settings. Managed children inherit the parent's captured configuration rather than rereading ambient settings. The program's executable and script contents are not pinned: editing those files can change what the next invocation does.

After a refusal, inspect the reason and repair operator configuration if needed. A new user message uses the active runtime; it does not automatically replay the refused call. A broken policy fails closed rather than silently granting access.

## Embedded hosts

`ion_core::ToolPolicy` and `ToolSet::with_policy` provide the in-process veto boundary. A policy receives immutable call/catalog facts and cancellation; it returns `Allow` or a denial reason, not replacement arguments, results or later execution authority.

The coding recipe exposes `CodingApplication::with_tool_veto` for the program adapter. Supplying `with_command_environment` skips ambient user settings, including veto settings; such a host must install any desired veto explicitly. Keep started policy work owned and joined through cancellation. These interfaces are application controls, not isolation from malicious trusted host code.
