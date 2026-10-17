"""Offline built-client qualification of managed coding Sessions and custody."""
import json
import os
import queue
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
    subprocess.run([binary, "use", "smoke", "coding", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True, text=True, timeout=10)


def call(name, arguments):
    return {"tool_calls": [{"index": 0, "id": name, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}


def until(predicate, description, timeout=10):
    deadline = time.monotonic() + timeout
    while not predicate():
        assert time.monotonic() < deadline, description
        time.sleep(0.01)


def dead(pid):
    try:
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True


class Rpc:
    def __init__(self, env, workspace):
        self.process = subprocess.Popen([binary, "--cwd", workspace, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.queue = queue.Queue()
        self.records = []
        self.number = 0
        def read():
            for line in self.process.stdout:
                self.queue.put(json.loads(line))
            self.queue.put(None)
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        self.ready = self.wait(lambda record: record.get("type") == "ready")

    def send(self, kind, **fields):
        self.number += 1
        identity = str(self.number)
        self.process.stdin.write(json.dumps({"type": kind, "id": identity, **fields}) + "\n")
        self.process.stdin.flush()
        return identity

    def wait(self, predicate, timeout=10):
        deadline = time.monotonic() + timeout
        while True:
            record = self.queue.get(timeout=max(0.001, deadline - time.monotonic()))
            assert record is not None, (self.process.poll(), self.records[-5:])
            self.records.append(record)
            if predicate(record):
                return record

    def settled(self, turn, status):
        predicate = lambda record: record.get("type") == "turn_end" and record.get("turn") == turn
        record = next((record for record in self.records if predicate(record)), None)
        record = record if record is not None else self.wait(predicate)
        assert record["status"] == status, record
        return record

    def request(self, kind, **fields):
        identity = self.send(kind, **fields)
        return self.wait(lambda record: record.get("type") == "response" and record.get("id") == identity)

    def close(self):
        if self.process.stdin is not None:
            self.process.stdin.close()
            self.process.stdin = None
        self.process.wait(timeout=10)
        self.reader.join(timeout=5)
        assert not self.reader.is_alive()
        return self.process.stderr.read()

    def cleanup(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        self.reader.join(timeout=5)


with tempfile.TemporaryDirectory(prefix="ion-coding-subagent-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "AGENTS.md").write_text("SUBAGENT_RESOURCE: follow this workspace's instructions.\n")
    env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
    env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
    Path(env["HOME"]).mkdir()
    requests, failures, changed_route = [], [], set()
    processes, rpcs, pids = [], [], []

    def histories():
        return [(path, *facts(path)) for path in Path(env["XDG_STATE_HOME"]).rglob("*.sqlite")]

    def parent_for(identity):
        return next(path for path, header, _entries in histories() if header["id"] == identity)

    def marker_pid(tag):
        until(lambda: (workspace / (tag + ".ready")).exists(), "native child did not start: " + tag)
        pid = int((workspace / (tag + ".pid")).read_text())
        pids.append(pid)
        return pid

    def root_ended(path):
        return any(entry["kind"] == "turn_ended" for entry in facts(path)[1])

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(body)
                user_text = next(message["content"] for message in reversed(body["messages"]) if message["role"] == "user")
                user = user_text.split()[0]
                tools = {tool["function"]["name"] for tool in body.get("tools", [])}
                fresh = body["messages"][-1]["role"] == "user"
                assert body["model"] == "coding"
                assert "SUBAGENT_RESOURCE" in json.dumps(body)
                effort = {"PARENT_DIRECT":"high", "CHILD_DIRECT":"high", "PARENT_COMPOSED":"low", "CHILD_COMPOSED":"low", "PARENT_CANCEL":"medium", "CHILD_CANCEL":"medium", "PARENT_FORK":"high", "CHILD_FORK":"high"}.get(user)
                if effort:
                    assert body.get("reasoning_effort") == effort, body
                if user == "PARENT_DEFAULT":
                    assert "subagent" in tools
                    delta = {"content": "DEFAULT_OK"}
                elif user == "SEED":
                    delta = {"content": "SEEDED_CONTEXT"}
                elif user == "PARENT_BUSY":
                    delta = call("exec", {"command": "printf '%s' $$ > busy.pid; touch busy.ready; exec sleep 30", "timeout_ms":120000})
                elif user == "PARENT_BG_FAULT_ACTIVE" and not fresh:
                    delta = call("exec", {"command":"printf '%s' $$ > active.pid; touch active.ready; exec sleep 30", "timeout_ms":120000})
                elif user.startswith("PARENT_"):
                    assert "subagent" in tools
                    if fresh:
                        task = "CHILD_" + user.removeprefix("PARENT_")
                        arguments = {"action":"start", "task": task}
                        if user in ("PARENT_DIRECT", "PARENT_COMPOSED", "PARENT_CANCEL"):
                            configure(env, "http://127.0.0.1:1/v1/chat/completions")
                            changed_route.add(user)
                        if user == "PARENT_FORK":
                            arguments["context"] = {"type":"fork"}
                        if user.startswith("PARENT_BG_"):
                            arguments["mode"] = "background"
                        if user == "PARENT_BG_EOF":
                            arguments["task"] += " " + "x" * 64000
                        if user == "PARENT_BG_WAIT":
                            code = "const r=await tools.subagent(" + json.dumps(arguments) + "); await tools.exec({command:'while [ ! -f CHILD_BG_WAIT.ready ]; do sleep 0.01; done; touch parent.work',timeout_ms:10000}); const joined=await tools.subagent({action:'wait',child:r.value.child_session}); const same=await tools.subagent({action:'status',child:r.value.child_session}); const ack=await tools.subagent({action:'acknowledge',child:r.value.child_session}); return {joined:joined.value,same:same.value,ack:ack.value};"
                            delta = call("code_mode", {"code":code})
                        elif user == "PARENT_COMPOSED":
                            delta = call("code_mode", {"code":"const r=await tools.subagent(" + json.dumps(arguments) + "); return r.value;"})
                        else:
                            delta = call("subagent", arguments)
                    else:
                        assert "PRIVATE_CHILD_TOOL" not in json.dumps(body)
                        if not user.startswith("PARENT_BG_") or user == "PARENT_BG_WAIT":
                            assert "CHILD_ANSWER" in json.dumps(body)
                        delta = {"content": "BACKGROUND_ACCEPTED" if user.startswith("PARENT_BG_") else "PARENT_OK"}
                else:
                    assert user.startswith("CHILD_")
                    assert "subagent" not in tools, "built-in recursively enabled"
                    if user in ("CHILD_DIRECT", "CHILD_COMPOSED", "CHILD_CANCEL", "CHILD_FORK"):
                        assert ("code_mode" in tools) == (user == "CHILD_COMPOSED")
                    assert "PARENT_" not in json.dumps(body), "active parent conversation leaked"
                    assert ("SEEDED_CONTEXT" in json.dumps(body)) == (user == "CHILD_FORK"), "incorrect fork prefix"
                    if fresh:
                        if user == "CHILD_CANCEL":
                            delta = call("exec", {"command":"printf '%s' $$ > CHILD_CANCEL.pid; touch CHILD_CANCEL.ready; exec sleep 30", "timeout_ms":120000})
                        elif user.startswith("CHILD_BG_"):
                            release = "parent.work" if user == "CHILD_BG_WAIT" else user + ".release"
                            native = "printf '%s' $$ > " + user + ".pid; touch " + user + ".ready; while [ ! -f " + release + " ]; do sleep 0.01; done; printf CHILD_EFFECT > " + user + ".effect"
                            delta = call("exec", {"command":native,"timeout_ms":120000})
                        else:
                            marker = next(name for name in tools if name.startswith("mcp__") and name.endswith("marker"))
                            if user == "CHILD_COMPOSED":
                                code = "const m=await tools[" + json.dumps(marker) + "]({}); await tools.write({path:'CHILD_COMPOSED.txt',content:'CHILD_EFFECT'}); return {marker:m.value.content};"
                                delta = call("code_mode", {"code":code})
                            else:
                                first = call(marker, {})["tool_calls"][0]
                                second = call("write", {"path":user + ".txt", "content":"CHILD_EFFECT"})["tool_calls"][0]
                                second["index"] = 1
                                delta = {"tool_calls":[first, second]}
                    else:
                        if not user.startswith("CHILD_BG_"):
                            assert "PRIVATE_CHILD_TOOL" in json.dumps(body)
                        delta = {"content":"CHILD_ANSWER_" + user}
                reason = "tool_calls" if "tool_calls" in delta else "stop"
                events = [{"id":"fixture", "choices":[{"index":0,"delta":delta,"finish_reason":None}]}, {"id":"fixture","choices":[{"index":0,"delta":{},"finish_reason":reason}]}]
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
    config.write_text(json.dumps({"servers":{"fixture":{"command":sys.executable,"args":[str(Path(__file__).resolve()),"mcp"]}}}))
    passed = False
    try:
        configure(env, endpoint)
        assert command(env, workspace, "run", "PARENT_DEFAULT").stdout.strip() == "DEFAULT_OK"
        for composed in (False, True):
            configure(env, endpoint)
            before = {path for path, _header, _entries in histories()}
            prompt = "PARENT_COMPOSED" if composed else "PARENT_DIRECT"
            records = [json.loads(line) for line in command(env, workspace, "--reasoning", "low" if composed else "high", *(["--code-mode"] if composed else []), "--json", "run", prompt).stdout.splitlines()]
            result = next(record["output"] for record in records if record["type"] == "tool_finished")
            if composed:
                result = result["result"]
            assert result["outcome"]["answer"] == "CHILD_ANSWER_CHILD_" + ("COMPOSED" if composed else "DIRECT"), result
            assert result["outcome"]["receipt"]["end"] == "completed" and result["joined"], result
            child_path = Path(result["path"])
            allocated = {path for path, _header, _entries in histories()} - before
            assert len(allocated) == 2 and child_path in allocated
            parent_path = (allocated - {child_path}).pop()
            future = time.time_ns() + 5_000_000_000
            os.utime(child_path, ns=(future, future))
            count = len(requests)
            assert prompt in command(env, workspace, "--continue", "export").stdout
            assert len(requests) == count
            parent_header, entries = facts(parent_path)
            child_header, child_entries = facts(child_path)
            admitted = next(entry["data"]["record"] for entry in entries if entry["kind"] == "delegation_admitted")
            assert admitted["child"] == child_header["id"] == result["child_session"]
            assert admitted["site"]["session"] == parent_header["id"]
            assert admitted["ownership"] == "foreground" and admitted["spec"]["context"] == "fresh"
            assert admitted["spec"]["reasoning"] == ("Low" if composed else "High")
            assert child_entries[0] == {"kind":"child_session_of","data":{"record":admitted}}
            assert Path(child_header["cwd"]).samefile(workspace)
            assert child_path.parent == parent_path.parent / "children"
            assert not any(entry["kind"] == "delegation_transferred" for entry in entries)
            admission = next(i for i, entry in enumerate(entries) if entry["kind"] == "delegation_admitted")
            settlement = next(i for i, entry in enumerate(entries) if entry["kind"] == "delegation_settled")
            outer = next(i for i, entry in enumerate(entries) if entry["kind"] == ("child_tool_result" if composed else "tool_result"))
            assert admission < settlement < outer
            assert entries[settlement]["data"]["outcome"]["answer"] == result["outcome"]["answer"]
            assert (workspace / ("CHILD_COMPOSED.txt" if composed else "CHILD_DIRECT.txt")).read_text() == "CHILD_EFFECT"
            assert prompt in changed_route

        # Fork shares only the settled prefix; execution preferences remain current.
        configure(env, endpoint)
        before = {path for path, _header, _entries in histories()}
        command(env, workspace, "--reasoning", "low", "run", "SEED")
        parent_path = ({path for path, _header, _entries in histories()} - before).pop()
        command(env, workspace, "--session", str(parent_path), "--reasoning", "high", "run", "PARENT_FORK")
        entries = facts(parent_path)[1]
        link = next(entry["data"]["record"] for entry in entries if entry["kind"] == "delegation_admitted")
        assert link["spec"]["context"] == {"fork":{"point":{"before_turn":2}}}
        assert link["spec"]["reasoning"] == "High"

        # Parent and child actually overlap, then wait/status/ack return one report.
        records = [json.loads(line) for line in command(env, workspace, "--code-mode", "--json", "run", "PARENT_BG_WAIT").stdout.splitlines()]
        result = next(record["output"]["result"] for record in records if record["type"] == "tool_finished")
        assert result["joined"] == result["same"] == result["ack"], result
        assert result["joined"]["outcome"]["answer"] == "CHILD_ANSWER_CHILD_BG_WAIT"
        assert (workspace / "CHILD_BG_WAIT.effect").read_text() == "CHILD_EFFECT"

        # EOF of foreground ownership joins an already-started native process.
        configure(env, endpoint)
        rpc = Rpc(env, workspace)
        rpcs.append(rpc)
        assert rpc.request("set_reasoning", effort="medium")["success"]
        rpc.send("prompt", message="PARENT_CANCEL")
        pid = marker_pid("CHILD_CANCEL")
        assert rpc.close() == ""
        assert rpc.process.returncode == 0 and dead(pid)

        configure(env, endpoint)
        rpc = Rpc(env, workspace)
        rpcs.append(rpc)
        identity = rpc.send("prompt", message="PARENT_BG_IDLE")
        pid = marker_pid("CHILD_BG_IDLE")
        parent_path = parent_for(rpc.ready["session"])
        until(lambda: root_ended(parent_path), "background parent did not finish")
        state = rpc.request("get_background")["data"]["background"]
        job = state["children"][0]
        child_id = job["receipt"]["record"]["child"]
        assert not job["joined"] and job["receipt"]["transferred"]
        rpc.settled(1, "completed")
        # Abort a different active parent operation, not the transferred child.
        rpc.send("prompt", message="PARENT_BUSY")
        busy = marker_pid("busy")
        assert rpc.request("abort")["success"]
        rpc.settled(2, "cancelled")
        until(lambda: dead(busy), "parent abort did not join native work")
        assert not dead(pid), "parent abort crossed background ownership"
        until(lambda: any(entry["kind"] == "turn_ended" and entry["data"]["turn"] == 2 for entry in facts(parent_path)[1]), "aborted parent did not settle")
        replacement = rpc.request("new_session")
        assert replacement["success"] and replacement["data"]["session"] != rpc.ready["session"], replacement
        # Reuse the actual retained parent writer, not a competing open by path.
        assert rpc.request("switch_session", session=str(parent_path))["success"]
        assert not rpc.request("switch_session", session=job["receipt"]["record"]["path"])["success"], "live child was adopted"
        revision = rpc.request("get_background")["data"]["background"]["revision"]
        (workspace / "CHILD_BG_IDLE.release").touch()
        rpc.wait(lambda record: record.get("type") == "background_changed" and record["revision"] > revision)
        joined = rpc.request("get_background")["data"]["background"]["children"][0]
        assert joined["joined"] and joined["receipt"]["outcome"]["answer"] == "CHILD_ANSWER_CHILD_BG_IDLE"
        assert rpc.request("acknowledge_background", child=child_id)["success"]
        assert rpc.request("get_background")["data"]["background"]["children"] == []
        assert not rpc.request("cancel_background", child=child_id)["success"]
        # Explicit child cancellation does not close the application or its
        # parent Session. Its cancelled result remains available until ack.
        rpc.send("prompt", message="PARENT_BG_CANCEL")
        cancelled_pid = marker_pid("CHILD_BG_CANCEL")
        cancel_job = rpc.request("get_background")["data"]["background"]["children"][0]
        cancel_id = cancel_job["receipt"]["record"]["child"]
        assert rpc.request("cancel_background", child=cancel_id)["success"]
        until(lambda: dead(cancelled_pid), "explicit child cancellation did not join native work")
        until(lambda: rpc.request("get_background")["data"]["background"]["children"][0]["joined"], "cancelled child not retained")
        cancelled = rpc.request("get_background")["data"]["background"]["children"][0]
        assert cancelled["receipt"]["outcome"]["receipt"]["end"] == "cancelled"
        assert cancelled["receipt"]["outcome"]["answer"] is None
        until(lambda: not rpc.request("get_state")["data"]["busy"], "parent launch did not settle")
        rpc.send("prompt", message="PARENT_DEFAULT")
        assert rpc.settled(4, "completed")["text"] == "DEFAULT_OK"
        assert rpc.request("get_state")["success"], "child cancel closed the parent"
        rpc.close()
        assert rpc.process.returncode == 0 and dead(pid)

        # Normal headless completion keeps ownership; interrupt after the parent
        # ended still stops/joins children without rewriting that completed Turn.
        for tag, interrupt in [("BG_DRAIN", False), ("BG_INTERRUPT", True)]:
            before = {path for path, _header, _entries in histories()}
            process = subprocess.Popen([binary, "--cwd", workspace, "run", "PARENT_" + tag], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            processes.append(process)
            pid = marker_pid("CHILD_" + tag)
            def ended_parent():
                for path, _header, entries in histories():
                    if path not in before and any(entry["kind"] == "delegation_admitted" for entry in entries) and root_ended(path):
                        return path
                return None
            until(ended_parent, "headless parent did not complete")
            parent_path = ended_parent()
            assert process.poll() is None and not dead(pid), "headless dropped live background ownership"
            if interrupt:
                process.send_signal(signal.SIGINT)
            else:
                (workspace / ("CHILD_" + tag + ".release")).touch()
            output, error = process.communicate(timeout=10)
            assert (process.returncode != 0) == interrupt, (output, error)
            assert dead(pid)
            entries = facts(parent_path)[1]
            assert next(entry["data"]["reason"] for entry in entries if entry["kind"] == "turn_ended") == "completed"
            outcome = next(entry["data"]["outcome"] for entry in entries if entry["kind"] == "delegation_settled")
            assert (outcome["answer"] is None) == interrupt
            assert outcome["kind"] == "joined"

        # EOF cleanup precedes blocked recovery publication. This pipe is not
        # read until the actual background PID has stopped; inspect is >128 KiB.
        process = subprocess.Popen([binary, "--cwd", workspace, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(process)
        assert select.select([process.stdout], [], [], 8)[0]
        ready = json.loads(process.stdout.readline())
        process.stdin.write(json.dumps({"type":"prompt","id":"start","message":"PARENT_BG_EOF"}) + "\n")
        process.stdin.flush()
        pid = marker_pid("CHILD_BG_EOF")
        parent_path = parent_for(ready["session"])
        until(lambda: root_ended(parent_path), "EOF parent did not finish")
        process.stdin.write(json.dumps({"type":"inspect","id":"large"}) + "\n")
        for index in range(64):
            process.stdin.write(json.dumps({"type":"get_state","id":str(index)}) + "\n")
        process.stdin.flush()
        process.stdin.close()
        process.stdin = None
        until(lambda: dead(pid), "blocked RPC output prevented background stop/join")
        output, error = process.communicate(timeout=10)
        assert process.returncode == 0, (output[-1000:], error)
        outcome = next(entry["data"]["outcome"] for entry in facts(parent_path)[1] if entry["kind"] == "delegation_settled")
        assert outcome["receipt"]["end"] == "cancelled" and outcome["answer"] is None

        # Failure after parent completion is application-fatal even with an idle
        # client which never asks for the result or closes stdin.
        rpc = Rpc(env, workspace)
        rpcs.append(rpc)
        rpc.send("prompt", message="PARENT_BG_FAULT")
        pid = marker_pid("CHILD_BG_FAULT")
        parent_path = parent_for(rpc.ready["session"])
        until(lambda: root_ended(parent_path), "fault parent did not complete")
        with sqlite3.connect(parent_path) as db:
            db.execute("CREATE TRIGGER fail_settlement BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind')='delegation_settled' BEGIN SELECT RAISE(FAIL,'injected late settlement failure'); END")
        (workspace / "CHILD_BG_FAULT.release").touch()
        rpc.process.wait(timeout=10)
        assert rpc.process.returncode != 0, "idle infrastructure failure was hidden"
        assert dead(pid)
        assert not any(entry["kind"] == "delegation_settled" for entry in facts(parent_path)[1])

        # An application fault may cancel a still-active parent. Its command
        # closure is failed, not misclassified as an ordinary user cancellation.
        before = {path for path, _header, _entries in histories()}
        process = subprocess.Popen([binary, "--cwd", workspace, "--json", "run", "PARENT_BG_FAULT_ACTIVE"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(process)
        marker_pid("CHILD_BG_FAULT_ACTIVE")
        active = marker_pid("active")
        parent_path = next(path for path, _header, entries in histories() if path not in before and any(entry["kind"] == "delegation_admitted" for entry in entries))
        with sqlite3.connect(parent_path) as db:
            db.execute("CREATE TRIGGER fail_settlement BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind')='delegation_settled' BEGIN SELECT RAISE(FAIL,'injected active settlement failure'); END")
        (workspace / "CHILD_BG_FAULT_ACTIVE.release").touch()
        output, error = process.communicate(timeout=10)
        assert process.returncode != 0 and dead(active), (output, error)
        records = [json.loads(line) for line in output.splitlines()]
        assert next(record for record in records if record["type"] == "run_end")["status"] == "failed"
        assert not failures, failures
        passed = True
        print("coding delegation: inherited routes/controls, fresh/fork, foreground/background, joined reports, Session rebinding, independent abort, idle completion/failure, headless drain/interrupt and blocked-output EOF passed")
    finally:
        for rpc in rpcs:
            rpc.cleanup()
        for process in processes:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=5)
        if not passed:
            for pid in pids:
                if not dead(pid):
                    try:
                        os.killpg(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
