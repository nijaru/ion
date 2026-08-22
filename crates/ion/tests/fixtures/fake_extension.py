#!/usr/bin/env python3
"""An ion extension speaking the shared stdio JSON-RPC shape: registers
an `upper` tool that uppercases its input, then crashes on demand."""
import json
import sys

TOOLS = [
    {
        "name": "upper",
        "description": "Uppercase the text",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    }
]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


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
            "result": {"protocolVersion": req["params"]["protocolVersion"]},
        })
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": TOOLS}})
    elif method == "tools/call":
        args = req.get("params", {}).get("arguments", {})
        if args.get("crash"):
            sys.exit(1)
        send({
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {
                "content": [{"type": "text", "text": args.get("text", "").upper()}]
            },
        })
