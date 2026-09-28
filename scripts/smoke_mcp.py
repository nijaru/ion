"""Exercise explicit MCP stdio tool discovery, dispatch and lifecycle."""

import json
import os
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        if len(requests) == 1:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert {"read", "edit", "write", "exec", "mcp__demo__greet"} <= names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-mcp", "type": "function", "function": {"name": "mcp__demo__greet", "arguments": '{"name":"Ion"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        else:
            assert any(message.get("role") == "tool" and "Hello, Ion!" in message.get("content", "") for message in body["messages"]), body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        payload = b"".join(b"data: " + json.dumps(change).encode() + b"\n\n" for change in changes) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


server_source = '''import json, sys
from pathlib import Path
log = Path('mcp-events.txt')
log.write_text('started\\n')
for line in sys.stdin.buffer:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'ion-smoke', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'greet', 'description': 'Greet a name', 'inputSchema': {'type': 'object', 'properties': {'name': {'type': 'string'}}, 'required': ['name']}}]}
    elif method == 'tools/call':
        assert request['params']['name'] == 'greet'
        name = request['params']['arguments']['name']
        with log.open('a') as output: output.write('called:' + name + '\\n')
        result = {'content': [{'type': 'text', 'text': 'Hello, ' + name + '!'}], 'isError': False}
    elif method == 'ping':
        result = {}
    else:
        continue
    if 'id' in request:
        sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}) + '\\n')
        sys.stdout.flush()
with log.open('a') as output: output.write('eof\\n')
'''


with tempfile.TemporaryDirectory(prefix="ion-mcp-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    server_script = work / "server.py"
    server_script.write_text(server_source)
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "mcp-model", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "demo", sys.executable, str(server_script)], env=env, check=True, capture_output=True)
        listing = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "demo" in listing.stdout and str(server_script) in listing.stdout
        response = subprocess.run([binary, "--cwd", workspace, "run", "Use the greet tool to greet Ion."], env=env, check=True, capture_output=True, text=True)
        assert response.stdout.strip() == "MCP_OK", response
        assert len(requests) == 2
        events = (workspace / "mcp-events.txt").read_text()
        assert "called:Ion" in events and "eof" in events, events
        subprocess.run([binary, "mcp", "remove", "demo"], env=env, check=True, capture_output=True)
        assert subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True).stdout == ""
        print("Ion MCP discovery, tool call and child shutdown: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
