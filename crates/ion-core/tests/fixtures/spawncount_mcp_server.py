#!/usr/bin/env python3
"""MCP fixture that records every process spawn to a marker file.

Each process start appends one line — the supervision tests count
lines to prove /reload reconcile semantics: an unchanged def must
never spawn a second process, a changed def must replace exactly one.
"""
import json
import pathlib
import sys

MARKER = pathlib.Path(sys.argv[1])
TOOLS = [{
    "name": "echo",
    "description": "Echo the message back",
    "inputSchema": {
        "type": "object",
        "properties": {"message": {"type": "string"}},
        "required": ["message"],
    },
}]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


with MARKER.open("a") as handle:
    handle.write("spawn\n")

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
                "serverInfo": {"name": "spawncount-mcp", "version": "1.0.0"},
            },
        })
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": TOOLS}})
    elif method == "tools/call":
        args = req.get("params", {}).get("arguments", {})
        send({
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {
                "content": [
                    {"type": "text", "text": f"echo: {args.get('message', '')}"}
                ]
            },
        })
