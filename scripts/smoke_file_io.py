"""Exercise line-ranged native reads and special-file rejection through the built CLI."""

import hashlib
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
fixture = "fifo"
failures = []


def scoped_response(body):
    step = len(requests) - 1
    results = [json.loads(message["content"]) for message in body["messages"] if message["role"] == "tool"]
    target = "sub/new/deeper/output"
    path = workspace / target
    rule = workspace / "sub/AGENTS.md"

    def call(name, arguments):
        return {"index": 0, "id": f"scope-{step}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}

    def write(path=target):
        return call("write", {"path": path, "content": "before-edit"})

    def invoke(tool):
        return {"tool_calls": [tool]}, "tool_calls"

    def code(program):
        return invoke(call("code_mode", {"code": program}))

    if fixture == "scoped":
        if step == 0:
            read = call("read", {"path": "sub/input"})
            read["id"] = "scope-read"
            mutation = write()
            mutation["index"] = 1
            return {"tool_calls": [read, mutation]}, "tool_calls"
        if step in (1, 2):
            # A sibling read is not delivery to the already-issued write. A
            # changed rule must also defer before even creating parent dirs.
            assert not (workspace / "sub/new").exists(), "deferred write created parents before instruction review"
            assert results[-1]["is_error"], results[-1]
            text = results[-1]["result"]["project_instructions"]["text"]
            assert ("SCOPED_A" if step == 1 else "SCOPED_B") in text, text
            assert "ROOT_ONE" not in text and "GLOBAL_ONE" not in text, text
            system = body["messages"][0]["content"]
            assert "GLOBAL_ONE" in system and "ROOT_ONE" in system, system
            if step == 1:
                rule.write_text("SCOPED_B\n")
            return invoke(write())
        if step in (3, 4):
            assert path.read_text() == "before-edit", "deferred edit changed the file"
            if step == 3:
                assert not results[-1]["is_error"], results[-1]
                rule.unlink()
            else:
                assert results[-1]["is_error"], results[-1]
                assert results[-1]["result"]["project_instructions"]["text"] == "", results[-1]
            return invoke(call("edit", {"path": target, "base_digest": hashlib.sha256(b"before-edit").hexdigest(), "edits": [{"old_text": "before-edit", "new_text": "after-edit"}]}))
        assert step == 5 and path.read_text() == "after-edit", (step, results)
        return {"content": "SCOPED_WRITES_COMPLETE"}, "stop"

    if fixture == "scoped_reopen":
        target = "sub/new/deeper/reopened"
        if step == 0:
            system = body["messages"][0]["content"]
            assert "GLOBAL_TWO" in system and "ROOT_TWO" in system, system
            return invoke(write(target))
        if step == 1:
            assert not (workspace / target).exists(), "reopen reused a stale instruction snapshot"
            assert results[-1]["is_error"] and "SCOPED_C" in results[-1]["result"]["project_instructions"]["text"], results[-1]
            return invoke(write(target))
        assert step == 2 and (workspace / target).read_text() == "before-edit", (step, results)
        return {"content": "SCOPED_REOPEN_COMPLETE"}, "stop"

    assert fixture == "scoped_code", fixture
    target = "sub/guest/deeper/output"
    write_program = "tools.write(" + json.dumps({"path": target, "content": "guest-write"}) + ")"
    if step == 0:
        return code("const read=await tools.read({path:'sub/input'}); const write=await " + write_program + "; return {read_error:read.is_error,write_error:write.is_error};")
    if step in (1, 2):
        assert not (workspace / "sub/guest").exists(), "guest consumed rules without delivering them to the model"
        if step == 1:
            assert results[-1]["result"]["result"] == {"read_error": False, "write_error": True}, results[-1]
            assert "SCOPED_C" not in json.dumps(body), "hidden child instructions entered the model request"
        else:
            assert "SCOPED_C" in results[-1]["result"]["result"]["text"], results[-1]
        if step == 1:
            return code("const r=await " + write_program + "; return r.value.project_instructions;")
        return code("return await " + write_program + ";")
    assert step == 3 and (workspace / target).read_text() == "guest-write", (step, results)
    return {"content": "SCOPED_CODE_COMPLETE"}, "stop"


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        results = [message for message in body["messages"] if message["role"] == "tool"]
        if fixture.startswith("scoped"):
            try:
                delta, reason = scoped_response(body)
            except AssertionError as failure:
                failures.append(str(failure))
                delta, reason = {"content": "SCOPE_CONTRACT_FAILED"}, "stop"
        elif fixture == "fifo":
            if results:
                assert len(results) == 1 and "regular file" in results[0]["content"], results
                delta = {"content": "FIFO_REJECTED"}
                reason = "stop"
            else:
                args = {"path": "pipe"}
                delta = {"tool_calls": [{"index": 0, "id": "fifo", "type": "function", "function": {"name": "read", "arguments": json.dumps(args)}}]}
                reason = "tool_calls"
        elif fixture == "instructions":
            assert not results, results
            system = "\n".join(message["content"] for message in body["messages"] if message["role"] == "system")
            assert "Project instruction sources omitted" in system, system
            assert str((workspace / "AGENTS.md").resolve()) in system, system
            assert "regular file" in system, system
            delta = {"content": "INSTRUCTION_OMISSION_REPORTED"}
            reason = "stop"
        else:
            definitions = {tool["function"]["name"]: tool["function"] for tool in body["tools"]}
            properties = definitions["read"]["parameters"]["properties"]
            assert properties["offset"]["minimum"] == 1 and properties["limit"]["maximum"] == 2000
            if results:
                envelope = json.loads(results[-1]["content"])
                assert envelope["is_error"] is False, envelope
                page = envelope["result"]
                assert page["start_line"] == len(results) + 1, page
                assert page["next_line"] == len(results) + 2, page
                assert page["content"] == ("β🦀\n" if len(results) == 1 else "third"), page
                assert page["has_more"] == (len(results) == 1), page
                assert page["base_digest"] == hashlib.sha256(sample).hexdigest(), page
                assert "offset" not in page and "next_offset" not in page, page
            if len(results) == 2:
                delta = {"content": "LINE_RANGES_READ"}
                reason = "stop"
            else:
                args = {"path": "sample.txt", "offset": 2 if not results else page["next_line"], "limit": 1}
                delta = {"tool_calls": [{"index": 0, "id": f"line-{len(results)}", "type": "function", "function": {"name": "read", "arguments": json.dumps(args)}}]}
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
    env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
    env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), XDG_DATA_HOME=str(work / "data"), XDG_CACHE_HOME=str(work / "cache"))
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
        fixture = "lines"
        sample = "\ufefffirst\r\nβ🦀\nthird".encode()
        (workspace / "sample.txt").write_bytes(sample)
        result = subprocess.run([binary, "--cwd", workspace, "run", "Read the second line and continue to EOF."], env=env, capture_output=True, text=True, timeout=10)
        assert result.returncode == 0 and result.stdout.strip() == "LINE_RANGES_READ", result
        assert len(requests) == 3, len(requests)
        requests.clear()
        os.mkfifo(workspace / "AGENTS.md")
        fixture = "instructions"
        result = subprocess.run([binary, "--cwd", workspace, "run", "Report instruction availability."], env=env, capture_output=True, text=True, timeout=5)
        assert result.returncode == 0 and result.stdout.strip() == "INSTRUCTION_OMISSION_REPORTED", result
        assert "instruction file omitted" in result.stderr and "regular file" in result.stderr, result
        assert len(requests) == 1, len(requests)
        (workspace / "AGENTS.md").unlink()
        (workspace / "AGENTS.md").write_text("ROOT_ONE\n")
        global_rule = work / "ion/AGENTS.md"
        global_rule.write_text("GLOBAL_ONE\n")
        (workspace / "sub").mkdir()
        (workspace / "sub/input").write_text("original source\n")
        (workspace / "sub/AGENTS.md").write_text("SCOPED_A\n")
        requests.clear()
        fixture = "scoped"
        result = subprocess.run([binary, "--cwd", workspace, "run", "Exercise scoped instruction changes."], env=env, capture_output=True, text=True, timeout=15)
        assert not failures, failures
        assert result.returncode == 0 and result.stdout.strip() == "SCOPED_WRITES_COMPLETE", result
        assert len(requests) == 6, len(requests)

        global_rule.write_text("GLOBAL_TWO\n")
        (workspace / "AGENTS.md").write_text("ROOT_TWO\n")
        (workspace / "sub/AGENTS.md").write_text("SCOPED_C\n")
        requests.clear()
        fixture = "scoped_reopen"
        result = subprocess.run([binary, "--cwd", workspace, "--continue", "run", "Exercise instruction changes after reopen."], env=env, capture_output=True, text=True, timeout=15)
        assert not failures, failures
        assert result.returncode == 0 and result.stdout.strip() == "SCOPED_REOPEN_COMPLETE", result

        requests.clear()
        fixture = "scoped_code"
        result = subprocess.run([binary, "--cwd", workspace, "--code-mode", "run", "Exercise selected instruction delivery in Code Mode."], env=env, capture_output=True, text=True, timeout=15)
        assert not failures, failures
        assert result.returncode == 0 and result.stdout.strip() == "SCOPED_CODE_COMPLETE", result
        print("Ion file reads, instruction omissions/global scope, deferred mutations, refresh/reopen and Code Mode delivery: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
