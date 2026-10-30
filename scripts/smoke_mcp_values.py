"""Real-client MCP argv helpers: custody, capture, trust and secret-safe failure."""
import json
import os
import signal
import time
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

binary = Path(os.environ.get("ION_SMOKE_BIN", Path(__file__).resolve().parent.parent / "target/debug/ion"))
provider_packets = []
remote_methods = []
credential = "HEADER_CANARY_${NO_EXPANSION} !literal"


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        provider_packets.append(body)
        assert "CANARY" not in json.dumps(body), body
        names = {t["function"]["name"] for t in body.get("tools", [])}
        wanted = {"mcp__5_local__echo", "mcp__6_remote__echo"}
        if wanted <= names and not any(m["role"] == "tool" for m in body["messages"]):
            delta = {"tool_calls": [{"index": i, "id": f"call-{i}", "type": "function", "function": {"name": name, "arguments": "{}"}} for i, name in enumerate(sorted(wanted))]}
            reason = "tool_calls"
        else:
            if wanted <= names:
                replies = [json.loads(m["content"]) for m in body["messages"] if m["role"] == "tool"]
                assert len(replies) == 2 and all(not r["is_error"] for r in replies), replies
                assert {r["result"]["content"].strip() for r in replies} == {"LOCAL_OK", "REMOTE_OK"}, replies
            delta, reason = {"content": "READY"}, "stop"
        payload = b"data: " + json.dumps({"choices": [{"index": 0, "delta": delta, "finish_reason": reason}]}).encode() + b"\n\ndata: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_):
        pass


class Remote(BaseHTTPRequestHandler):
    def check(self, method):
        assert self.headers.get_all("X-Key") == [credential], self.headers
        remote_methods.append(method)

    def do_POST(self):
        self.check("POST")
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if "id" not in body:
            self.send_response(202)
            self.end_headers()
            return
        if body["method"] == "initialize":
            # Every later request must use the captured value, not reread the helper.
            (workspace / "header-secret").write_text("CHANGED_AFTER_CAPTURE")
            result = {"protocolVersion": body["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "helper", "version": "1"}}
        elif body["method"] == "tools/list":
            result = {"tools": [{"name": "echo", "description": "echo", "inputSchema": {"type": "object"}}]}
        else:
            assert body["method"] == "tools/call", body
            (workspace / "remote-effect").write_text("called")
            result = {"content": [{"type": "text", "text": "REMOTE_OK"}], "isError": False}
        payload = json.dumps({"jsonrpc": "2.0", "id": body["id"], "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Mcp-Session-Id", "helper-session")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.check("GET")
        self.send_response(405)
        self.end_headers()

    def do_DELETE(self):
        self.check("DELETE")
        self.send_response(200)
        self.end_headers()

    def log_message(self, *_):
        pass


with tempfile.TemporaryDirectory(prefix="ion-mcp-values-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    for path in [workspace / ".git", workspace / "server", work / "home", work / "ion", work / "tmp"]:
        path.mkdir(parents=True)
    env = {**os.environ, "HOME": str(work / "home"), "ION_HOME": str(work / "ion"), "TMPDIR": str(work / "tmp") + "/", "MCP_PARENT": "parent"}
    for key in ["CONFIG", "CACHE", "DATA", "STATE"]:
        env[f"XDG_{key}_HOME"] = str(work / key.lower())
    env.pop("MCP_ABSENT", None)
    config_path = work / "ion/mcp.json"
    counter = workspace / "helpers"
    (workspace / "helper.py").write_text("import os,sys\nfrom pathlib import Path\nassert os.environ['MCP_PARENT']=='parent'\nassert sys.argv[3]=='${LITERAL} !literal'\nwith open('helpers','a') as f:f.write(sys.argv[1]+'\\n')\nsys.stderr.write('STDERR_CANARY')\nprint(Path(sys.argv[2]).read_text())\n")
    (workspace / "env-secret").write_text("ENV_CANARY_${NO_EXPANSION} !literal")
    (workspace / "header-secret").write_text(credential)
    source = work / "server.py"
    source.write_text("""import json,os,sys
from pathlib import Path
Path('observed').write_text(json.dumps({'cwd':os.getcwd(),'token':os.environ['TOKEN'],'parent':os.environ['MCP_PARENT'],'literal':os.environ['LITERAL']}))
for line in sys.stdin:
 request=json.loads(line)
 if 'id' not in request:continue
 method=request['method']
 if method=='initialize':result={'protocolVersion':request['params']['protocolVersion'],'capabilities':{'tools':{}},'serverInfo':{'name':'helper','version':'1'}}
 elif method=='tools/list':result={'tools':[{'name':'echo','description':'echo','inputSchema':{'type':'object'}}]}
 else:
  assert method=='tools/call'
  Path('local-effect').write_text('called')
  result={'content':[{'type':'text','text':'LOCAL_OK'}],'isError':False}
 print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
""")
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    remote = ThreadingHTTPServer(("127.0.0.1", 0), Remote)
    threads = [threading.Thread(target=s.serve_forever, daemon=True) for s in [provider, remote]]
    for thread in threads:
        thread.start()

    def run(*args):
        scope = [] if args[0] == "use" else ["--cwd", workspace]
        result = subprocess.run([binary, *scope, *args], env=env, capture_output=True, text=True, timeout=30)
        assert result.returncode == 0, result
        assert "CANARY" not in result.stdout + result.stderr, result
        return result

    def save(servers):
        config_path.write_text(json.dumps({"servers": servers}))

    def helper(name, file):
        return {"command": sys.executable, "args": ["helper.py", name, file, "${LITERAL} !literal"]}

    local = {"command": sys.executable, "args": [str(source)], "cwd": "server", "env": {"TOKEN": helper("env", "env-secret"), "MCP_PARENT": "override", "LITERAL": "!printf not-executed"}}
    http = {"url": f"http://127.0.0.1:{remote.server_port}/mcp", "headers": {"X-Key": helper("header", "header-secret")}}
    try:
        run("use", "fixture", "fixture-model", "--endpoint", f"http://127.0.0.1:{provider.server_port}/chat/completions", "--wire", "chat-completions")
        save({"local": local, "remote": http})
        run("mcp", "list")
        run("doctor")
        assert not counter.exists() and not remote_methods and not provider_packets
        save({"local": {**local, "enabled": False}, "remote": {**http, "enabled": False}})
        run("run", "Disabled servers must stay inert.")
        assert not counter.exists() and not remote_methods
        for headers in [
            {"Host": helper("forbidden", "header-secret")},
            {"A-Key": helper("forbidden", "header-secret"), "Z-Key": "${MCP_ABSENT}"},
            {"X-Key": {"command": sys.executable, "args": [731982654]}},
            {"X-Key": {"command": sys.executable, "args": ["-c", "import sys;print('OUTPUT_CANARY');sys.stderr.write('STDERR_CANARY');sys.exit(7)"]}},
        ]:
            save({"invalid": {**http, "headers": headers}})
            failed = run("run", "Fail closed before transport admission.")
            assert "invalid" in failed.stderr and "731982654" not in failed.stderr, failed
            assert not counter.exists() and not remote_methods
        save({"local": local, "remote": http})
        before = len(provider_packets)
        assert run("run", "Call both MCP echo tools.").stdout.strip() == "READY"
        assert len(provider_packets) == before + 2
        assert sorted(counter.read_text().splitlines()) == ["env", "header"]
        assert {"POST", "GET", "DELETE"} <= set(remote_methods), remote_methods
        assert (workspace / "remote-effect").read_text() == "called"
        assert (workspace / "server/local-effect").read_text() == "called"
        assert json.loads((workspace / "server/observed").read_text()) == {"cwd": str((workspace / "server").resolve()), "token": "ENV_CANARY_${NO_EXPANSION} !literal", "parent": "override", "literal": "!printf not-executed"}
        save({})
        project = workspace / ".ion/mcp.json"
        project.parent.mkdir()
        project.write_text(json.dumps({"servers": {"trusted": local}}))
        run("run", "Untrusted helpers must stay inert.")
        assert len(counter.read_text().splitlines()) == 2
        run("trust")
        assert len(counter.read_text().splitlines()) == 2, "trust validation executed a helper"
        run("run", "Explicitly trusted helper may start.")
        assert counter.read_text().splitlines().count("env") == 2
        project.write_text(project.read_text() + "\n")
        run("run", "Changed digest must revoke future helper admission.")
        assert len(counter.read_text().splitlines()) == 3
        project.unlink()
        sleeper = workspace / "interrupt-helper.py"
        sleeper.write_text("import os,sys,time\nfrom pathlib import Path\npath=Path(sys.argv[1]);tmp=path.with_suffix('.tmp')\ntmp.write_text(str(os.getpid()));tmp.replace(path)\ntime.sleep(60)\n")
        for client in [["run", "Never admitted."], ["rpc"], ["chat"]]:
            marker = workspace / ("pid-" + client[0])
            save({"interrupted": {"command": sys.executable, "args": [str(source)], "env": {
                "TOKEN": {"command": sys.executable, "args": [str(sleeper), str(marker)]}
            }}})
            before = len(provider_packets)
            child = subprocess.Popen([binary, "--cwd", workspace, *client], env=env,
                                     stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     start_new_session=True)
            pid = None
            try:
                deadline = time.monotonic() + 5
                while not marker.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                assert marker.exists(), f"startup helper was not admitted: {client}, Ion status {child.poll()}"
                pid = int(marker.read_text())
                os.killpg(child.pid, signal.SIGINT)
                stdout, stderr = child.communicate(timeout=5)
                assert child.returncode == 1, (client, child.returncode, stdout, stderr)
                assert b"MCP startup interrupted" in stderr, stderr
                assert len(provider_packets) == before, "interrupted preparation admitted a model request"
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    pass
                else:
                    raise AssertionError("SIGINT returned before reaping the helper")
            finally:
                if child.poll() is None:
                    child.kill()
                    child.communicate(timeout=5)
                if pid is not None:
                    # Failure cleanup may signal only this still-owned helper group.
                    observed = subprocess.run(["ps", "-p", str(pid), "-o", "pgid=,args="],
                                              capture_output=True, text=True)
                    if observed.returncode == 0 and str(sleeper) in observed.stdout:
                        assert int(observed.stdout.split()[0]) == pid, observed.stdout
                        os.killpg(pid, signal.SIGKILL)
        print("Ion MCP argv helpers: startup SIGINT joins preparation before headless/RPC/terminal admission: OK")
        print("Ion MCP argv helpers: literal data, captured headers/env, offline readers, trust, secret-safe failure and zero replay: OK")
    finally:
        for server in [provider, remote]:
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=5)
