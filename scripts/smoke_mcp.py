"""Exercise MCP configuration, tool/resource delivery, refresh and owned lifecycle."""

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
launch_requests = []
remote_calls = []


def check_embedded_delivery(body):
    tool, images = body["messages"][-2:]
    assert tool["role"] == "tool" and images["role"] == "user", body
    envelope = json.loads(tool["content"])
    assert envelope["is_error"] is False, envelope
    value = envelope["result"]
    content = value["content"]
    assert not value["is_error"] and "conversion_notices" not in value, value
    assert "EMBEDDED_NOTE" in content and '{"rows":2}' in content and "metadata only" in content, value
    assert "LINK_TARGET_MUST_NOT_BE_READ" not in content, content
    projected = [part["image_url"]["url"] for part in images["content"] if part["type"] == "image_url"]
    assert len(projected) == 2 and all(url.startswith("data:image/png;base64,") for url in projected), images
    assert projected[0] != projected[1], "distinct inline and embedded images were not retained"
    metadata = next(json.loads(line[line.index("{"):]) for line in content.splitlines() if '"savedPath"' in line)
    saved = Path(metadata["savedPath"])
    assert saved.parent == work / "tmp" and saved.read_bytes() == b"\x00\x01binary", metadata
    assert saved.stat().st_mode & 0o777 == 0o600 and metadata["size"] == 8, metadata


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path != "/launch/chat/completions":
            requests.append(body)
        if self.path == "/launch/chat/completions":
            changes = self.launch_changes(body)
        elif self.path == "/exposure/chat/completions":
            changes = self.exposure_changes(body)
        elif len(requests) == 1:
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
            check_embedded_delivery(body)
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
            assert names == {"read", "ls", "find", "grep", "web_fetch", "web_search", "history", "list_mcp_resources", "list_mcp_resource_templates", "read_mcp_resource"}, names
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

    def launch_changes(self, body):
        launch_requests.append(body)
        if len(launch_requests) == 1:
            names = {tool["function"]["name"] for tool in body["tools"]}
            assert all(name.startswith("mcp__6_launch__") for name in names if name.startswith("mcp__")), names
            greet = next(name for name in names if name.startswith("mcp__6_launch__greet_user_"))
            return [{"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "launch", "type": "function", "function": {"name": greet, "arguments": '{"name":"Launch"}'}}]}, "finish_reason": "tool_calls"}]}]
        assert len(launch_requests) == 2, launch_requests
        assert any("Hello, Launch!" in message.get("content", "") for message in body["messages"] if message.get("role") == "tool"), body
        return [{"choices": [{"index": 0, "delta": {"content": "MCP_LAUNCH_OK"}, "finish_reason": "stop"}]}]

    def exposure_changes(self, body):
        step = len(requests) - self.server.exposure_start
        names = {tool["function"]["name"] for tool in body["tools"]}
        demo = {name for name in names if name.startswith("mcp__4_demo__")}
        greet = next(name for name in demo if name.startswith("mcp__4_demo__greet_user_"))
        assert demo == {greet} | ({"mcp__4_demo__picture"} if step >= 3 else set()), demo
        assert not any(name.startswith("mcp__6_remote__") for name in names), names
        assert not any("mcp_resource" in name for name in names), names
        if step == 1:
            assert "mcp__8_changing__first" in names, names
            description = next(tool["function"]["description"] for tool in body["tools"] if tool["function"]["name"] == greet)
            assert description == "Greet a name\n\nMCP server demo: Visual laboratory ${UNSET_DESCRIPTION} !literal", description
            name, arguments = "mcp__4_demo__greet_user", {}
        elif step == 2:
            assert "tool was not declared for this request" in body["messages"][-1]["content"], body
            name, arguments = "tool_search", {"query": "Visual laboratory"}
        elif step == 3:
            name, arguments = "mcp__4_demo__picture", {}
        elif step == 4:
            check_embedded_delivery(body)
            name, arguments = greet, {"name": "Exposure"}
        elif step == 5:
            assert "Hello, Exposure!" in body["messages"][-1]["content"], body
            name, arguments = "mcp__8_changing__first", {}
        elif step == 6:
            assert "mcp__8_changing__first" not in names and "mcp__8_changing__second" not in names, names
            name, arguments = "tool_search", {"query": "Refreshed inventory"}
        elif step == 7:
            assert "mcp__8_changing__second" in names, names
            description = next(tool["function"]["description"] for tool in body["tools"] if tool["function"]["name"] == "mcp__8_changing__second")
            assert description == "second\n\nMCP server changing: Refreshed inventory", description
            name, arguments = "mcp__8_changing__second", {}
        elif step == 8:
            assert "SECOND" in body["messages"][-1]["content"], body
            code = f"""
const hidden = 'mcp__4_demo__greet_user';
const found = await searchTools('greet');
if (found.some(tool => tool.name === hidden) || tools[hidden] !== undefined)
    throw new Error('hidden MCP tool leaked into Code Mode');
const purpose = await searchTools('Visual laboratory');
const expected = ['mcp__4_demo__picture', {json.dumps(greet)}].sort();
if (JSON.stringify(purpose.map(tool => tool.name).sort()) !== JSON.stringify(expected))
    throw new Error('server purpose did not find only exposed tools');
const described = await describeTool('mcp__4_demo__picture');
if (described.description !== 'Return a picture\\n\\nMCP server demo: Visual laboratory ${{UNSET_DESCRIPTION}} !literal')
    throw new Error('server purpose replaced remote metadata or was expanded');
text('MCP_PURPOSE_DISCOVERY_OK');
const picture = await tools.mcp__4_demo__picture({{}});
if (picture.is_error || picture.images.length !== 2 ||
    !picture.value.content.includes('EMBEDDED_NOTE') ||
    !picture.value.content.includes('savedPath') ||
    !picture.value.content.includes('metadata only'))
    throw new Error('embedded MCP data was not delivered through Code Mode');
text('MCP_EMBEDDED_COMPOSED_OK');
text(await tools[{json.dumps(greet)}]({{name: 'Composed'}}));
"""
            name, arguments = "code_mode", {"code": code}
        else:
            assert step == 9 and all(marker in body["messages"][-1]["content"] for marker in ("Hello, Composed!", "MCP_EMBEDDED_COMPOSED_OK", "MCP_PURPOSE_DISCOVERY_OK")), body
            return [{"id": "exposure", "choices": [{"index": 0, "delta": {"content": "MCP_EXPOSURE_OK"}, "finish_reason": "stop"}]}]
        return [
            {"id": "exposure", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": f"exposure-{step}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}, "finish_reason": None}]},
            {"id": "exposure", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
        ]

    def log_message(self, *_args):
        pass


class RemoteMcp(BaseHTTPRequestHandler):
    def check_headers(self):
        assert self.headers.get_all("Authorization") == ["Bearer test-token"], self.headers
        assert self.headers.get_all("X-Api-Key") == ["header-fixture-value"], self.headers
        if self.path == "/mcp":
            assert self.headers.get("X-Tenant") == "tenant", self.headers
            assert self.headers.get("X-Empty") == "", self.headers
        self.server.header_methods.append(self.command)

    def do_POST(self):
        self.server.received_paths.append(self.path)
        if self.path in ("/redirect", "/mcp"):
            self.check_headers()
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
        if method == "initialize":
            self.send_header("Mcp-Session-Id", "header-session")
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        if self.path == "/embedded-link":
            self.server.link_fetches += 1
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"LINK_TARGET_MUST_NOT_BE_READ")
            return
        self.check_headers()
        self.send_response(405)
        self.end_headers()

    def do_DELETE(self):
        self.check_headers()
        assert self.headers.get("Mcp-Session-Id") == "header-session", self.headers
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
            result = {'content': [
                {'type': 'text', 'text': 'A tiny picture'},
                {'type': 'image', 'mimeType': 'image/png', 'data': 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=='},
                {'type': 'resource', 'resource': {'uri': 'mem://note', 'mimeType': 'text/plain', 'text': 'EMBEDDED_NOTE'}},
                {'type': 'resource', 'resource': {'uri': 'mem://data.json', 'mimeType': 'application/json', 'blob': 'eyJyb3dzIjoyfQ=='}},
                {'type': 'resource', 'resource': {'uri': 'mem://picture', 'mimeType': 'image/png', 'blob': 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='}},
                {'type': 'resource', 'resource': {'uri': 'mem://data.bin', 'mimeType': 'application/octet-stream', 'blob': 'AAFiaW5hcnk='}},
                {'type': 'resource_link', 'uri': 'LINK_TARGET', 'name': 'Unfetched link', 'description': 'Metadata only'}
            ], 'isError': False}
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
        env = {**os.environ, "ION_HOME": str(work / "ion"), "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
        subprocess.run([binary, "use", "offline", "model", "--endpoint", "http://127.0.0.1:1/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        cases = [
            ("exit", "ready", ["rpc"], None),
            ("preflight", "ready", ["--reasoning", "budget:128", "rpc"], "exact reasoning-token budgets"),
            ("input-preflight", "ready", ["run", "/probe 'unfinished"], "quote"),
            ("init-fault", "init-error", ["rpc"], "initialization failed"),
            ("listing-fault", "list-error", ["rpc"], "tool listing failed"),
            ("diagnostic-io", "ready", ["rpc"], None),
            ("preflight-io", "ready", ["--reasoning", "budget:128", "rpc"], None),
        ]
        config_path = work / "ion/mcp.json"
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
            input_database = workspace / "must-not-exist.sqlite"
            if name == "input-preflight":
                args = ["--session", str(input_database), *args]
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
                if name == "input-preflight":
                    # Complete headless input validation now precedes Session/MCP
                    # startup; there is no root to clean up for invalid input.
                    assert response.returncode == 1 and error in response.stderr, response
                    assert not input_database.exists(), "invalid input allocated a Session"
                    assert not (workspace / "owned.pid").exists(), "invalid input started MCP"
                    assert not (workspace / "owned.eof").exists(), "unexpected MCP transport"
                    continue
                pid = int((workspace / "owned.pid").read_text())
                process = subprocess.run(["ps", "-p", str(pid), "-o", "pid="], capture_output=True, text=True)
                assert not process.stderr, process
                assert not process.stdout.strip(), f"{name}: owned root {pid} survived or was not reaped"
                assert (workspace / "owned.eof").exists(), f"{name}: transport did not close stdin"
                if error:
                    assert error in response.stderr, response
                if name == "preflight":
                    assert "cannot start MCP server broken" in response.stderr, response
                expected_failure = name in ("preflight", "diagnostic-io", "preflight-io")
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
    print("Ion MCP pre-effect input rejection and owned roots on exit/preflight/startup faults: OK")


check_owned_cleanup()

with tempfile.TemporaryDirectory(prefix="ion-mcp-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    server_script = work / "server.py"
    bad_script = work / "bad-listing.py"
    bad_script.write_text(bad_listing_source)
    paired_script = work / "paired.py"
    paired_script.write_text(paired_source)
    changing_script = work / "changing.py"
    changing_script.write_text(changing_source)
    for directory in ("home", "tmp", "cache"):
        (work / directory).mkdir()
    env = {**os.environ, "HOME": str(work / "home"), "TMPDIR": str(work / "tmp"), "XDG_CACHE_HOME": str(work / "cache"), "ION_HOME": str(work / "ion"), "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "REMOTE_MCP_TOKEN": "test-token"}
    env.update(REMOTE_HEADER_KEY="header-fixture-value", REMOTE_HEADER_INJECTED="header-fixture-value\r\nInjected: yes")
    env.pop("REMOTE_HEADER_ABSENT", None)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    remote = ThreadingHTTPServer(("127.0.0.1", 0), RemoteMcp)
    remote.link_fetches = 0
    server_source = server_source.replace("LINK_TARGET", f"http://127.0.0.1:{remote.server_port}/embedded-link")
    server_script.write_text(server_source)
    remote_thread = threading.Thread(target=remote.serve_forever, daemon=True)
    remote_thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "mcp-model", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        remote.redirect_followed = False
        remote.header_methods = []
        remote.received_paths = []
        paused_script = work / "paused.py"
        paused_script.write_text(server_source.replace("'mcp-events.txt'", "'enable-events.txt'"))
        env.pop("ION_MCP_ENABLE_MISSING", None)
        subprocess.run([binary, "mcp", "add", "paused-local", "--env", "TOKEN=${ION_MCP_ENABLE_MISSING}", sys.executable, str(paused_script)], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "add-http", "paused-remote", f"http://127.0.0.1:{remote.server_port}/mcp", "--bearer-token-env", "REMOTE_MCP_TOKEN", "--header", "X-Api-Key=${REMOTE_HEADER_KEY}", "--header", "X-Tenant=tenant", "--header", "X-Empty="], env=env, check=True, capture_output=True)
        for name in ("paused-local", "paused-remote"):
            subprocess.run([binary, "mcp", "disable", name], env=env, check=True, capture_output=True)
        paused_env = {key: value for key, value in env.items() if key != "REMOTE_MCP_TOKEN"}
        disabled = subprocess.run([binary, "--cwd", workspace, "rpc"], input="", env=paused_env, check=True, capture_output=True, text=True, timeout=15)
        assert not disabled.stderr, disabled
        assert not (workspace / "enable-events.txt").exists(), "disabled stdio server ran"
        assert remote.received_paths == [], remote.received_paths
        paused_listing = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "paused-local\tdisabled\t" in paused_listing.stdout and "paused-remote\tdisabled\t" in paused_listing.stdout, paused_listing
        assert "  exposure\tdirect" in paused_listing.stdout and "${ION_MCP_ENABLE_MISSING}" not in paused_listing.stdout, paused_listing
        for name in ("paused-local", "paused-remote"):
            subprocess.run([binary, "mcp", "enable", name], env=env, check=True, capture_output=True)
        enabled = subprocess.run([binary, "--cwd", workspace, "rpc"], input="", env={**env, "ION_MCP_ENABLE_MISSING": "fixture-token"}, check=True, capture_output=True, text=True, timeout=15)
        assert not enabled.stderr, enabled
        assert (workspace / "enable-events.txt").read_text() == "started\neof\n"
        assert "/mcp" in remote.received_paths and set(remote.header_methods) == {"POST", "GET", "DELETE"}
        assert requests == [] and remote_calls == [], "configuration management dispatched model/tool work"
        for name in ("paused-local", "paused-remote"):
            subprocess.run([binary, "mcp", "remove", name], env=env, check=True, capture_output=True)
        remote.header_methods = []
        remote.received_paths = []
        subprocess.run([binary, "mcp", "add-http", "redirected", f"http://127.0.0.1:{remote.server_port}/redirect", "--header", "Authorization=Bearer ${REMOTE_MCP_TOKEN}", "--header", "X-Api-Key=${REMOTE_HEADER_KEY}"], env=env, check=True, capture_output=True)
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
        subprocess.run([binary, "mcp", "add-http", "remote", f"http://127.0.0.1:{remote.server_port}/mcp", "--description", "Remote text conversion", "--bearer-token-env", "REMOTE_MCP_TOKEN", "--header", "X-Api-Key=obsolete", "--header", "x-api-key=${REMOTE_HEADER_KEY}", "--header", "X-Tenant=${REMOTE_HEADER_ABSENT:-tenant}", "--header", "X-Empty="], env=env, check=True, capture_output=True)
        listing = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "demo" in listing.stdout and "demo\tdirect\t" in listing.stdout, listing.stdout
        assert str(server_script) in listing.stdout and "/mcp" in listing.stdout
        assert "  header\tx-api-key" in listing.stdout and "header-fixture-value" not in listing.stdout + listing.stderr and "${REMOTE_HEADER_KEY}" not in listing.stdout
        rejected = subprocess.run([binary, "mcp", "add-http", "rejected", f"http://127.0.0.1:{remote.server_port}/must-not-connect", "--header", "Host=other.test"], env=env, capture_output=True, text=True)
        assert rejected.returncode != 0 and "reserved MCP HTTP header" in rejected.stderr, rejected
        literal_purpose = "${UNSET_DESCRIPTION} !literal\n\x1b[2J"
        subprocess.run([binary, "mcp", "add", "flag-probe", "--exposure", "hidden", "--description", literal_purpose, sys.executable, "-c", "import sys"], env=env, check=True, capture_output=True)
        flagged = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "flag-probe\thidden\t" in flagged.stdout, flagged.stdout
        assert "  description\t${UNSET_DESCRIPTION} !literal  [2J\n" in flagged.stdout and "\x1b" not in flagged.stdout, flagged.stdout
        flag_config = json.loads((Path(env["ION_HOME"]) / "mcp.json").read_text())
        assert flag_config["servers"]["flag-probe"]["args"] == ["-c", "import sys"], flag_config
        assert flag_config["servers"]["flag-probe"]["description"] == literal_purpose, flag_config
        subprocess.run([binary, "mcp", "remove", "flag-probe"], env=env, check=True, capture_output=True)
        config_path = Path(env["ION_HOME"]) / "mcp.json"
        config = json.loads(config_path.read_text())
        config["servers"]["malformed"] = {"command": 7}
        for name, headers in {
            "headers-missing": {"X-Key": "${REMOTE_HEADER_ABSENT}"},
            "headers-injected": {"X-Key": "${REMOTE_HEADER_INJECTED}"},
            "headers-malformed": {"PIN": 731982654},
        }.items():
            config["servers"][name] = {"url": f"http://127.0.0.1:{remote.server_port}/must-not-connect", "headers": headers}
        assert config["servers"]["remote"]["headers"]["x-api-key"] == "${REMOTE_HEADER_KEY}"
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
        remote_descriptions = [tool["function"]["description"] for tool in requests[0]["tools"] if tool["function"]["name"].startswith("mcp__6_remote__")]
        assert remote_descriptions and all(description.endswith("\n\nMCP server remote: Remote text conversion") for description in remote_descriptions), remote_descriptions
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
        assert "header-fixture-value" not in remote_response.stdout + remote_response.stderr and "731982654" not in remote_response.stderr, remote_response
        assert all(name in remote_response.stderr for name in ["headers-missing", "headers-injected", "headers-malformed"]), remote_response
        assert "/must-not-connect" not in remote.received_paths, remote.received_paths
        assert set(remote.header_methods) == {"POST", "GET", "DELETE"}, remote.header_methods
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

        # Select exact names and ordered patterns without changing startup or
        # resources. Newly listed tools must use the same captured overrides.
        saved_config = config_path.read_text()
        exposure_config = json.loads(saved_config)
        exposure_config["servers"]["demo"].update({
            "exposure": "hidden",
            "description": "  Visual laboratory ${UNSET_DESCRIPTION} !literal  ",
            "tool_exposure": {"p*": "deferred", "*": "hidden", "greet.user": "direct"},
        })
        exposure_config["servers"]["changing"].update({
            "exposure": "hidden",
            "description": "Refreshed inventory",
            "tool_exposure": {"s*": "deferred", "*": "hidden", "first": "direct"},
        })
        exposure_config["servers"]["remote"]["tool_exposure"] = {"*case": "hidden", "*": "direct"}
        config_path.write_text(json.dumps(exposure_config))
        subprocess.run([binary, "mcp", "add", "order-probe", sys.executable, "-c", "import sys"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "order-probe"], env=env, check=True, capture_output=True)
        for action in ("disable", "enable"):
            subprocess.run([binary, "mcp", action, "demo"], env=env, check=True, capture_output=True)
            saved_demo = json.loads(config_path.read_text())["servers"]["demo"]
            assert list(saved_demo["tool_exposure"]) == ["p*", "*", "greet.user"]
            assert saved_demo.get("enabled", True) == (action == "enable")
        exposed_listing = subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "demo\thidden\t" in exposed_listing.stdout and "  tool\tp*\tdeferred\n  tool\t*\thidden\n  tool\tgreet.user\tdirect" in exposed_listing.stdout, exposed_listing
        server.exposure_start = len(requests)
        previous_remote_calls = len(remote_calls)
        previous_changed_events = (workspace / "changing-mcp-events.txt").read_text()
        subprocess.run([binary, "use", "smoke", "mcp-model", "--endpoint", f"http://127.0.0.1:{server.server_port}/exposure/chat/completions", "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        exposure = subprocess.run([binary, "--cwd", workspace, "--code-mode", "run", "Exercise the selected MCP tools."], env=env, check=True, capture_output=True, text=True, timeout=30)
        assert exposure.stdout.strip() == "MCP_EXPOSURE_OK", exposure
        assert len(requests) - server.exposure_start == 9, requests
        assert len(remote_calls) == previous_remote_calls, remote_calls
        events = (workspace / "mcp-events.txt").read_text()
        assert events.count("called:Exposure") == 1 and events.count("called:Composed") == 1 and events.count("called:picture") == 2, events
        assert "read:" not in events and remote.link_fetches == 0, (events, remote.link_fetches)
        print("Ion MCP embedded text/image/binary resources and unfetched link metadata: OK")
        print("Ion MCP literal server purpose in declarations, deferred search, Code Mode and refresh: OK")
        changed_events = (workspace / "changing-mcp-events.txt").read_text()
        for name in ("first", "second"):
            assert changed_events.count(f"called:{name}") - previous_changed_events.count(f"called:{name}") == 1, changed_events
        config_path.write_text(saved_config)

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
        changed_config["servers"]["project-demo"]["env"] = {"TRUST_PROBE": "changed"}
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
        subprocess.run([binary, "--cwd", workspace, "mcp", "disable", "--project", "project-added"], env=env, check=True, capture_output=True)
        pending = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "not trusted" in pending.stderr and "project-added" not in pending.stdout, pending
        subprocess.run([binary, "--cwd", workspace, "trust"], env=env, check=True, capture_output=True)
        disabled_project = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "project-added\tdisabled\t" in disabled_project.stdout, disabled_project
        subprocess.run([binary, "--cwd", workspace, "mcp", "enable", "--project", "project-added"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "--cwd", workspace, "trust"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "--cwd", workspace, "mcp", "remove", "--project", "project-added"], env=env, check=True, capture_output=True)
        for action in ("disable", "enable"):
            subprocess.run([binary, "--cwd", workspace, "mcp", action, "--project", "demo"], env=env, check=True, capture_output=True)
            assert json.loads(project_config.read_text())["servers"]["demo"] == {"enabled": action == "enable"}, "project policy copied launch settings"
            subprocess.run([binary, "--cwd", workspace, "trust"], env=env, check=True, capture_output=True)
            scoped = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
            assert f"demo\t{'direct' if action == 'enable' else 'disabled'}\t" in scoped.stdout, scoped
        subprocess.run([binary, "--cwd", workspace, "untrust"], env=env, check=True, capture_output=True)
        project_config.unlink()
        (workspace / ".ion/mcp.lock").unlink(missing_ok=True)
        (workspace / ".ion").rmdir()
        for name in ("malformed", "headers-missing", "headers-injected", "headers-malformed"):
            config["servers"].pop(name)
        config_path.write_text(json.dumps(config))
        subprocess.run([binary, "mcp", "remove", "demo"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "broken"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "bad-listing"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "paired-left"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "paired-right"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "changing"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "mcp", "remove", "remote"], env=env, check=True, capture_output=True)
        assert subprocess.run([binary, "mcp", "list"], env=env, check=True, capture_output=True, text=True).stdout == ""
        print("Ion MCP persisted enablement, zero disabled startup I/O, re-enable and project trust: OK")
        print("Ion MCP discovery, tool call and child shutdown: OK")
        print("Ion MCP exact-first ordered patterns, persisted order and refreshed selection: OK")
        print("Ion MCP captured headers across POST/GET/DELETE, pre-network rejection and no credential redirects: OK")

        launch_directory = workspace / "server data"
        launch_directory.mkdir()
        launch_script = work / "launch.py"
        launch_script.write_text(
            "import os,json\nfrom pathlib import Path\n"
            "Path('launch-observed.json').write_text(json.dumps({'cwd':os.getcwd(),"
            "'env':{key:os.environ.get(key) for key in ['INHERITED','OVERRIDE','COPY','EMPTY','TOKEN','DEFAULT','COMMAND']}}))\n"
            + server_source
        )
        env.update(INHERITED="parent-value", OVERRIDE="parent-override", ION_MCP_LAUNCH_TOKEN="sensitive-fixture-value")
        env.pop("ION_MCP_MISSING", None)
        subprocess.run([
            binary, "mcp", "add", "launch", "--server-cwd", "server data",
            "--env", "OVERRIDE=old", "--env", "OVERRIDE=new", "--env", "COPY=${INHERITED}",
            "--env", "EMPTY=", "--env", "TOKEN=${ION_MCP_LAUNCH_TOKEN}",
            "--env", "DEFAULT=${ION_MCP_MISSING:-fallback}", "--env", "COMMAND=!printf not-executed",
            sys.executable, str(launch_script),
        ], env=env, check=True, capture_output=True)
        launch_config = json.loads(config_path.read_text())
        assert launch_config["servers"]["launch"]["cwd"] == "server data"
        assert launch_config["servers"]["launch"]["env"]["TOKEN"] == "${ION_MCP_LAUNCH_TOKEN}"
        launch_config["servers"]["missing-env"] = {
            "command": sys.executable, "args": [str(launch_script)],
            "env": {"TOKEN": "${ION_MCP_MISSING}"},
        }
        launch_config["servers"]["missing-dir"] = {
            "command": sys.executable, "args": [str(launch_script)], "cwd": "absent-directory",
            "env": {"TOKEN": "sensitive-fixture-value"},
        }
        launch_config["servers"]["bad-env-shape"] = {
            "command": sys.executable, "env": {"PIN": 731982654},
        }
        config_path.write_text(json.dumps(launch_config))
        listed = subprocess.run([binary, "--cwd", workspace, "mcp", "list"], env=env, check=True, capture_output=True, text=True)
        assert "  cwd\tserver data" in listed.stdout and "  env\tTOKEN" in listed.stdout, listed
        assert all(value not in listed.stdout + listed.stderr for value in ["sensitive-fixture-value", "731982654", "${ION_MCP_LAUNCH_TOKEN}"]), listed
        assert "bad-env-shape" in listed.stderr, listed
        subprocess.run([binary, "use", "smoke", "mcp-model", "--endpoint", f"http://127.0.0.1:{server.server_port}/launch/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        launched = subprocess.run([binary, "--cwd", workspace, "run", "Check local server launch."], env=env, check=True, capture_output=True, text=True, timeout=30)
        assert launched.stdout.strip() == "MCP_LAUNCH_OK" and len(launch_requests) == 2, launched
        assert "missing-env" in launched.stderr and "ION_MCP_MISSING" in launched.stderr and "missing-dir" in launched.stderr, launched
        assert all(value not in launched.stdout + launched.stderr for value in ["sensitive-fixture-value", "731982654"]), launched
        observed = json.loads((launch_directory / "launch-observed.json").read_text())
        assert Path(observed["cwd"]).samefile(launch_directory), observed
        assert observed["env"] == {
            "INHERITED": "parent-value", "OVERRIDE": "new", "COPY": "parent-value", "EMPTY": "",
            "TOKEN": "sensitive-fixture-value", "DEFAULT": "fallback", "COMMAND": "!printf not-executed",
        }, observed
        assert (launch_directory / "mcp-events.txt").read_text() == "started\ncalled:Launch\neof\n"
        assert not (workspace / "launch-observed.json").exists(), "failed launch ran a process"
        print("Ion MCP cwd, inherited/overridden environment, secret-safe diagnostics and pre-spawn failures: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        remote.shutdown()
        remote.server_close()
        remote_thread.join(timeout=5)
