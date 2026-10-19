"""Exercise explicit MCP stdio tool discovery, dispatch and lifecycle."""

import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []
remote_calls = []


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        if len(requests) == 1:
            names = {tool["function"]["name"] for tool in body["tools"]}
            greet = next(name for name in names if name.startswith("mcp__4_demo__greet_user_"))
            assert {"read", "edit", "write", "exec", "mcp__4_demo__greet_user", "mcp__4_demo__picture"} <= names, names
            assert any(name.startswith("mcp__4_demo__query") and len(name) <= 64 for name in names), names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-mcp", "type": "function", "function": {"name": greet, "arguments": '{"name":"Ion"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 3:
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-picture", "type": "function", "function": {"name": "mcp__4_demo__picture", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 4:
            assert body["messages"][-2]["role"] == "tool" and "[image: image/png]" in body["messages"][-2]["content"], body
            assert body["messages"][-1]["role"] == "user" and body["messages"][-1]["content"][1]["image_url"]["url"].startswith("data:image/png;base64,"), body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_IMAGE_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 5:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__6_remote__uppercase" in names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-remote", "type": "function", "function": {"name": "mcp__6_remote__uppercase", "arguments": '{"text":"ion"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 6:
            assert any(message.get("role") == "tool" and "ION" in message.get("content", "") for message in body["messages"]), body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_REMOTE_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 7:
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-unknown", "type": "function", "function": {"name": "mcp__6_remote__uppercase", "arguments": '{"text":"retry-check"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 8:
            assert body["messages"][-1]["role"] == "tool" and "failed" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_UNCERTAIN_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 9:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__8_changing__first" in names and "mcp__8_changing__second" not in names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-first", "type": "function", "function": {"name": "mcp__8_changing__first", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 10:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__8_changing__first" not in names and "mcp__8_changing__second" in names, names
            assert body["messages"][-1]["role"] == "tool" and "FIRST" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-withdrawn", "type": "function", "function": {"name": "mcp__8_changing__first", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 11:
            assert body["messages"][-1]["role"] == "tool" and "tool was not declared for this request" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-second", "type": "function", "function": {"name": "mcp__8_changing__second", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 12:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__8_changing__second" in names and "mcp__8_changing__first" not in names, names
            assert body["messages"][-1]["role"] == "tool" and "SECOND" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_REFRESH_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 13:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__6_remote__uppercase" in names and "mcp__6_remote__lowercase" not in names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-http-shift", "type": "function", "function": {"name": "mcp__6_remote__uppercase", "arguments": '{"text":"shift"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 14:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__6_remote__uppercase" not in names and "mcp__6_remote__lowercase" in names, names
            assert body["messages"][-1]["role"] == "tool" and "SHIFT" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-http-new", "type": "function", "function": {"name": "mcp__6_remote__lowercase", "arguments": '{"text":"ION"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 15:
            assert body["messages"][-1]["role"] == "tool" and "ion" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_HTTP_REFRESH_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 16:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert names == {"read", "mcp__4_demo__large_report"}, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-large", "type": "function", "function": {"name": "mcp__4_demo__large_report", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 17:
            envelope = json.loads(body["messages"][-1]["content"])
            assert envelope["is_error"] is False, envelope
            result = envelope["result"]
            assert result["truncated"] is True and len(result["content"]) < 64 * 1024, result
            path = Path(result["full_output_path"])
            full = json.loads(path.read_text())
            assert full["content"].startswith("START_MARKER\n") and full["content"].endswith("\nEND_MARKER"), full
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "read-head", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": str(path), "byte_offset": 0, "byte_limit": 4096})}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 18:
            envelope = json.loads(body["messages"][-1]["content"])
            assert envelope["is_error"] is False, envelope
            page = envelope["result"]
            assert "START_MARKER\\n" in page["content"], page
            assert page["start_byte"] == 0 and page["has_more"], page
            assert page["next_byte"] == len(page["content"].encode()) <= 4096, page
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "read-middle", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": page["path"], "byte_offset": 32768, "byte_limit": 4096})}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 19:
            envelope = json.loads(body["messages"][-1]["content"])
            assert envelope["is_error"] is False, envelope
            page = envelope["result"]
            assert "MIDDLE_MARKER" in page["content"], page
            assert page["start_byte"] == 32768 and page["has_more"], page
            assert page["next_byte"] == 32768 + len(page["content"].encode()) <= 32768 + 4096, page
            Path(page["path"]).unlink()
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_LARGE_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 20:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert {"list_mcp_resources", "list_mcp_resource_templates", "read_mcp_resource"} <= names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-resource", "type": "function", "function": {"name": "read_mcp_resource", "arguments": '{"server":"demo","uri":"mem://guide"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 21:
            assert body["messages"][-1]["role"] == "tool" and "RESOURCE_OK" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_RESOURCE_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 22:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert names == {"read", "ls", "find", "grep", "history", "list_mcp_resources", "list_mcp_resource_templates", "read_mcp_resource"}, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "READ_ONLY_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 23:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert names == {"read", "write"}, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "TOOL_SELECTION_OK"}, "finish_reason": None}]},
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


class RemoteMcp(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path == "/redirect":
            self.rfile.read(int(self.headers["Content-Length"]))
            self.send_response(307)
            self.send_header("Location", "/redirect-target")
            self.end_headers()
            return
        if self.path == "/redirect-target":
            self.server.redirect_followed = True
            self.send_response(500)
            self.end_headers()
            return
        if self.path in ("/stalled-headers", "/stalled-sse"):
            release = self.server.startup_release
            self.rfile.read(int(self.headers["Content-Length"]))
            self.server.startup_seen.set()
            if self.path == "/stalled-sse":
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                self.wfile.flush()
            # Ion must interrupt and join its own worker, not wait for this
            # fixture to finish sending headers or an initial SSE event.
            release.wait(20)
            self.close_connection = True
            return
        assert self.path == "/mcp", self.path
        assert self.headers.get("Authorization") == "Bearer test-token"
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        method = request.get("method")
        if method == "initialize":
            result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "ion-remote-smoke", "version": "1"}}
        elif method == "tools/list":
            name = "lowercase" if getattr(self.server, "tool_changed", False) else "uppercase"
            result = {"tools": [{"name": name, "description": name, "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}]}
        elif method == "tools/call":
            remote_calls.append(request)
            text = request["params"]["arguments"]["text"]
            if text == "retry-check":
                self.send_response(404)
                self.end_headers()
                return
            result = {"content": [{"type": "text", "text": text.lower() if request["params"]["name"] == "lowercase" else text.upper()}], "isError": False}
            if text == "shift":
                self.server.tool_changed = True
                notification = b'data: {"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n\n'
                response = b"data: " + json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}).encode() + b"\n\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(notification) + len(response)))
                self.end_headers()
                self.wfile.write(notification)
                self.wfile.flush()
                import time
                time.sleep(0.05)
                self.wfile.write(response)
                return
        else:
            self.send_response(202)
            self.end_headers()
            return
        payload = json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.send_response(405)
        self.end_headers()

    def do_DELETE(self):
        self.send_response(200)
        self.end_headers()

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
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}, 'resources': {}}, 'serverInfo': {'name': 'ion-smoke', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [
            {'name': 'greet.user', 'description': 'Greet a name', 'inputSchema': {'type': 'object', 'properties': {'name': {'type': 'string'}}, 'required': ['name']}},
            {'name': 'greet_user', 'description': 'A similarly named tool', 'inputSchema': {'type': 'object'}},
            {'name': 'query' * 22, 'description': 'A long tool name', 'inputSchema': {'type': 'object'}},
            {'name': 'picture', 'description': 'Return a picture', 'inputSchema': {'type': 'object', 'properties': {}}},
            {'name': 'large_report', 'description': 'Return a large text report', 'inputSchema': {'type': 'object'}},
        ]}
    elif method == 'resources/list':
        result = {'resources': [{'uri': 'mem://guide', 'name': 'Guide', 'mimeType': 'text/plain'}]}
    elif method == 'resources/read':
        with log.open('a') as output: output.write('read:' + request['params']['uri'] + '\\n')
        result = {'contents': [{'uri': 'mem://guide', 'mimeType': 'text/plain', 'text': 'RESOURCE_OK\\nGuide body'}]}
    elif method == 'tools/call':
        if request['params']['name'] == 'greet.user':
            name = request['params']['arguments']['name']
            with log.open('a') as output: output.write('called:' + name + '\\n')
            result = {'content': [{'type': 'text', 'text': 'Hello, ' + name + '!'}], 'isError': False}
        elif request['params']['name'] == 'picture':
            with log.open('a') as output: output.write('called:picture\\n')
            result = {'content': [{'type': 'text', 'text': 'A tiny picture'}, {'type': 'image', 'mimeType': 'image/png', 'data': 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=='}], 'isError': False}
        elif request['params']['name'] == 'large_report':
            result = {'content': [{'type': 'text', 'text': 'START_MARKER\\n' + 'x' * (32 * 1024) + '\\nMIDDLE_MARKER\\n' + 'x' * (40 * 1024) + '\\nEND_MARKER'}], 'isError': False}
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

paired_source = '''import json, sys, time
from pathlib import Path
own, peer = map(Path, sys.argv[1:3])
for line in sys.stdin.buffer:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        own.touch()
        deadline = time.monotonic() + 3
        while not peer.exists() and time.monotonic() < deadline: time.sleep(0.01)
        if not peer.exists(): sys.exit(1)
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'paired', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': []}
    else:
        continue
    if 'id' in request:
        sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}) + '\\n')
        sys.stdout.flush()
'''

changing_source = '''import json, sys, time
from pathlib import Path
log = Path('changing-mcp-events.txt')
changed = 0
for line in sys.stdin.buffer:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {'listChanged': True}}, 'serverInfo': {'name': 'changing', 'version': '1'}}
    elif method == 'tools/list':
        name = 'first' if changed == 0 else 'second' if changed == 1 else 'invalid'
        with log.open('a') as output: output.write('list:' + name + '\\n')
        result = {'tools': 'invalid list'} if changed == 2 else {'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'}}]}
    elif method == 'tools/call':
        name = request['params']['name']
        with log.open('a') as output: output.write('called:' + name + '\\n')
        if name in ('first', 'second'):
            changed += 1
            sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/tools/list_changed'}) + '\\n')
            sys.stdout.flush()
            time.sleep(0.05)
        result = {'content': [{'type': 'text', 'text': name.upper()}], 'isError': False}
    else:
        continue
    if 'id' in request:
        sys.stdout.write(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}) + '\\n')
        sys.stdout.flush()
'''


owned_source = '''import json, os, sys, time
from pathlib import Path
Path('owned.pid').write_text(str(os.getpid()))
mode = sys.argv[1]
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    method = request.get('method')
    if method == 'initialize':
        if mode == 'init-error':
            print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32603, 'message': 'owned init fault'}}), flush=True)
            continue
        result = {'protocolVersion': request['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'owned', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': 'invalid' if mode == 'list-error' else []}
    else: continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
Path('owned.eof').touch()
time.sleep(60)
'''


def check_owned_cleanup():
    # Offline startup/exit only: no provider requests or live server routes.
    with tempfile.TemporaryDirectory(prefix="ion-mcp-owned-") as temporary:
        work = Path(temporary)
        fixture = work / "owned-server.py"
        fixture.write_text(owned_source)
        env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
        subprocess.run([binary, "use", "offline", "model", "--endpoint", "http://127.0.0.1:1/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        cases = [
            ("exit", "ready", ["rpc"], None),
            ("preflight", "ready", ["--reasoning", "budget:128", "rpc"], "exact reasoning-token budgets"),
            ("input-setup", "ready", ["run", "/probe 'unfinished"], "quote"),
            ("init-fault", "init-error", ["rpc"], "initialization failed"),
            ("listing-fault", "list-error", ["rpc"], "tool listing failed"),
            ("diagnostic-io", "ready", ["rpc"], None),
            ("preflight-io", "ready", ["--reasoning", "budget:128", "rpc"], None),
        ]
        config_path = work / "config/ion/mcp.json"
        for name, mode, args, error in cases:
            workspace = work / name
            workspace.mkdir()
            prompts = workspace / ".ion/prompts"
            prompts.mkdir(parents=True)
            (prompts / "probe.md").write_text("Inspect $@")
            configured = {"owned": {"command": sys.executable, "args": [str(fixture), mode]}}
            if name in ("preflight", "diagnostic-io", "preflight-io"):
                configured["broken"] = {"command": str(work / "missing-server")}
            config_path.write_text(json.dumps({"servers": configured}))
            pid = None
            try:
                # File-backed output separates CLI exit from inherited stderr
                # still held by a leaked MCP root; inspect its PID separately.
                with (workspace / "stdout").open("w+") as out, (workspace / "stderr").open("w+") as err:
                    if name.endswith("-io"):
                        reader, writer = os.pipe()
                        os.close(reader)
                        try:
                            response = subprocess.run([binary, "--cwd", workspace, *args], input="", env=env, stdout=out, stderr=writer, text=True, timeout=25)
                        finally:
                            os.close(writer)
                        assert response.returncode == 1, f"{name}: CLI diagnostic delivery panicked: {response.returncode}"
                        assert out.tell() == 0, f"{name}: RPC started despite failed startup diagnostics"
                    else:
                        response = subprocess.run([binary, "--cwd", workspace, *args], input="", env=env, stdout=out, stderr=err, text=True, timeout=25)
                    out.seek(0)
                    err.seek(0)
                    response.stdout = out.read()
                    response.stderr = err.read()
                pid = int((workspace / "owned.pid").read_text())
                process = subprocess.run(["ps", "-p", str(pid), "-o", "pid="], capture_output=True, text=True)
                assert not process.stderr, process
                assert not process.stdout.strip(), f"{name}: owned root {pid} survived or was not reaped"
                assert (workspace / "owned.eof").exists(), f"{name}: transport did not close stdin"
                if error:
                    assert error in response.stderr, response
                if name == "preflight":
                    assert "cannot start MCP server broken" in response.stderr, response
                expected_failure = name in ("preflight", "input-setup", "diagnostic-io", "preflight-io")
                assert (response.returncode != 0) == expected_failure, response
            finally:
                # On regression, clean up only the root still running this exact
                # isolated fixture, never another process that reused its PID.
                if pid is None and (workspace / "owned.pid").exists():
                    pid = int((workspace / "owned.pid").read_text())
                if pid is not None:
                    command = subprocess.run(["ps", "-ww", "-p", str(pid), "-o", "command="], capture_output=True, text=True).stdout
                    if str(fixture) in command:
                        os.kill(pid, signal.SIGKILL)
    print("Ion MCP owned roots on normal exit, preflight and startup faults: OK")


check_owned_cleanup()

with tempfile.TemporaryDirectory(prefix="ion-mcp-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    server_script = work / "server.py"
    server_script.write_text(server_source)
    bad_script = work / "bad-listing.py"
    bad_script.write_text(bad_listing_source)
    paired_script = work / "paired.py"
    paired_script.write_text(paired_source)
    changing_script = work / "changing.py"
    changing_script.write_text(changing_source)
    for directory in ("home", "tmp", "cache"):
        (work / directory).mkdir()
    env = {**os.environ, "HOME": str(work / "home"), "TMPDIR": str(work / "tmp"), "XDG_CACHE_HOME": str(work / "cache"), "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "REMOTE_MCP_TOKEN": "test-token"}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    remote = ThreadingHTTPServer(("127.0.0.1", 0), RemoteMcp)
    remote_thread = threading.Thread(target=remote.serve_forever, daemon=True)
    remote_thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "mcp-model", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        remote.redirect_followed = False
        subprocess.run([binary, "mcp", "add-http", "redirected", f"http://127.0.0.1:{remote.server_port}/redirect", "--bearer-token-env", "REMOTE_MCP_TOKEN"], env=env, check=True, capture_output=True)
        redirected = subprocess.run([binary, "--cwd", workspace, "rpc"], input="", env=env, check=True, capture_output=True, text=True, timeout=15)
        assert "initialization failed" in redirected.stderr, redirected
        assert not remote.redirect_followed, "HTTP startup replayed credentials to a redirect target"
        subprocess.run([binary, "mcp", "remove", "redirected"], env=env, check=True, capture_output=True)
        for stage in ("headers", "sse"):
            remote.startup_release = threading.Event()
            remote.startup_seen = threading.Event()
            subprocess.run([binary, "mcp", "add-http", "stalled", f"http://127.0.0.1:{remote.server_port}/stalled-{stage}"], env=env, check=True, capture_output=True)
            try:
                stalled = subprocess.run([binary, "--cwd", workspace, "rpc"], input="", env=env, check=True, capture_output=True, text=True, timeout=15)
                assert remote.startup_seen.is_set(), "HTTP bootstrap was not exercised"
                assert "initialization timed out" in stalled.stderr, stalled
            finally:
                remote.startup_release.set()
                subprocess.run([binary, "mcp", "remove", "stalled"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "demo", sys.executable, str(server_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "broken", str(work / "missing-server")], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "bad-listing", sys.executable, str(bad_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "paired-left", sys.executable, str(paired_script), "paired-left", "paired-right"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "paired-right", sys.executable, str(paired_script), "paired-right", "paired-left"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "changing", sys.executable, str(changing_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add-http", "remote", f"http://127.0.0.1:{remote.server_port}/mcp", "--bearer-token-env", "REMOTE_MCP_TOKEN"], env=env, check=True, capture_output=True)
        listing = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "demo" in listing.stdout and "demo\tdirect\t" in listing.stdout, listing.stdout
        assert str(server_script) in listing.stdout and "/mcp" in listing.stdout
        subprocess.run([binary, "mcp", "add", "flag-probe", "--exposure", "hidden", sys.executable, "-c", "import sys"], env=env, check=True, capture_output=True)
        flagged = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "flag-probe\thidden\t" in flagged.stdout, flagged.stdout
        subprocess.run([binary, "mcp", "remove", "flag-probe"], env=env, check=True, capture_output=True)
        config_path = Path(env["XDG_CONFIG_HOME"]) / "ion" / "mcp.json"
        config = json.loads(config_path.read_text())
        config["servers"]["malformed"] = {"command": 7}
        config_path.write_text(json.dumps(config))
        subprocess.run([binary, "mcp", "remove", "demo"], env=env, check=True, capture_output=True, text=True)
        edited = json.loads(config_path.read_text())
        assert "demo" not in edited["servers"] and "malformed" in edited["servers"], edited
        subprocess.run([binary, "mcp", "add", "demo", sys.executable, str(server_script)], env=env, check=True, capture_output=True)
        edited = json.loads(config_path.read_text())
        assert "demo" in edited["servers"] and "malformed" in edited["servers"], edited
        response = subprocess.run([binary, "--cwd", workspace, "run", "Use the greet tool to greet Ion."], env=env, check=True, capture_output=True, text=True)
        assert response.stdout.strip() == "MCP_OK", response
        assert "malformed" in response.stderr and "invalid MCP server" in response.stderr, response
        assert "paired-left" not in response.stderr and "paired-right" not in response.stderr, response
        assert (workspace / "paired-left").exists() and (workspace / "paired-right").exists()
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
        remote_response = subprocess.run([binary, "--cwd", workspace, "run", "Uppercase ion using the remote tool."], env=env, check=True, capture_output=True, text=True)
        assert remote_response.stdout.strip() == "MCP_REMOTE_OK", remote_response
        assert len(remote_calls) == 1 and remote_calls[0]["params"]["name"] == "uppercase", remote_calls
        uncertain = subprocess.run([binary, "--cwd", workspace, "run", "Call the remote tool for retry-check."], env=env, check=True, capture_output=True, text=True)
        assert uncertain.stdout.strip() == "MCP_UNCERTAIN_OK", uncertain
        assert len(remote_calls) == 2 and remote_calls[1]["params"]["arguments"]["text"] == "retry-check", remote_calls
        changed = subprocess.run([binary, "--cwd", workspace, "run", "Call the changing MCP tools."], env=env, check=True, capture_output=True, text=True)
        assert changed.stdout.strip() == "MCP_REFRESH_OK", changed
        assert "tool listing failed" in changed.stderr and "changing" in changed.stderr, changed
        events = (workspace / "changing-mcp-events.txt").read_text()
        assert "list:first" in events and "list:second" in events and "list:invalid" in events, events
        assert events.count("called:first") == 1 and events.count("called:second") == 1, events
        http_changed = subprocess.run([binary, "--cwd", workspace, "run", "Call the remote changing tools."], env=env, check=True, capture_output=True, text=True)
        assert http_changed.stdout.strip() == "MCP_HTTP_REFRESH_OK", http_changed
        assert len(remote_calls) == 4 and remote_calls[-1]["params"]["name"] == "lowercase", remote_calls
        large = subprocess.run([binary, "--cwd", workspace, "--tools", "read,mcp__4_demo__large_report", "run", "Read the large MCP report, including its omitted middle."], env=env, check=True, capture_output=True, text=True, timeout=30)
        assert large.stdout.strip() == "MCP_LARGE_OK", large
        resource = subprocess.run([binary, "--cwd", workspace, "run", "Read the MCP guide resource."], env=env, check=True, capture_output=True, text=True)
        assert resource.stdout.strip() == "MCP_RESOURCE_OK", resource
        events = (workspace / "mcp-events.txt").read_text()
        assert "read:mem://guide" in events, events
        read_only = subprocess.run([binary, "--cwd", workspace, "--read-only", "run", "Inspect without mutations."], env=env, check=True, capture_output=True, text=True)
        assert read_only.stdout.strip() == "READ_ONLY_OK", read_only
        selected = subprocess.run([binary, "--cwd", workspace, "--tools", "read,write", "run", "Read and write only."], env=env, check=True, capture_output=True, text=True)
        assert selected.stdout.strip() == "TOOL_SELECTION_OK", selected

        # Project MCP configuration is inert until explicitly trusted for its
        # current contents. Editing it invalidates trust.
        (workspace / ".git").mkdir()
        (workspace / ".ion").mkdir()
        project_config = workspace / ".ion/mcp.json"
        project_config.write_text(json.dumps({"servers": {"project-demo": {"command": sys.executable, "args": ["-c", "import sys"]}}}))
        untrusted = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "project-demo" not in untrusted.stdout and "not trusted" in untrusted.stderr, untrusted
        trusted = subprocess.run([binary, "--cwd", workspace, "trust"], env=env, check=True, capture_output=True, text=True)
        assert "Trusted project MCP configuration" in trusted.stdout, trusted
        listed = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "project-demo\tdirect\t" in listed.stdout, listed
        changed_config = json.loads(project_config.read_text())
        changed_config["servers"]["project-two"] = {"command": sys.executable, "args": ["-c", "import sys"]}
        project_config.write_text(json.dumps(changed_config))
        changed = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "project-demo" not in changed.stdout and "not trusted" in changed.stderr, changed
        subprocess.run([binary, "--cwd", workspace, "trust"], env=env, check=True, capture_output=True)
        added = subprocess.run([binary, "--cwd", workspace, "mcp", "add", "--project", "project-added", "--exposure", "hidden", sys.executable, "-c", "import sys"], env=env, check=True, capture_output=True, text=True)
        assert "run `ion trust`" in added.stdout, added
        pending = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "project-added" not in pending.stdout and "not trusted" in pending.stderr, pending
        subprocess.run([binary, "--cwd", workspace, "trust"], env=env, check=True, capture_output=True)
        added_listed = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "project-added\thidden\t" in added_listed.stdout, added_listed
        subprocess.run([binary, "--cwd", workspace, "mcp", "remove", "--project", "project-added"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "--cwd", workspace, "untrust"], env=env, check=True, capture_output=True)
        project_config.unlink()
        (workspace / ".ion/mcp.lock").unlink(missing_ok=True)
        (workspace / ".ion").rmdir()
        config["servers"].pop("malformed")
        config_path.write_text(json.dumps(config))
        subprocess.run([binary, "mcp", "remove", "demo"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "broken"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "bad-listing"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "paired-left"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "paired-right"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "changing"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "remote"], env=env, check=True, capture_output=True)
        assert subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True).stdout == ""
        print("Ion MCP discovery, tool call and child shutdown: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        remote.shutdown()
        remote.server_close()
        remote_thread.join(timeout=5)
