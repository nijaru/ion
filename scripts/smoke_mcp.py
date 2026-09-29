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
remote_calls = []


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        if len(requests) == 1:
            names = {tool["function"]["name"] for tool in body["tools"]}
            greet = next(name for name in names if name.startswith("mcp__demo__greet_user_"))
            assert {"read", "edit", "write", "exec", "mcp__demo__greet_user", "mcp__demo__picture"} <= names, names
            assert any(name.startswith("mcp__demo__query") and len(name) <= 64 for name in names), names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-mcp", "type": "function", "function": {"name": greet, "arguments": '{"name":"Ion"}'}}]}, "finish_reason": None}]},
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
        elif len(requests) == 5:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__remote__uppercase" in names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-remote", "type": "function", "function": {"name": "mcp__remote__uppercase", "arguments": '{"text":"ion"}'}}]}, "finish_reason": None}]},
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
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-unknown", "type": "function", "function": {"name": "mcp__remote__uppercase", "arguments": '{"text":"retry-check"}'}}]}, "finish_reason": None}]},
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
            assert "mcp__changing__first" in names and "mcp__changing__second" not in names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-first", "type": "function", "function": {"name": "mcp__changing__first", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 10:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__changing__first" not in names and "mcp__changing__second" in names, names
            assert body["messages"][-1]["role"] == "tool" and "FIRST" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-withdrawn", "type": "function", "function": {"name": "mcp__changing__first", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 11:
            assert body["messages"][-1]["role"] == "tool" and "unknown tool" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-second", "type": "function", "function": {"name": "mcp__changing__second", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 12:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__changing__second" in names and "mcp__changing__first" not in names, names
            assert body["messages"][-1]["role"] == "tool" and "SECOND" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_REFRESH_OK"}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(requests) == 13:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__remote__uppercase" in names and "mcp__remote__lowercase" not in names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-http-shift", "type": "function", "function": {"name": "mcp__remote__uppercase", "arguments": '{"text":"shift"}'}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 14:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert "mcp__remote__uppercase" not in names and "mcp__remote__lowercase" in names, names
            assert body["messages"][-1]["role"] == "tool" and "SHIFT" in body["messages"][-1]["content"], body
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-http-new", "type": "function", "function": {"name": "mcp__remote__lowercase", "arguments": '{"text":"ION"}'}}]}, "finish_reason": None}]},
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
            assert "mcp__demo__large_report" in names, names
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-large", "type": "function", "function": {"name": "mcp__demo__large_report", "arguments": "{}"}}]}, "finish_reason": None}]},
                {"id": "mcp", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif len(requests) == 17:
            result = json.loads(body["messages"][-1]["content"])
            assert result["truncated"] is True and len(result["content"]) < 64 * 1024, result
            path = Path(result["full_output_path"])
            full = json.loads(path.read_text())
            assert full["content"].startswith("START_MARKER\n") and full["content"].endswith("\nEND_MARKER"), full
            path.unlink()
            changes = [
                {"id": "mcp", "choices": [{"index": 0, "delta": {"content": "MCP_LARGE_OK"}, "finish_reason": None}]},
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
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'ion-smoke', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [
            {'name': 'greet.user', 'description': 'Greet a name', 'inputSchema': {'type': 'object', 'properties': {'name': {'type': 'string'}}, 'required': ['name']}},
            {'name': 'greet_user', 'description': 'A similarly named tool', 'inputSchema': {'type': 'object'}},
            {'name': 'query' * 22, 'description': 'A long tool name', 'inputSchema': {'type': 'object'}},
            {'name': 'picture', 'description': 'Return a picture', 'inputSchema': {'type': 'object', 'properties': {}}},
            {'name': 'large_report', 'description': 'Return a large text report', 'inputSchema': {'type': 'object'}},
        ]}
    elif method == 'tools/call':
        if request['params']['name'] == 'greet.user':
            name = request['params']['arguments']['name']
            with log.open('a') as output: output.write('called:' + name + '\\n')
            result = {'content': [{'type': 'text', 'text': 'Hello, ' + name + '!'}], 'isError': False}
        elif request['params']['name'] == 'picture':
            with log.open('a') as output: output.write('called:picture\\n')
            result = {'content': [{'type': 'text', 'text': 'A tiny picture'}, {'type': 'image', 'mimeType': 'image/png', 'data': 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=='}], 'isError': False}
        elif request['params']['name'] == 'large_report':
            result = {'content': [{'type': 'text', 'text': 'START_MARKER\\n' + 'x' * (70 * 1024) + '\\nEND_MARKER'}], 'isError': False}
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
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "REMOTE_MCP_TOKEN": "test-token"}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    remote = ThreadingHTTPServer(("127.0.0.1", 0), RemoteMcp)
    remote_thread = threading.Thread(target=remote.serve_forever, daemon=True)
    remote_thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "mcp-model", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "demo", sys.executable, str(server_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "broken", str(work / "missing-server")], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "bad-listing", sys.executable, str(bad_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "paired-left", sys.executable, str(paired_script), "paired-left", "paired-right"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "paired-right", sys.executable, str(paired_script), "paired-right", "paired-left"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add", "changing", sys.executable, str(changing_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add-http", "remote", f"http://127.0.0.1:{remote.server_port}/mcp", "--bearer-token-env", "REMOTE_MCP_TOKEN"], env=env, check=True, capture_output=True)
        listing = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "demo" in listing.stdout and str(server_script) in listing.stdout and "/mcp" in listing.stdout
        config_path = Path(env["XDG_CONFIG_HOME"]) / "ion" / "mcp.json"
        config = json.loads(config_path.read_text())
        config["servers"]["malformed"] = {"command": 7}
        config_path.write_text(json.dumps(config))
        invalid_edit = subprocess.run([binary, "mcp", "remove", "demo"], env=env, capture_output=True, text=True)
        assert invalid_edit.returncode != 0 and "malformed" in invalid_edit.stderr, invalid_edit
        assert "demo" in config_path.read_text()
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
        large = subprocess.run([binary, "--cwd", workspace, "run", "Read the large MCP report."], env=env, check=True, capture_output=True, text=True)
        assert large.stdout.strip() == "MCP_LARGE_OK", large
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
