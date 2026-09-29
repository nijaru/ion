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
            assert {"read", "edit", "write", "exec", "mcp__demo__greet", "mcp__demo__picture"} <= names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-mcp", "type": "function", "function": {"name": "mcp__demo__greet", "arguments": '{"name":"Ion"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 3:
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-picture", "type": "function", "function": {"name": "mcp__demo__picture", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 4:
            assert body["messages"][-2]["role"] == "tool" and "[image: image/png]" in body["messages"][-2]["content"], body
            assert body["messages"][-1]["role"] == "user" and body["messages"][-1]["content"][1]["image_url"]["url"].startswith("data:image/png;base64,"), body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_IMAGE_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
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
        result = {'tools': [
            {'name': 'greet', 'description': 'Greet a name', 'inputSchema': {'type': 'object', 'properties': {'name': {'type': 'string'}}, 'required': ['name']}},
            {'name': 'picture', 'description': 'Return a picture', 'inputSchema': {'type': 'object', 'properties': {}}},
        ]}
    elif method == 'tools/call':
        if request['params']['name'] == 'greet':
            name = request['params']['arguments']['name']
            with log.open('a') as output: output.write('called:' + name + '\\n')
            result = {'content': [{'type': 'text', 'text': 'Hello, ' + name + '!'}], 'isError': False}
        elif request['params']['name'] == 'picture':
            with log.open('a') as output: output.write('called:picture\\n')
            result = {'content': [{'type': 'text', 'text': 'A tiny picture'}, {'type': 'image', 'mimeType': 'image/png', 'data': 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=='}], 'isError': False}
        else:
            raise AssertionError(request['params']['name'])
    elif method == 'ping':
        result = {}
    else:
        continue
    if 'id' in request:
        sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}) + '\\n')
        sys.stdout.flush()
with log.open('a') as output: output.write('eof\\n')
'''

bad_listing_source = '''import json, sys
from pathlib import Path
log = Path('bad-mcp-events.txt')
log.write_text('started\\n')
for line in sys.stdin.buffer:
    request = json.loads(line)
    if request.get('method') == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'bad-listing', 'version': '1'}}
    elif request.get('method') == 'tools/list':
        result = {'tools': 'invalid list'}
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
    bad_script = work / "bad-listing.py"
    bad_script.write_text(bad_listing_source)
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "mcp-model", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "demo", sys.executable, str(server_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "broken", str(work / "missing-server")], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "bad-listing", sys.executable, str(bad_script)], env=env, check=True, capture_output=True)
        listing = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "demo" in listing.stdout and str(server_script) in listing.stdout
        response = subprocess.run([binary, "--cwd", workspace, "run", "Use the greet tool to greet Ion."], env=env, check=True, capture_output=True, text=True)
        assert response.stdout.strip() == "MCP_OK", response
        assert "broken" in response.stderr and "cannot start MCP server" in response.stderr, response
        assert "bad-listing" in response.stderr and "tool listing failed" in response.stderr, response
        assert "eof" in (workspace / "bad-mcp-events.txt").read_text()
        assert len(requests) == 2
        events = (workspace / "mcp-events.txt").read_text()
        assert "called:Ion" in events and "eof" in events, events
        image_response = subprocess.run([binary, "--cwd", workspace, "run", "Inspect an MCP picture."], env=env, check=True, capture_output=True, text=True)
        assert image_response.stdout.strip() == "MCP_IMAGE_OK", image_response
        assert len(requests) == 4
        events = (workspace / "mcp-events.txt").read_text()
        assert "called:picture" in events and "eof" in events, events
        inspected = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, check=True, capture_output=True, text=True).stdout
        assert "base64 image data omitted" in inspected and "iVBORw0KGgo" not in inspected
        subprocess.run([binary, "mcp", "remove", "demo"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "broken"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "bad-listing"], env=env, check=True, capture_output=True)
        assert subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True).stdout == ""
        print("Ion MCP discovery, tool call and child shutdown: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
