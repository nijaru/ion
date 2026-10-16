"""Exercise coding delegation defaults and ownership without provider credentials."""
import json
import os
import select
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))

if len(sys.argv) > 1 and sys.argv[1] == "mcp":
    for line in sys.stdin:
        message = json.loads(line)
        if "id" not in message:
            continue
        method = message["method"]
        if method == "initialize":
            result = {"protocolVersion": message["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "marker", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "marker", "description": "Fixture marker", "inputSchema": {"type": "object", "properties": {}}}]}
        elif method == "tools/call":
            result = {"content": [{"type": "text", "text": "PRIVATE_CHILD_TOOL"}]}
        else:
            result = {}
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
    raise SystemExit


def facts(path):
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
        header = json.loads(db.execute("SELECT header FROM session WHERE id=1").fetchone()[0])
        entries = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
    return header, entries


def command(env, workspace, *args):
    return subprocess.run([binary, "--cwd", workspace, *args], env=env, check=True, capture_output=True, text=True, timeout=20)


def configure(env, endpoint):
    return subprocess.run([binary, "use", "smoke", "coding", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True, text=True, timeout=10)


def call(name, arguments):
    return {"tool_calls": [{"index": 0, "id": name, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}


with tempfile.TemporaryDirectory(prefix="ion-coding-subagent-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "AGENTS.md").write_text("SUBAGENT_RESOURCE: follow this workspace's instructions.\n")
    env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
    env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
    Path(env["HOME"]).mkdir()
    requests = []
    failures = []
    changed_route = set()

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(body)
                user = next(message["content"] for message in reversed(body["messages"]) if message["role"] == "user")
                tools = {tool["function"]["name"] for tool in body.get("tools", [])}
                fresh = body["messages"][-1]["role"] == "user"
                assert body["model"] == "coding"
                assert "SUBAGENT_RESOURCE" in json.dumps(body)
                expected_effort = {"PARENT_DIRECT":"high", "CHILD_DIRECT":"high", "PARENT_COMPOSED":"low", "CHILD_COMPOSED":"low", "PARENT_CANCEL":"medium", "CHILD_CANCEL":"medium"}.get(user)
                if expected_effort:
                    assert body.get("reasoning_effort") == expected_effort, body
                if user == "PARENT_DEFAULT":
                    assert "subagent" in tools
                    delta = {"content": "DEFAULT_OK"}
                elif user.startswith("PARENT_"):
                    assert "subagent" in tools
                    if fresh:
                        # Freeze the invocation-local route, not just its model ID.
                        # A child consulting the now-changed store would hit port 1.
                        configure(env, "http://127.0.0.1:1/v1/chat/completions")
                        changed_route.add(user)
                        task = "CHILD_" + user.removeprefix("PARENT_")
                        delta = call("code_mode", {"code": "const r=await tools.subagent(" + json.dumps({"task": task}) + "); return r.value;"}) if user == "PARENT_COMPOSED" else call("subagent", {"task": task})
                    else:
                        assert "PRIVATE_CHILD_TOOL" not in json.dumps(body)
                        assert "CHILD_ANSWER" in json.dumps(body)
                        delta = {"content": "PARENT_OK"}
                else:
                    assert user.startswith("CHILD_")
                    assert "PARENT_" not in json.dumps(body), "parent conversation leaked into fresh child"
                    assert "subagent" not in tools, "built-in delegation recursively enabled"
                    assert ("code_mode" in tools) == (user == "CHILD_COMPOSED")
                    if fresh:
                        if user == "CHILD_CANCEL":
                            delta = call("exec", {"command": "printf '%s' $$ > child.pid; touch child.ready; exec sleep 30", "timeout_ms": 120000})
                        else:
                            marker = next(name for name in tools if name.startswith("mcp__") and name.endswith("marker"))
                            if user == "CHILD_COMPOSED":
                                # Both parent and child VMs stay owned: the parent
                                # awaits delegation while the child composes tools.
                                code = "const m=await tools[" + json.dumps(marker) + "]({}); await tools.write({path:'CHILD_COMPOSED.txt',content:'CHILD_EFFECT'}); return {marker:m.value.content};"
                                delta = call("code_mode", {"code": code})
                            else:
                                first = call(marker, {})["tool_calls"][0]
                                second = call("write", {"path": user + ".txt", "content": "CHILD_EFFECT"})["tool_calls"][0]
                                second["index"] = 1
                                delta = {"tool_calls": [first, second]}
                    else:
                        assert "PRIVATE_CHILD_TOOL" in json.dumps(body)
                        delta = {"content": "CHILD_ANSWER_" + user}
                reason = "tool_calls" if "tool_calls" in delta else "stop"
                events = [{"id": "fixture", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}, {"id": "fixture", "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}]
                payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except Exception as error:
                failures.append(repr(error))
                self.send_error(500, str(error))

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
    config = Path(env["XDG_CONFIG_HOME"]) / "ion/mcp.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({"servers": {"fixture": {"command": sys.executable, "args": [str(Path(__file__).resolve()), "mcp"]}}}))
    child = None
    pid = None
    passed = False
    try:
        configure(env, endpoint)
        assert command(env, workspace, "run", "PARENT_DEFAULT").stdout.strip() == "DEFAULT_OK"
        for composed in (False, True):
            configure(env, endpoint)
            before = set(Path(env["XDG_STATE_HOME"]).rglob("*.sqlite"))
            prompt = "PARENT_COMPOSED" if composed else "PARENT_DIRECT"
            records = [json.loads(line) for line in command(env, workspace, "--reasoning", "low" if composed else "high", *(["--code-mode"] if composed else []), "--json", "run", prompt).stdout.splitlines()]
            result = next(record["output"] for record in records if record["type"] == "tool_finished")
            if composed:
                result = result["result"]
            assert result["answer"] == "CHILD_ANSWER_CHILD_" + ("COMPOSED" if composed else "DIRECT"), result
            assert result["outcome"]["kind"] == "joined" and result["outcome"]["error"] is None, result
            assert result["outcome"]["receipt"]["end"] == "completed", result
            child_path = Path(result["path"])
            allocated = set(Path(env["XDG_STATE_HOME"]).rglob("*.sqlite")) - before
            assert len(allocated) == 2 and child_path in allocated, allocated
            parent_path = (allocated - {child_path}).pop()
            # Managed work must not hijack automatic continuation, including
            # when its file is newer than the parent's (as after a process cut).
            future = time.time_ns() + 5_000_000_000
            os.utime(child_path, ns=(future, future))
            count = len(requests)
            continued = command(env, workspace, "--continue", "export")
            assert prompt in continued.stdout, "automatic continuation selected a managed child"
            assert len(requests) == count, "passive continuation export dispatched work"
            parent_header, entries = facts(parent_path)
            child_header, child_entries = facts(child_path)
            admitted = next(entry["data"]["record"] for entry in entries if entry["kind"] == "delegation_admitted")
            assert admitted["child"] == child_header["id"] == result["child_session"]
            assert admitted["site"]["session"] == parent_header["id"]
            assert admitted["ownership"] == "foreground" and admitted["spec"]["context"] == "fresh"
            assert admitted["spec"]["reasoning"] == ("Low" if composed else "High")
            assert child_entries[0] == {"kind": "child_session_of", "data": {"record": admitted}}
            assert Path(child_header["cwd"]).samefile(workspace), (child_header["cwd"], workspace)
            assert child_path.parent == parent_path.parent / "children"
            assert not any(entry["kind"] == "delegation_transferred" for entry in entries)
            admission = next(i for i, entry in enumerate(entries) if entry["kind"] == "delegation_admitted")
            settlement = next(i for i, entry in enumerate(entries) if entry["kind"] == "delegation_settled")
            outer = next(i for i, entry in enumerate(entries) if entry["kind"] == ("child_tool_result" if composed else "tool_result"))
            assert admission < settlement < outer
            assert (workspace / ("CHILD_COMPOSED.txt" if composed else "CHILD_DIRECT.txt")).read_text() == "CHILD_EFFECT"
            assert prompt in changed_route

        # EOF requests parent stop, and foreground ownership joins the actual
        # started native child process before the client can return.
        configure(env, endpoint)
        before = set(Path(env["XDG_STATE_HOME"]).rglob("*.sqlite"))
        child = subprocess.Popen([binary, "--cwd", workspace, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        assert select.select([child.stdout], [], [], 8)[0], "RPC startup timed out"
        assert json.loads(child.stdout.readline())["type"] == "ready"
        # A preference changed after application assembly must reach the child,
        # not just an initial CLI value captured by a recipe factory.
        child.stdin.write(json.dumps({"type":"set_reasoning", "id":"effort", "effort":"medium"}) + "\n")
        child.stdin.flush()
        assert select.select([child.stdout], [], [], 8)[0], "reasoning change timed out"
        acknowledgement = json.loads(child.stdout.readline())
        assert acknowledgement.get("id") == "effort" and acknowledgement.get("success"), acknowledgement
        child.stdin.write(json.dumps({"type": "prompt", "id": "root", "message": "PARENT_CANCEL"}) + "\n")
        child.stdin.flush()
        deadline = time.monotonic() + 10
        while not (workspace / "child.ready").exists():
            assert child.poll() is None, child.stderr.read() if child.poll() is not None else ""
            assert time.monotonic() < deadline, "child native effect did not start"
            time.sleep(0.01)
        pid = int((workspace / "child.pid").read_text())
        child.stdin.close()
        child.stdin = None
        output, error = child.communicate(timeout=10)
        assert child.returncode == 0, (output, error)
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("native child still alive after foreground EOF shutdown")
        allocated = set(Path(env["XDG_STATE_HOME"]).rglob("*.sqlite")) - before
        assert len(allocated) == 2, allocated
        histories = [(path, *facts(path)) for path in allocated]
        parent_entries = next(entries for _, _, entries in histories if any(entry["kind"] == "delegation_admitted" for entry in entries))
        settlement = next(entry for entry in parent_entries if entry["kind"] == "delegation_settled")
        assert settlement["data"]["outcome"]["kind"] == "joined"
        assert settlement["data"]["outcome"]["error"] is not None
        assert not failures, failures
        passed = True
        print("coding subagent: direct/composed recipes, frozen route, root continuation, actual effects and EOF joined cancellation passed")
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.communicate(timeout=5)
        if not passed and pid is not None:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
