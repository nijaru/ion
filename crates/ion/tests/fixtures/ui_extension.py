#!/usr/bin/env python3
"""An ion extension exercising the whole UI contribution surface:
tools, footer status pushes, widgets, a registered command, and a
select dialog. One process, driven by request methods; see
extensions_ui.rs for the host-side expectations."""
import json
import sys

TOOLS = [
    {
        "name": "ping",
        "description": "UI-capable extension ping",
        "inputSchema": {
            "type": "object",
            "properties": {"crash": {"type": "boolean"}},
            "required": [],
        },
    }
]

COMMANDS = [{"name": "greet", "description": "Greet someone"}]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def notify(method, params):
    send({"jsonrpc": "2.0", "method": method, "params": params})


for line in sys.stdin:
    try:
        req = json.loads(line)
    except ValueError:
        continue
    method = req.get("method")
    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {
                "protocolVersion": req["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "ui-extension", "version": "1.0.0"},
            },
        })
        # After initialize the peer pushes UI state: a footer status
        # and a widget above the composer (pi ctx.ui parity).
        notify("ion/ui/status", {"key": "uikit", "text": "ready"})
        notify("ion/ui/widget", {
            "key": "hint",
            "lines": ["uikit widget line 1", "uikit widget line 2"],
        })
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": TOOLS}})
    elif method == "tools/call":
        args = req.get("params", {}).get("arguments", {})
        if args.get("crash"):
            sys.exit(1)
        # A tool call that pushes a status update, then answers.
        notify("ion/ui/status", {"key": "uikit", "text": "served ping"})
        send({
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {"content": [{"type": "text", "text": "pong"}]},
        })
    elif method == "ion/commands/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": COMMANDS})
    elif method == "ion/command/run":
        params = req.get("params", {})
        command = params.get("command", "")
        args = params.get("args", "")
        if command == "greet":
            # Elicitation round trip inside a command: ask the user for
            # their name, then finish the command with a message.
            send({
                "jsonrpc": "2.0",
                "id": "ask-name",
                "method": "elicitation/create",
                "params": {
                    "message": "Who should I greet?",
                    "requestedSchema": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "title": "Name",
                                "description": "e.g. Ada",
                            }
                        },
                        "required": ["name"],
                    },
                },
            })
            # The answer arrives as a response to our request id; the
            # loop below resolves it when it shows up on stdin.
            pending_command = {"id": req["id"], "command": command}
            # store and continue; handled when the elicitation result arrives
            globals()["PENDING"] = pending_command
        else:
            send({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {"message": f"ran {command} {args}".strip()},
            })
    elif "id" in req and req.get("result") is not None:
        # This is a response to OUR elicitation request (ask-name).
        result = req.get("result", {})
        action = result.get("action")
        pending = globals().get("PENDING")
        if action == "accept" and pending:
            name = result.get("content", {}).get("name", "stranger")
            message = f"hello {name} (from uikit)"
            notify("ion/ui/status", {"key": "uikit", "text": f"greeted {name}"})
            send({"jsonrpc": "2.0", "id": pending["id"], "result": {"message": message}})
            globals()["PENDING"] = None
        elif pending:
            send({
                "jsonrpc": "2.0",
                "id": pending["id"],
                "result": {"message": "greet declined"},
            })
            globals()["PENDING"] = None
    elif method == "notifications/cancelled":
        # Host cancelled our elicitation: answer the parked command.
        pending = globals().get("PENDING")
        if pending:
            send({
                "jsonrpc": "2.0",
                "id": pending["id"],
                "result": {"message": "greet cancelled"},
            })
            globals()["PENDING"] = None
