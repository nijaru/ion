#!/usr/bin/env python3
"""MCP fixture that exits after its first discovery, then stays available."""
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
                "serverInfo": {"name": "restarting-mcp", "version": "1.0.0"},
            },
        })
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req["id"], "result": {"tools": TOOLS}})
        if not MARKER.exists():
            MARKER.write_text("restarted\n")
            sys.exit(0)
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
