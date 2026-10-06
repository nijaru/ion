"""Exercise opt-in composition through headless, MCP, RPC, and terminal hosts."""
import errno
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []

# The same file is the configured stdio MCP fixture, not a second agent runtime.
if len(sys.argv) > 1 and sys.argv[1] == "mcp":
    for line in sys.stdin:
        message = json.loads(line)
        if "id" not in message:
            continue
        method = message["method"]
        if method == "initialize":
            result = {"protocolVersion": message["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "lookup", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "lookup", "description": "Lookup an item count", "inputSchema": {"type": "object", "properties": {"index": {"type": "integer"}}, "required": ["index"]}}]}
        elif method == "tools/call":
            index = message["params"]["arguments"]["index"]
            result = {"content": [{"type": "text", "text": f"PRIVATE_MCP_{index}"}], "structuredContent": {"count": index}}
        else:
            result = {}
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
    raise SystemExit


def program(prompt):
    if prompt == "ACTIVE":
        return "await Promise.all(['a','b'].map(x=>tools.call('exec',{command:`touch ${x}.ready; sleep 1.5; printf done`}))); return true;"
    if prompt == "MCP":
        return "const [d]=await tools.describe('Lookup an item'); const rs=await Promise.all([1,2].map(index=>tools.call(d.name,{index}))); return {total:rs.reduce((n,r)=>n+r.value.structured_content.count,0)};"
    if prompt in ("IO", "STDERR"):
        first = "sleep 0.5; printf FIRST_DONE" if prompt == "STDERR" else "exec sleep 30"
        return "await Promise.all([" + ",".join(
            "tools.call('exec'," + json.dumps({"command": f"printf '%s\\n' $$ > {x}.pid; printf STARTED_{x}; touch {x}.ready; {end}", "timeout_ms": 120000}) + ")"
            for x, end in [("a", first), ("b", "exec sleep 30")]
        ) + "]); return true;"
    return "const rs=await Promise.all(['one','two'].map(path=>tools.call('read',{path}))); await tools.call('write',{path:'changed',content:'done'}); return {lengths:rs.map(r=>r.value.content.length)};"


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        messages = body["messages"]
        user_index = max(i for i, message in enumerate(messages) if message["role"] == "user")
        prompt = messages[user_index]["content"]
        enabled = "code_mode" in {tool["function"]["name"] for tool in body.get("tools", [])}
        assert "PRIVATE_" not in json.dumps(body), "child payload entered model context"
        if enabled and prompt in ("FANOUT", "MCP", "IO", "STDERR", "ACTIVE") and not any(message["role"] == "tool" for message in messages[user_index + 1:]):
            delta = {"tool_calls": [{"index": 0, "id": "parent", "type": "function", "function": {"name": "code_mode", "arguments": json.dumps({"code": program(prompt)})}}]}
            reason = "tool_calls"
        else:
            delta, reason = {"content": "CODE_OK" if enabled else "DIRECT_OK"}, "stop"
        events = [{"id": "code", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}, {"id": "code", "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


def run(env, workspace, *args):
    return subprocess.run([binary, "--cwd", workspace, *args], env=env, capture_output=True, text=True, timeout=15, check=True)


def read_record(child):
    assert select.select([child.stdout], [], [], 8)[0], "RPC timed out"
    return json.loads(child.stdout.readline())


def ready(workspace, child):
    deadline = time.monotonic() + 8
    while not all((workspace / f"{x}.ready").exists() for x in ("a", "b")):
        assert child.poll() is None, "child exited before native work started"
        assert time.monotonic() < deadline, "native work did not start"
        time.sleep(0.005)


def exited_pids(workspace):
    for x in ("a", "b"):
        pid = int((workspace / f"{x}.pid").read_text())
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue
        raise AssertionError(f"native PID {pid} still alive after operation returned")


with tempfile.TemporaryDirectory(prefix="ion-code-mode-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "one").write_text("PRIVATE_ONE_" * 100)
    (workspace / "two").write_text("PRIVATE_TWO_" * 100)
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "TERM": "xterm-256color"}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "code", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        assert run(env, workspace, "run", "DEFAULT").stdout.strip() == "DIRECT_OK"
        records = [json.loads(line) for line in run(env, workspace, "--code-mode", "--json", "run", "FANOUT").stdout.splitlines()]
        session_id = next(record["id"] for record in records if record["type"] == "session")
        children = [record for record in records if record["type"] == "child_tool_finished"]
        assert len(children) == 3 and all(record["state"] == "observed" and not record["is_error"] for record in children), children
        assert (workspace / "changed").read_text() == "done"
        assert run(env, workspace, "--session", session_id, "export").stdout.count("PRIVATE_") == 200
        assert run(env, workspace, "--session", session_id, "run", "RESUME").stdout.strip() == "DIRECT_OK"
        config = work / "config/ion/mcp.json"
        config.write_text(json.dumps({"servers": {"demo": {"command": sys.executable, "args": [str(Path(__file__).resolve()), "mcp"]}}}))
        records = [json.loads(line) for line in run(env, workspace, "--code-mode", "--json", "run", "MCP").stdout.splitlines()]
        parent = next(record for record in records if record["type"] == "tool_finished")
        assert parent["output"]["result"] == {"total": 3}, parent
        assert len([record for record in records if record["type"] == "child_tool_finished"]) == 2
        config.unlink()

        rpc_work = work / "rpc"
        rpc_work.mkdir()
        child = subprocess.Popen([binary, "--cwd", rpc_work, "--code-mode", "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            assert read_record(child)["type"] == "ready"
            child.stdin.write(b'{"type":"prompt","message":"IO"}\n')
            ready(rpc_work, child)
            child.stdin.close()
            child.stdin = None
            stdout, stderr = child.communicate(timeout=10)
            assert child.returncode == 0, stderr
            records = [json.loads(line) for line in stdout.splitlines()]
            assert len([record for record in records if record["type"] == "child_tool_finished" and record["output"]["cancelled"]]) == 2, records
            exited_pids(rpc_work)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)

        stderr_work = work / "stderr"
        stderr_work.mkdir()
        child = subprocess.Popen([binary, "--cwd", stderr_work, "--code-mode", "run", "STDERR"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            ready(stderr_work, child)
            child.stderr.close()
            child.wait(timeout=10)
            assert child.returncode != 0
            exited_pids(stderr_work)
            exported = run(env, stderr_work, "--continue", "export").stdout
            assert "FIRST_DONE" in exported and '"cancelled": true' in exported, exported
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)

        # Exercise a real coding turn through each physical terminal policy.
        for mode in ("inline", "fullscreen"):
            active_work = work / f"active-{mode}"
            active_work.mkdir()
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 26, 80, 0, 0))
            def attach_active():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            child = subprocess.Popen([binary, "--cwd", active_work, "--code-mode", "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_active)
            os.close(slave)
            output = bytearray()
            submitted = running = finished = False
            deadline = time.monotonic() + 12
            try:
                while time.monotonic() < deadline:
                    readable, _, _ = select.select([master], [], [], 0.05)
                    if readable:
                        try:
                            data = os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            data = b""
                        output.extend(data)
                        if b"\x1b[6n" in data:
                            os.write(master, b"\x1b[2;1R")
                    if not submitted and b"\xe2\x80\xba " in output:
                        output.clear()
                        os.write(master, b"ACTIVE\r")
                        submitted = True
                    if submitted and b"2 running" in output:
                        running = True
                    if submitted and not finished and b"CODE_OK" in output:
                        assert running, (mode, output[-3000:])
                        assert all((active_work / f"{x}.ready").exists() for x in ("a", "b"))
                        os.write(master, b"\x03")
                        finished = True
                    if child.poll() is not None:
                        break
                assert finished, (mode, output[-3000:])
                child.wait(timeout=5)
                assert child.returncode == 0
            finally:
                os.close(master)
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)

        # Qualify nested saved inspection in both physical terminal policies.
        for mode in ("inline", "fullscreen"):
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 26, 80, 0, 0))
            def attach():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            child = subprocess.Popen([binary, "--cwd", workspace, "--session", session_id, "--code-mode", "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
            os.close(slave)
            output = bytearray()
            opened = closed = quit_sent = False
            last_page = 0
            deadline = time.monotonic() + 12
            try:
                while time.monotonic() < deadline:
                    readable, _, _ = select.select([master], [], [], 0.05)
                    if readable:
                        try:
                            data = os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            data = b""
                        output.extend(data)
                        if b"\x1b[6n" in data:
                            os.write(master, b"\x1b[2;1R")
                    if not opened and b"child calls" in output and (b"Read 2" in output or b"Read one" in output):
                        output.clear()
                        os.write(master, b"\x0f")
                        opened = True
                    if opened and not closed and b"Conversation" in output and b"PRIVATE_ONE_" not in output and time.monotonic() - last_page > 0.3:
                        os.write(master, b"\x1b[5~")
                        last_page = time.monotonic()
                    if opened and not closed and b"PRIVATE_ONE_" in output and b"Child activity" in output:
                        output.clear()
                        os.write(master, b"\x1b")
                        closed = True
                    if closed and not quit_sent and b"Details closed" in output:
                        os.write(master, b"\x03")
                        quit_sent = True
                    if child.poll() is not None:
                        break
                assert opened and closed and quit_sent, (mode, opened, closed, quit_sent, output[-3000:])
                child.wait(timeout=5)
                assert child.returncode == 0 and opened and closed and quit_sent, output[-2000:]
            finally:
                os.close(master)
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
        print("Ion Code Mode: native fanout, selective context, MCP, RPC EOF, stderr fault, nested PTY inspection: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
