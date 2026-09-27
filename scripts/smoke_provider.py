"""Deterministic local Chat Completions stream for the executable smoke check."""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


port_file, requests_file = map(Path, sys.argv[1:3])
steps = [
    ("read", {"path": "data.txt"}),
    ("edit", {"path": "data.txt", "old_text": "sample data", "new_text": "sample data updated"}),
    ("write", {"path": "created.txt", "content": "created by ion\n"}),
    ("exec", {"command": "cat data.txt created.txt"}),
]
count = 0


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        global count
        try:
            assert self.path == "/v1/chat/completions"
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with requests_file.open("a") as trace:
                trace.write(json.dumps(body) + "\n")
            assert body["stream"] is True
            assert len(body["tools"]) == 4
            assert {tool["function"]["name"] for tool in body["tools"]} == {"read", "edit", "write", "exec"}
            if count < 4:
                if count:
                    last = body["messages"][-1]
                    assert last["role"] == "tool" and last["tool_call_id"] == f"ion_call_{count - 1}"
                name, arguments = steps[count]
                delta = {"tool_calls": [{"index": 0, "id": f"call-{count + 1}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
                finish = "tool_calls"
            elif count == 4:
                assert body["messages"][-1]["role"] == "tool"
                assert "sample data updated" in body["messages"][-1]["content"]
                delta, finish = {"content": "TASK_COMPLETE"}, "stop"
            elif count == 5:
                assert len([m for m in body["messages"] if m["role"] == "user"]) == 2
                delta, finish = {"content": "RESUMED"}, "stop"
            else:
                raise AssertionError("unexpected extra model request")
            count += 1
            events = [
                {"id": "smoke", "model": "smoke-model", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                {"id": "smoke", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]},
            ]
            payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except Exception as error:
            print(f"mock provider request {count} failed: {error!r}", file=sys.stderr, flush=True)
            self.send_error(500, str(error))

    def log_message(self, format, *args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
port_file.write_text(str(server.server_port))
server.serve_forever()
