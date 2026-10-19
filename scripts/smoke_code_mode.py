"""Exercise opt-in composition through headless, MCP, RPC, and terminal hosts."""
import base64
import errno
import fcntl
import hashlib
import json
import os
import pty
import select
import shlex
import signal
import sqlite3
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
    if prompt == "RECOVER_FAIL":
        producer = "import secrets; print(secrets.token_hex(24)); print('X'*100000)"
        command = f"printf ONCE >> effects; {shlex.quote(sys.executable)} -c {shlex.quote(producer)}; exit 7"
        return "await tools.exec(" + json.dumps({"command": command}) + "); throw new Error('after effect');"
    if prompt == "GUARD":
        return "const before=await tools.read({path:'guarded',limit:1}); if(!before.value.has_more) throw new Error('expected paged source'); const first=await tools.write({path:'guarded',content:'FIRST',base_digest:before.value.base_digest}); if(first.is_error) throw new Error('first guarded write'); const second=await tools.write({path:'guarded',content:'SECOND',base_digest:first.value.new_digest}); if(second.is_error) throw new Error('second guarded write'); const stale=await tools.write({path:'guarded',content:'LOST',base_digest:before.value.base_digest}); const after=await tools.read({path:'guarded'}); return {first:first.value.new_digest,second:second.value.new_digest,stale:stale.is_error,final:after.value.content};"
    if prompt == "ACTIVE":
        return "await Promise.all(['a','b'].map(x=>tools.exec({command:`touch ${x}.ready; sleep 1.5; printf done`}))); return true;"
    if prompt == "SELECT":
        return "const report=await tools.read({path:'one'}); text({length:report.value.content.length}); const picture=await tools.read({path:'pixel.png'}); image(picture.images[0]);"
    if prompt == "HISTORY_SOURCE":
        command = "printf 'HISTORY_NEEDLE_SMOKE\\n' >> effects; printf 'HISTORY_NEEDLE_SMOKE\\n'"
        return "await tools.exec(" + json.dumps({"command": command}) + "); return true;"
    if prompt == "HISTORY_RECOVERY":
        return "const found=await tools.history({action:'search',query:'HISTORY_'+'NEEDLE_SMOKE'}); if(found.is_error) throw new Error(JSON.stringify(found.value)); const target=found.value.entries.find(entry=>entry.kind==='tool_result'&&entry.tool&&entry.tool.name==='exec'); if(!target) throw new Error('missing compacted tool result'); const page=await tools.history({action:'read',entry:target.entry,limit:512}); if(page.is_error) throw new Error(JSON.stringify(page.value)); return {entry:target.entry,state:target.state,content:page.value.entry.content,next:page.value.entry.next_offset};"
    if prompt == "MCP":
        return "const candidates=await searchTools('Lookup an item'); const bounded=await searchTools('Lookup an item',{limit:1}); if(bounded.length!==1 || bounded[0].name!==candidates[0].name) throw new Error('bounded discovery'); let rejected=false; try { await searchTools('Lookup an item',{limit:null}); } catch (_) { rejected=true; } if(!rejected) throw new Error('invalid limit admitted'); const [candidate]=bounded; const d=await describeTool(candidate.name); if(d.input_schema.required[0]!=='index' || await describeTool('code_mode')!==null || await describeTool('missing')!==null) throw new Error('discovery contract'); const rs=await Promise.all([1,2].map(index=>tools[d.name]({index}))); return {total:rs.reduce((n,r)=>n+r.value.structured_content.count,0)};"
    if prompt in ("IO", "STDERR"):
        first = "sleep 0.5; printf FIRST_DONE" if prompt == "STDERR" else "exec sleep 30"
        return "await Promise.all([" + ",".join(
            "tools.exec(" + json.dumps({"command": f"printf '%s\\n' $$ > {x}.pid; printf STARTED_{x}; touch {x}.ready; {end}", "timeout_ms": 120000}) + ")"
            for x, end in [("a", first), ("b", "exec sleep 30")]
        ) + "]); return true;"
    return "const rs=await Promise.all(['one','two'].map(path=>tools.read({path}))); await tools.write({path:'changed',content:'done'}); return {lengths:rs.map(r=>r.value.content.length)};"


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        messages = body["messages"]
        user_index = max(i for i, message in enumerate(messages) if message["role"] == "user")
        prompt = messages[user_index]["content"]
        enabled = "code_mode" in {tool["function"]["name"] for tool in body.get("tools", [])}
        assert "PRIVATE_" not in json.dumps(body), "child payload entered model context"
        fresh = not any(message["role"] == "tool" for message in messages[user_index + 1:])
        if enabled and prompt in ("RECOVER_CAPTURE", "INSPECT_RPC") and fresh:
            if prompt == "RECOVER_CAPTURE":
                assert "assistant_entry" not in json.dumps(messages), "cut retained the original locator"
            discovery = "const refs=await inspectSaved({kind:'parents'}); let parent; for(const ref of refs.parents) { const cs=await inspectSaved({kind:'children',parent:ref.parent}); if(cs.children.some(c=>c.name==='exec')) { parent=ref.parent; break; } } if(!parent) throw new Error('saved exec not found'); "
            code = discovery + "const meta=await inspectSaved({kind:'children',parent}); const path=await inspectSaved({kind:'output',parent,child:0,pointer:'/stdout_full_path'}); const exit=await inspectSaved({kind:'output',parent,child:0,pointer:'/exit_code'}); const saved=await tools.read({path:JSON.parse(path.json),limit:1024}); if(saved.is_error) return {read_failure:saved.value}; return {nonce:saved.value.content.split('\\n')[0], original_failed:path.is_error, exit:JSON.parse(exit.json), states:meta.children.map(c=>c.state)};"
            if prompt == "INSPECT_RPC":
                code = discovery + "const exit=await inspectSaved({kind:'output',parent,child:0,pointer:'/exit_code'}); return {exit:JSON.parse(exit.json), original_failed:exit.is_error};"
            delta = {"tool_calls": [{"index": 0, "id": "inspect", "type": "function", "function": {"name": "code_mode", "arguments": json.dumps({"code": code})}}]}
            reason = "tool_calls"
        elif enabled and prompt == "HISTORY_SOURCE" and fresh:
            delta = {"tool_calls": [{"index": 0, "id": "history-source", "type": "function", "function": {"name": "exec", "arguments": json.dumps({"command": "printf 'HISTORY_NEEDLE_SMOKE\\n' >> effects; printf 'HISTORY_NEEDLE_SMOKE\\n'"})}}]}
            reason = "tool_calls"
        elif enabled and prompt in ("FANOUT", "GUARD", "MCP", "SELECT", "IO", "STDERR", "ACTIVE", "RECOVER_FAIL", "HISTORY_RECOVERY") and fresh:
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
        if prompt == "ACTIVE" and not fresh:
            # Provisional final text is not committed Turn completion.
            first, rest = payload.split(b"\n\n", 1)
            self.wfile.write(first + b"\n\n")
            self.wfile.flush()
            time.sleep(0.5)
            self.wfile.write(rest)
        else:
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
    (workspace / "pixel.png").write_bytes(base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="))
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "TERM": "xterm-256color"}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "code", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        assert run(env, workspace, "run", "DEFAULT").stdout.strip() == "DIRECT_OK"
        records = [json.loads(line) for line in run(env, workspace, "--code-mode", "--json", "run", "FANOUT").stdout.splitlines()]
        session_id = next(record["id"] for record in records if record["type"] == "session")
        children = [record for record in records if record["type"] == "child_tool_finished"]
        assert len(children) == 3 and all(record["state"] == "observed" and not record["is_error"] for record in children), children
        assert (workspace / "changed").read_text() == "done"
        assert run(env, workspace, "--session", session_id, "export").stdout.count("PRIVATE_") == 200
        assert run(env, workspace, "--session", session_id, "run", "RESUME").stdout.strip() == "DIRECT_OK"
        # The guard must cover unread lines, not just this first-line page.
        (workspace / "guarded").write_text("PRIVATE_GUARD_\n" * 4000)
        guarded = [json.loads(line) for line in run(env, workspace, "--code-mode", "--json", "run", "GUARD").stdout.splitlines()]
        parent = next(record for record in guarded if record["type"] == "tool_finished")
        assert parent["output"]["result"] == {"first": hashlib.sha256(b"FIRST").hexdigest(), "second": hashlib.sha256(b"SECOND").hexdigest(), "stale": True, "final": "SECOND"}, parent
        assert parent["output"]["calls"] == 5 and parent["output"]["failed_calls"] == 1
        assert (workspace / "guarded").read_text() == "SECOND"
        config = work / "config/ion/mcp.json"
        config.write_text(json.dumps({"servers": {"demo": {"command": sys.executable, "args": [str(Path(__file__).resolve()), "mcp"]}}}))
        records = [json.loads(line) for line in run(env, workspace, "--code-mode", "--json", "run", "MCP").stdout.splitlines()]
        parent = next(record for record in records if record["type"] == "tool_finished")
        assert parent["output"]["result"] == {"total": 3}, parent
        assert len([record for record in records if record["type"] == "child_tool_finished"]) == 2
        config.unlink()

        selected = [json.loads(line) for line in run(env, workspace, "--code-mode", "--json", "run", "SELECT").stdout.splitlines()]
        parent = next(record for record in selected if record["type"] == "tool_finished")
        assert parent["output"]["result"] is None, parent
        assert parent["output"]["output"] == [{"type": "text", "text": '{"length":1200}'}, {"type": "image", "index": 0}], parent
        assert parent["image_mime_types"] == ["image/png"], parent
        image_parts = [part for message in requests[-1]["messages"] if isinstance(message["content"], list) for part in message["content"] if part["type"] == "image_url"]
        assert len(image_parts) == 1 and image_parts[0]["image_url"]["url"].startswith("data:image/png;base64,"), requests[-1]

        # The nonce exists only at the start of a retained complete capture,
        # not in the bounded native tail or any model-visible parent output.
        recovery_work = work / "recovery"
        recovery_work.mkdir()
        records = [json.loads(line) for line in run(env, recovery_work, "--code-mode", "--json", "run", "RECOVER_FAIL").stdout.splitlines()]
        recovery_id = next(record["id"] for record in records if record["type"] == "session")
        original = next(record for record in records if record["type"] == "child_tool_finished")
        capture = Path(original["output"]["stdout_full_path"])
        try:
            failed_parent = next(record for record in records if record["type"] == "tool_finished")
            assert isinstance(failed_parent["output"].get("parent"), dict), "failed guest has no stable evidence locator"
            saved_bytes = capture.read_bytes()
            nonce = saved_bytes.splitlines()[0].decode()
            assert len(saved_bytes) == 100050 and len(nonce) == 48
            assert nonce not in original["output"]["stdout"]
            # The controlled summary omits locators. This tests the cut/reopen
            # boundary, not live-model summary fidelity or adherence.
            run(env, recovery_work, "--session", recovery_id, "compact")
            before_recovery = len(requests)
            records = [json.loads(line) for line in run(env, recovery_work, "--session", recovery_id, "--code-mode", "--json", "run", "RECOVER_CAPTURE").stdout.splitlines()]
            recovered = next(record for record in records if record["type"] == "tool_finished")
            assert not recovered["is_error"], recovered
            assert recovered["output"]["result"] == {"nonce": nonce, "original_failed": True, "exit": 7, "states": ["observed"]}, recovered
            assert recovered["output"]["calls"] == 1
            assert len([record for record in records if record["type"] == "child_tool_finished"]) == 1
            assert nonce not in json.dumps(requests[before_recovery])
            assert (recovery_work / "effects").read_text() == "ONCE"
            assert capture.read_bytes() == saved_bytes
        finally:
            capture.unlink()

        # Recover a committed tool observation after a real compaction through
        # the built Code Mode route. The summary request does not see the needle;
        # the history tool reads the original tool-result entry and must not
        # repeat its shell effect.
        history_work = work / "history"
        history_work.mkdir()
        records = [json.loads(line) for line in run(env, history_work, "--code-mode", "--json", "run", "HISTORY_SOURCE").stdout.splitlines()]
        history_id = next(record["id"] for record in records if record["type"] == "session")
        assert (history_work / "effects").read_text() == "HISTORY_NEEDLE_SMOKE\n"
        run(env, history_work, "--session", history_id, "compact")
        before_recovery = len(requests)
        records = [json.loads(line) for line in run(env, history_work, "--session", history_id, "--code-mode", "--json", "run", "HISTORY_RECOVERY").stdout.splitlines()]
        recovered = next(record for record in records if record["type"] == "tool_finished")
        assert not recovered["is_error"], recovered
        result = recovered["output"]["result"]
        assert result["state"] == "observed", recovered
        assert "HISTORY_NEEDLE_SMOKE" in result["content"], recovered
        assert isinstance(result["entry"], int) and result["entry"] > 0
        assert "HISTORY_NEEDLE_SMOKE" not in json.dumps(requests[before_recovery])
        assert (history_work / "effects").read_text() == "HISTORY_NEEDLE_SMOKE\n"

        # Read the same settled facts through RPC after another process reopen,
        # even though the external capture has now been removed.
        inspector = subprocess.Popen([binary, "--cwd", recovery_work, "--session", recovery_id, "--code-mode", "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            assert read_record(inspector)["type"] == "ready"
            inspector.stdin.write(b'{"type":"prompt","message":"INSPECT_RPC"}\n')
            rpc_records = []
            while True:
                record = read_record(inspector)
                rpc_records.append(record)
                if record["type"] == "turn_end":
                    assert record["status"] == "completed", record
                    break
            evidence = next(record for record in rpc_records if record["type"] == "tool_finished")
            assert evidence["output"]["result"] == {"exit": 7, "original_failed": True}, evidence
            assert evidence["output"]["calls"] == 0
            assert not any(record["type"] == "child_tool_finished" for record in rpc_records)
            assert (recovery_work / "effects").read_text() == "ONCE"
            inspector.stdin.close()
            inspector.stdin = None
            _, stderr = inspector.communicate(timeout=10)
            assert inspector.returncode == 0, stderr
        finally:
            if inspector.poll() is None:
                inspector.kill()
                inspector.wait(timeout=5)

        rpc_work = work / "rpc"
        rpc_work.mkdir()
        child = subprocess.Popen([binary, "--cwd", rpc_work, "--code-mode", "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            admission = read_record(child)
            assert admission["type"] == "ready"
            child.stdin.write(b'{"type":"prompt","message":"IO"}\n')
            ready(rpc_work, child)
            child.stdin.close()
            child.stdin = None
            stdout, stderr = child.communicate(timeout=10)
            assert child.returncode == 0, stderr
            records = [json.loads(line) for line in stdout.splitlines()]
            ended = next(record for record in records if record["type"] == "turn_end")
            assert ended["status"] == "cancelled", ended
            if ended["progress_complete"]:
                assert len([record for record in records if record["type"] == "child_tool_finished" and record["output"]["cancelled"]]) == 2, records
            else:
                assert any(record["type"] == "publication_gap" and record["resync"] == "inspect" for record in records), records
            # EOF joins before publication, which may omit provisional events.
            # Durable observations and independently dead processes are required.
            saved = json.loads(run(env, rpc_work, "--session", admission["session"], "inspect").stdout)
            outcomes = [entry["data"]["outcome"] for entry in saved["entries"] if entry["kind"] == "child_tool_result"]
            assert len(outcomes) == 2 and all(outcome["state"] == "observed" and outcome["output"]["value"]["cancelled"] for outcome in outcomes), outcomes
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
            existing_sessions = set((work / "state/ion/sessions").rglob("*.sqlite"))
            child = subprocess.Popen([binary, "--cwd", active_work, "--code-mode", "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_active)
            os.close(slave)
            output = bytearray()
            submitted = running = finished = False
            last_quit = 0
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
                    if not submitted and b"\xe2\x80\xba" in output:
                        output.clear()
                        os.write(master, b"ACTIVE\r")
                        submitted = True
                    if submitted and b"2 running" in output:
                        running = True
                    if submitted and not finished and b"CODE_OK" in output:
                        assert running, (mode, output[-3000:])
                        assert all((active_work / f"{x}.ready").exists() for x in ("a", "b"))
                        sessions = set((work / "state/ion/sessions").rglob("*.sqlite")) - existing_sessions
                        assert len(sessions) == 1, sessions
                        with sqlite3.connect(f"file:{next(iter(sessions))}?mode=ro", uri=True) as db:
                            completed = db.execute("SELECT count(*) FROM entries WHERE json_extract(body,'$.kind')='turn_ended' AND json_extract(body,'$.data.reason')='completed'").fetchone()[0]
                        finished = completed == 1
                    # Completion publication can lag its commit. Empty-composer
                    # EOF remains safe if delivery occurs during terminal restore.
                    if finished and time.monotonic() - last_quit > 0.25:
                        os.write(master, b"\x04")
                        last_quit = time.monotonic()
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
                        os.write(master, b"\x04")  # Quit, not a racing stop request.
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
