#!/usr/bin/env python3
"""A minimal MCP stdio server for tests: initialize, tools/list, and an
echo tool that fails when asked."""
import json
import sys

TOOLS = [
    {
        "name": "echo",
        "description": "Echo the message back",
        "inputSchema": {
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
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
            "result": {
                "protocolVersion": req["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-mcp", "version": "1.0.0"},
            },
        })
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": TOOLS}})
    elif method == "tools/call":
        args = req.get("params", {}).get("arguments", {})
        if args.get("fail"):
            send({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {
                    "isError": True,
                    "content": [{"type": "text", "text": "forced failure"}],
                },
            })
        else:
            send({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {
                    "content": [
                        {"type": "text", "text": f"echo: {args.get('message', '')}"}
                    ]
                },
            })
