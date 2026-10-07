"""Reject special files at the real native-read and startup-instruction boundaries."""

import json
import os
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        results = [message for message in body["messages"] if message["role"] == "tool"]
        if results:
            assert len(results) == 1 and "regular file" in results[0]["content"], results
            delta = {"content": "FIFO_REJECTED"}
            reason = "stop"
        else:
            delta = {"tool_calls": [{"index": 0, "id": "fifo", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": "pipe"})}}]}
            reason = "tool_calls"
        events = [
            {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]},
        ]
        wire = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(wire)))
        self.end_headers()
        self.wfile.write(wire)

    def log_message(self, *args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-file-io-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    os.mkfifo(workspace / "pipe")
    env = {**os.environ, "HOME": str(work / "home"), "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        result = subprocess.run([binary, "--cwd", workspace, "run", "Read the pipe and report the refusal."], env=env, capture_output=True, text=True, timeout=5)
        assert result.returncode == 0 and result.stdout.strip() == "FIFO_REJECTED", result
        assert len(requests) == 2, len(requests)
        requests.clear()
        os.mkfifo(workspace / "AGENTS.md")
        result = subprocess.run([binary, "--cwd", workspace, "run", "Never admit this input."], env=env, capture_output=True, text=True, timeout=5)
        assert result.returncode != 0 and "regular file" in result.stderr, result
        assert not requests, "invalid instructions reached the provider"
        print("Ion native read and startup instructions reject FIFOs without blocking: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
