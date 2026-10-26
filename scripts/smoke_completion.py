"""Offline real-client checks of receiver-owned managed-completion delivery."""
import json
import os
import queue
import re
import select
import shlex
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

BINARY = Path(os.environ.get("ION_SMOKE_BIN", Path(__file__).resolve().parent.parent / "target/debug/ion"))
NOTICE = "Automated managed-work readiness (not a user instruction)."


def until(predicate, description, timeout=10):
    deadline = time.monotonic() + timeout
    while not predicate():
        assert time.monotonic() < deadline, description
        time.sleep(0.01)


def facts(path):
    with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as db:
        return [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]


def dead(pid):
    try:
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True


def tool(name, arguments):
    return {"tool_calls": [{"index": 0, "id": name, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}


def text(message):
    value = message.get("content")
    return value if isinstance(value, str) else "".join(part.get("text", "") for part in value or [])


class Rpc:
    def __init__(self, fixture, policy):
        self.fixture = fixture
        self.process = subprocess.Popen(fixture.command("--completion-delivery", policy, "rpc"), env=fixture.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        fixture.processes.append(self.process)
        self.events, self.records, self.number = queue.Queue(), [], 0
        def read():
            for line in self.process.stdout:
                self.events.put(json.loads(line))
            self.events.put(None)
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        fixture.rpcs.append(self)
        self.ready = self.wait(lambda r: r.get("type") == "ready")
        self.path = fixture.session(self.ready["session"])

    def send(self, kind, **fields):
        self.number += 1
        identity = str(self.number)
        self.process.stdin.write(json.dumps({"id": identity, "type": kind, **fields}) + "\n")
        self.process.stdin.flush()
        return identity

    def wait(self, predicate):
        deadline = time.monotonic() + 10
        while True:
            record = self.events.get(timeout=max(0.001, deadline - time.monotonic()))
            assert record is not None, (self.process.poll(), self.records[-5:])
            self.records.append(record)
            if predicate(record):
                return record

    def request(self, kind, **fields):
        identity = self.send(kind, **fields)
        record = self.wait(lambda r: r.get("type") == "response" and r.get("id") == identity)
        assert record["success"], record
        return record["data"]

    def settled(self, turn, status="completed"):
        match = lambda r: r.get("type") == "turn_end" and r.get("turn") == turn
        record = next((r for r in self.records if match(r)), None) or self.wait(match)
        assert record["status"] == status, record
        return record

    def job(self):
        runs = self.request("get_background")["background"]["runs"]
        assert len(runs) == 1, runs
        return runs[0]

    def close(self):
        if self.process.poll() is None and self.process.stdin is not None:
            self.process.stdin.close()
            self.process.stdin = None
        self.process.wait(timeout=10)
        self.reader.join(timeout=5)
        assert not self.reader.is_alive()
        assert self.process.returncode == 0, self.process.stderr.read()


class Fixture:
    def __init__(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ion-completion-")
        self.root = Path(self.temporary.name)
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        self.env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
        self.env.update(HOME=str(self.root / "home"), ION_HOME=str(self.root / "ion"), XDG_CONFIG_HOME=str(self.root / "config"), XDG_STATE_HOME=str(self.root / "state"), TMPDIR=str(self.root / "tmp"), TERM="xterm-256color", PYTHONSAFEPATH="1")
        Path(self.env["HOME"]).mkdir()
        Path(self.env["TMPDIR"]).mkdir()
        self.processes, self.rpcs, self.requests, self.failures = [], [], [], []
        self.parent_waiting, self.parent_release = threading.Event(), threading.Event()
        fixture = self
        class Provider(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                try:
                    body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    fixture.requests.append(body)
                    messages = body["messages"]
                    index, human = next((index, text(message)) for index, message in reversed(list(enumerate(messages))) if message["role"] == "user" and not text(message).startswith(NOTICE))
                    current = messages[index + 1:]
                    tools = {item["function"]["name"] for item in body.get("tools", [])}
                    calls = [call for message in current for call in message.get("tool_calls", [])]
                    notices = [(index, text(message)) for index, message in enumerate(current) if message["role"] == "user" and text(message).startswith(NOTICE)]
                    if human.startswith("CHILD_"):
                        assert "subagent" not in tools and not any(message["role"] == "user" and text(message).startswith("WATCH_") for message in messages), "child inherited parent control/context"
                        if not calls:
                            command = fixture.native(human)
                            delta = tool("exec", {"command": command, "timeout_ms": 120000})
                        else:
                            delta = {"content": "CHILD_DONE_" + human}
                    elif notices:
                        notice_index, notice = notices[-1]
                        run = re.search(r"run ([0-9a-f-]{36});", notice).group(1)
                        later = current[notice_index + 1:]
                        if not any(message.get("tool_calls") for message in later):
                            delta = tool("subagent", {"action": "wait", "ids": [run], "timeout_ms": 1})
                        else:
                            output = json.loads(messages[-1]["content"])
                            assert not output["is_error"], output
                            result = output["result"]
                            assert result["runs"][0]["joined"] and result["runs"][0]["outcome"]["answer"].startswith("CHILD_DONE_"), result
                            delta = {"content": "REPORT_READ_" + human}
                    elif human.startswith("WATCH_"):
                        assert "subagent" in tools
                        if not calls:
                            delta = tool("subagent", {"action": "start", "task": "CHILD_" + human, "mode": "background"})
                        else:
                            output = json.loads(messages[-1]["content"])
                            assert not output["is_error"] and output["result"]["transferred"], output
                            if human == "WATCH_CONTINUE":
                                fixture.parent_waiting.set()
                                assert fixture.parent_release.wait(5), "active-boundary fixture was not released"
                            delta = {"content": "ROOT_DONE_" + human}
                    elif human == "BUSY":
                        delta = tool("exec", {"command": fixture.native("BUSY"), "timeout_ms": 120000}) if not calls else {"content": "BUSY_DONE"}
                    else:
                        assert human == "FRESH" or human.startswith("FOLLOW_"), human
                        delta = {"content": "FRESH_DONE"}
                    finish = "tool_calls" if "tool_calls" in delta else "stop"
                    events = [{"id": "fixture", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}, {"id": "fixture", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}]
                    payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except Exception as error:
                    fixture.failures.append(repr(error))
                    self.send_error(500, str(error))
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        endpoint = f"http://127.0.0.1:{self.server.server_port}/v1/chat/completions"
        subprocess.run([BINARY, "use", "fixture", "coding", "--endpoint", endpoint, "--wire", "chat-completions"], env=self.env, capture_output=True, check=True, timeout=10)

    def command(self, *args):
        return [BINARY, "--cwd", self.workspace, *args]

    def native(self, name):
        return f"cd {shlex.quote(str(self.workspace))} && printf '%s' $$ > {name}.pid; touch {name}.ready; while [ ! -f {name}.release ]; do sleep 0.01; done; printf 'effect\\n' >> {name}.effect"

    def session(self, identity):
        for path in Path(self.env["ION_HOME"]).rglob("*.sqlite"):
            with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as db:
                if json.loads(db.execute("SELECT header FROM session WHERE id=1").fetchone()[0])["id"] == identity:
                    return path
        raise AssertionError(("missing Session", identity))

    def release_child(self, case):
        name = "CHILD_" + case
        until(lambda: (self.workspace / (name + ".ready")).exists(), "child did not execute")
        (self.workspace / (name + ".release")).touch()

    def effect(self, case):
        name = "CHILD_" + case
        assert (self.workspace / (name + ".effect")).read_text() == "effect\n", "effect repeated or absent"
        assert dead(int((self.workspace / (name + ".pid")).read_text())), "source was not joined"

    def close(self):
        self.parent_release.set()
        for rpc in self.rpcs:
            try:
                rpc.close()
            except Exception:
                if rpc.process.poll() is None:
                    rpc.process.kill()
                    rpc.process.wait(timeout=5)
        for process in self.processes:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        for path in self.workspace.glob("*.pid"):
            if not path.read_text().strip():
                continue
            pid = int(path.read_text())
            if not dead(pid):
                command = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True).stdout
                assert str(self.workspace) in command and os.getpgid(pid) == pid, "refuse cleanup of unverified process"
                os.killpg(pid, signal.SIGKILL)
                until(lambda: dead(pid), "owned process survived cleanup")
        self.temporary.cleanup()


def smoke():
    fixture = Fixture()
    try:
        for policy in ("inbox", "continue", "wake"):
            rpc = Rpc(fixture, policy)
            assert rpc.request("get_state")["completion_delivery"] == policy
            case = "WATCH_" + policy.upper()
            rpc.send("prompt", message=case)
            if policy == "continue":
                assert fixture.parent_waiting.wait(5), "parent never reached in-flight response"
            else:
                assert rpc.settled(1)["text"] == "ROOT_DONE_" + case
            fixture.release_child(case)
            if policy == "continue":
                until(lambda: rpc.job()["completion"]["state"] == "queued", "actual joined source did not queue readiness")
                fixture.parent_release.set()
                assert rpc.settled(1)["text"] == "REPORT_READ_" + case
            elif policy == "wake":
                started = rpc.wait(lambda r: r.get("type") == "automated_turn_started")
                assert started["turn"] == 2 and "id" not in started
                result = rpc.settled(2)
                assert result["automated"] and "id" not in result and result["text"] == "REPORT_READ_" + case
            else:
                until(lambda: rpc.job()["completion"]["state"] == "queued", "inbox did not retain readiness")
                assert not rpc.request("get_state")["busy"]
                assert sum(entry["kind"] == "turn_started" for entry in facts(rpc.path)) == 1, "inbox fabricated an idle Turn"
                rpc.send("prompt", message="FOLLOW_INBOX")
                assert rpc.settled(2)["text"] == "REPORT_READ_FOLLOW_INBOX"
            entries = facts(rpc.path)
            starts = [entry["data"] for entry in entries if entry["kind"] == "turn_started"]
            assert len(starts) == (1 if policy == "continue" else 2), starts
            assert sum(bool(entry.get("completion")) for entry in starts) == (policy == "wake")
            assert sum(entry["kind"] == "completion" for entry in entries) == (policy != "wake")
            job = rpc.job()
            assert job["joined"] and job["completion"]["state"] == "offered", "offer must not acknowledge the report"
            fixture.effect(case)
            rpc.request("acknowledge_background", run=job["receipt"]["record"]["run"])
            assert rpc.request("get_background")["background"]["runs"] == []
            rpc.close()

        # Stop revokes the original grant even after a fresh explicit Start.
        rpc = Rpc(fixture, "wake")
        case = "WATCH_STOP"
        rpc.send("prompt", message=case)
        rpc.settled(1)
        rpc.send("prompt", message="BUSY")
        until(lambda: (fixture.workspace / "BUSY.ready").exists(), "parent command did not start")
        rpc.request("abort")
        rpc.settled(2, "cancelled")
        assert dead(int((fixture.workspace / "BUSY.pid").read_text()))
        rpc.send("prompt", message="FRESH")
        rpc.settled(3)
        fixture.release_child(case)
        until(lambda: rpc.job()["joined"], "transferred child did not join after Stop")
        assert rpc.job()["completion"] == {"state": "revoked", "reason": "stopped"}
        assert not rpc.request("get_state")["busy"]
        assert sum(entry["kind"] == "turn_started" for entry in facts(rpc.path)) == 3
        fixture.effect(case)
        rpc.close()

        # Failed durable admission is not a started automated Turn.
        rpc = Rpc(fixture, "wake")
        case = "WATCH_STORAGE_FAULT"
        rpc.send("prompt", message=case)
        rpc.settled(1)
        with closing(sqlite3.connect(rpc.path)) as db:
            db.execute("CREATE TRIGGER reject_automatic_turn BEFORE INSERT ON entries WHEN json_extract(NEW.body, '$.kind') = 'turn_started' BEGIN SELECT RAISE(ABORT, 'fixture automatic admission fault'); END")
            db.commit()
        fixture.release_child(case)
        failure = rpc.wait(lambda r: r.get("type") == "operation_failed" and r.get("automated"))
        assert "fixture automatic admission fault" in failure["error"] and "id" not in failure, failure
        assert not any(r.get("type") == "automated_turn_started" for r in rpc.records), rpc.records
        assert sum(entry["kind"] == "turn_started" for entry in facts(rpc.path)) == 1
        assert failure["stop_requested"] and not rpc.request("get_state")["busy"]
        fixture.effect(case)
        rpc.close()

        # Headless waits for authorized wakes, not just the original parent result.
        case = "WATCH_HEADLESS"
        process = subprocess.Popen(fixture.command("--completion-delivery", "wake", "--json", "run", case), env=fixture.env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        fixture.processes.append(process)
        until(lambda: (fixture.workspace / ("CHILD_" + case + ".ready")).exists(), "headless child did not start")
        assert select.select([process.stdout], [], [], 5)[0], "headless did not publish its Session identity"
        session_record = json.loads(process.stdout.readline())
        assert session_record["type"] == "session", session_record
        path = fixture.session(session_record["id"])
        until(lambda: any(entry["kind"] == "turn_ended" for entry in facts(path)), "headless root did not settle before release")
        fixture.release_child(case)
        stdout, stderr = process.communicate(timeout=15)
        assert process.returncode == 0, stderr
        records = [session_record, *[json.loads(line) for line in stdout.splitlines()]]
        assert records[-1]["type"] == "run_end" and records[-1]["status"] == "completed", records[-3:]
        final = [record for record in records if record["type"] == "final"]
        assert len(final) == 1 and final[0]["text"] == "REPORT_READ_" + case, final
        path = fixture.session(next(record["id"] for record in records if record["type"] == "session"))
        assert any(entry["kind"] == "completion" or entry["kind"] == "turn_started" and entry["data"].get("completion") for entry in facts(path))
        fixture.effect(case)
        assert not fixture.failures, fixture.failures
        print("managed completion: inbox, active continuation, idle wake, Stop/new Start, failed admission and headless passed")
    except BaseException:
        print("provider failures:", fixture.failures)
        for rpc in fixture.rpcs:
            print("recent RPC records:", rpc.records[-5:])
        raise
    finally:
        fixture.close()


if __name__ == "__main__":
    smoke()
