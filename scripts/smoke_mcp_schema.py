"""Exercise observed MCP conformance through the built client and model packets."""
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))
CHECKED = "mcp__6_schema__checked"
UNCHECKED = "mcp__6_schema__unchecked"


def verify_observation(output, status):
    assert output["is_error"] is False, output
    value = output["value"]
    assert value["is_error"] is False and value["structured_content"] == {"count": "VALUE_CANARY"}, value
    assert value["output_schema_validation"]["status"] == status, value
    assert value["raw_result_error"] is None, value
    path = Path(value["raw_result_path"])
    assert path.parent == work / "tmp" and path.stat().st_mode & 0o777 == 0o600, path
    raw = json.loads(path.read_text())
    assert raw["structuredContent"] == value["structured_content"] and raw["isError"] is False, raw
    assert raw["_meta"] == {"retained": "RAW_ONLY"} and "output_schema_validation" not in raw, raw


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.packets.append(body)
        step = len(self.server.packets)
        names = {tool["function"]["name"] for tool in body["tools"]}
        mode = self.server.mode
        if mode == "composed":
            if step == 1:
                code = f"""
const found = await searchTools('schema conformance');
if (![{json.dumps(CHECKED)}, {json.dumps(UNCHECKED)}].every(n => found.some(t => t.name === n))) throw new Error('missing schema tools');
const hint = await describeTool({json.dumps(CHECKED)});
if (hint.result_schema.schema.$defs.Count.type !== 'integer' || JSON.stringify(hint.result_schema.path) !== '["value","structured_content"]') throw new Error('raw schema hint changed');
const checked = await tools[{json.dumps(CHECKED)}]({{}});
const unchecked = await tools[{json.dumps(UNCHECKED)}]({{}});
return [checked, unchecked];
"""
                name, arguments = "code_mode", {"code": code}
            else:
                assert step == 2, body
                envelope = json.loads(body["messages"][-1]["content"])
                assert envelope["is_error"] is False, envelope
                checked, unchecked = envelope["result"]["result"]
                verify_observation(checked, "invalid")
                verify_observation(unchecked, "not_checked")
                name = None
        elif mode == "deferred" and step == 1:
            assert CHECKED not in names and UNCHECKED not in names, names
            name, arguments = "tool_search", {"query": "schema conformance"}
        else:
            offset = 1 if mode == "deferred" else 0
            assert {CHECKED, UNCHECKED} <= names, names
            if step == 1 + offset:
                description = next(tool["function"]["description"] for tool in body["tools"] if tool["function"]["name"] == CHECKED)
                assert "output_schema_validation" in description, description
                name, arguments = CHECKED, {}
            else:
                envelope = json.loads(body["messages"][-1]["content"])
                assert envelope["is_error"] is False, envelope
                verify_observation({"value": envelope["result"], "is_error": envelope["is_error"]}, "invalid" if step == 2 + offset else "not_checked")
                if step == 2 + offset:
                    name, arguments = UNCHECKED, {}
                else:
                    assert step == 3 + offset, body
                    name = None
        if name is None:
            delta, finish = {"content": "MCP_SCHEMA_OK"}, "stop"
        else:
            delta = {"tool_calls": [{"index": 0, "id": f"schema-{step}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
            finish = "tool_calls"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for change in ({"choices": [{"index": 0, "delta": delta, "finish_reason": None}]}, {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}):
            self.wfile.write(("data: " + json.dumps(change) + "\n\n").encode())
        self.wfile.write(b"data: [DONE]\n\n")

    def log_message(self, *_args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-mcp-schema-smoke-") as temporary:
    work = Path(temporary)
    for directory in ("workspace", "home", "tmp", "ion"):
        (work / directory).mkdir()
    env = {**os.environ, "HOME": str(work / "home"), "TMPDIR": str(work / "tmp"), "ION_HOME": str(work / "ion"), "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "XDG_CACHE_HOME": str(work / "cache")}
    fixture = work / "server.py"
    effects = work / "effects"
    fixture.write_text("""
import json,sys
from pathlib import Path
calls=Path(sys.argv[1])
for line in sys.stdin:
    r=json.loads(line)
    if 'id' not in r: continue
    if r['method']=='initialize':
        result={'protocolVersion':r['params']['protocolVersion'],'capabilities':{'tools':{}},'serverInfo':{'name':'schema','version':'1'}}
    elif r['method']=='tools/list':
        result={'tools':[
            {'name':'checked','description':'schema conformance checked','inputSchema':{'type':'object'},'outputSchema':{'$defs':{'Count':{'type':'integer'}},'type':'object','properties':{'count':{'$ref':'#/$defs/Count'}},'required':['count']}},
            {'name':'unchecked','description':'schema conformance unchecked','inputSchema':{'type':'object'},'outputSchema':{'$ref':sys.argv[2]}}]}
    elif r['method']=='tools/call':
        with calls.open('a') as out: out.write(r['params']['name']+'\\n')
        result={'content':[{'type':'text','text':'remote observation'}],'structuredContent':{'count':'VALUE_CANARY'},'isError':False,'_meta':{'retained':'RAW_ONLY'}}
    else: continue
    print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
""")
    with socket.socket() as external:
        external.bind(("127.0.0.1", 0))
        external.listen()
        external.setblocking(False)
        reference = f"http://127.0.0.1:{external.getsockname()[1]}/SCHEMA_CANARY"
        server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            subprocess.run([BINARY, "use", "smoke", "schema", "--endpoint", f"http://127.0.0.1:{server.server_port}/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
            for mode, packets in (("direct", 3), ("deferred", 4), ("composed", 2)):
                config = {"servers": {"schema": {"command": sys.executable, "args": [str(fixture), str(effects), reference], "exposure": "direct" if mode == "direct" else "deferred"}}}
                (work / "ion/mcp.json").write_text(json.dumps(config))
                server.mode, server.packets = mode, []
                command = [BINARY, "--cwd", work / "workspace"] + (["--code-mode"] if mode == "composed" else []) + ["run", "Check MCP schema conformance."]
                response = subprocess.run(command, env=env, check=True, capture_output=True, text=True, timeout=30)
                assert response.stdout.strip() == "MCP_SCHEMA_OK" and len(server.packets) == packets, response
                assert "unchecked" in response.stderr and "SCHEMA_CANARY" not in response.stderr, response
            assert effects.read_text().splitlines() == ["checked", "unchecked"] * 3, effects.read_text()
            try:
                external.accept()
            except BlockingIOError:
                pass
            else:
                raise AssertionError("output schema retrieved an external reference")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
    print("Ion MCP output conformance: direct/deferred/Code Mode, raw outcomes, unchecked availability and no replay/reference I/O: OK")
