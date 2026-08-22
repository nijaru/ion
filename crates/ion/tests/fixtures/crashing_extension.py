#!/usr/bin/env python3
"""An extension that answers the handshake and discovery, then exits
before any tool call can complete."""
import json
import sys


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
        send({
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {
                "tools": [
                    {"name": "ghost", "description": "never completes",
                     "inputSchema": {"type": "object", "required": []}}
                ]
            },
        })
        sys.exit(1)
