"""Offline built-RPC reconnect: shared child calls, recovery and joined EOF."""
import json
import os
import queue
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

BINARY = Path(os.environ.get("ION_SMOKE_BIN", Path(__file__).resolve().parent.parent / "target/debug/ion"))
SCRIPT = Path(__file__).resolve()


def peer():
    name = sys.argv[2]
    value = os.environ["GENERATION"]

    def record(event):
        with open(name + "-events", "a") as output:
            output.write(json.dumps({"event": event, "generation": value, "pid": os.getpid()}) + "\n")

    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    record("start")
    try:
        for line in sys.stdin:
            request = json.loads(line)
            if "id" not in request:
                continue
            method = request["method"]
            if method == "initialize":
                if value == "hang":
                    record("waiting")
                    continue
                result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}},
                          "serverInfo": {"name": name, "version": value}}
            elif method == "tools/list":
                result = {"tools": [{"name": "probe", "inputSchema": {"type": "object"},
                                     "outputSchema": {"type": "object", "required": ["generation"],
                                                      "properties": {"generation": {"const": value}}}}]}
            elif method == "tools/call":
                record("call")
                if request["params"].get("arguments", {}).get("wait"):
                    record("waiting")
                    continue
                result = {"content": [], "structuredContent": {"generation": value}}
            else:
                continue
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
    finally:
        record("exit")


def until(predicate, description):
    deadline = time.monotonic() + 8
    while not predicate():
        assert time.monotonic() < deadline, description
        time.sleep(.01)


def tool(name, arguments):
    return {"tool_calls": [{"index": 0, "id": name, "type": "function",
                            "function": {"name": name, "arguments": json.dumps(arguments)}}]}


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        fixture = self.server.fixture
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            fixture.requests.append(body)
            # Readiness is an existing automated notification, not a new task.
            start, user = next((i, message["content"]) for i, message in reversed(list(enumerate(body["messages"])))
                               if message["role"] == "user" and not message["content"].startswith("Automated managed-work readiness (not a user instruction)."))
            replies = [message for message in body["messages"][start + 1:] if message["role"] == "tool"]
            names = {item["function"]["name"] for item in body.get("tools", [])}
            if user == "PARENT":
                if not replies:
                    delta = tool("subagent", {"action": "start", "task": "CHILD", "mode": "background"})
                elif len(replies) == 1:
                    run = json.loads(replies[-1]["content"])["result"]["id"]
                    delta = tool("subagent", {"action": "wait", "ids": [run], "timeout_ms": 5000})
                else:
                    joined = json.loads(replies[-1]["content"])["result"]["runs"][0]
                    assert joined["joined"] and joined["outcome"]["answer"] == "CHILD_OK", joined
                    delta = {"content": "PARENT_OK"}
            elif user == "CHILD":
                assert "subagent" not in names and "mcp__4_peer__probe" in names, names
                if not replies:
                    delta = tool("mcp__4_peer__probe", {"wait": True})
                elif len(replies) == 1:
                    observed = json.loads(replies[-1]["content"])
                    assert observed["is_error"], observed
                    delta = tool("write", {"path": "child-survived", "content": "CHILD_CONTINUED"})
                else:
                    assert not json.loads(replies[-1]["content"])["is_error"]
                    delta = {"content": "CHILD_OK"}
            else:
                assert user.startswith("PROBE-"), user
                expected = user.removeprefix("PROBE-")
                assert "mcp__6_steady__probe" in names, names
                assert ("mcp__4_peer__probe" in names) == (expected != "unavailable"), names
                if not replies:
                    name = "mcp__6_steady__probe" if expected == "unavailable" else "mcp__4_peer__probe"
                    delta = tool(name, {})
                else:
                    observed = json.loads(replies[-1]["content"])
                    assert not observed["is_error"], observed
                    assert observed["result"]["structured_content"]["generation"] == ("stable" if expected == "unavailable" else expected), observed
                    assert observed["result"]["output_schema_validation"]["status"] == "valid", observed
                    delta = {"content": "PROBE_OK"}
            reason = "tool_calls" if "tool_calls" in delta else "stop"
            payload = b"data: " + json.dumps({"choices": [{"index": 0, "delta": delta, "finish_reason": reason}]}).encode() + b"\n\ndata: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except Exception as error:
            fixture.failures.append(repr(error))
            self.send_error(500, str(error))

    def log_message(self, *_):
        pass


class Remote(BaseHTTPRequestHandler):
    def check(self):
        fixture = self.server.fixture
        session, value = fixture.remote_active
        assert self.headers.get("Mcp-Session-Id") == session, self.headers
        assert self.headers.get("X-Generation") == value, self.headers
        return fixture, value

    def do_POST(self):
        fixture = self.server.fixture
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if request["method"] == "initialize":
            assert fixture.remote_active is None, "replacement opened before old session deletion"
            value = self.headers["X-Generation"]
            fixture.remote_active = ("session-" + value, value)
            fixture.remote_events.append(("initialize", value))
            result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}},
                      "serverInfo": {"name": "remote", "version": value}}
        else:
            _, value = self.check()
            if "id" not in request:
                self.send_response(202)
                self.end_headers()
                return
            if request["method"] == "tools/list":
                result = {"tools": [{"name": "probe", "inputSchema": {"type": "object"},
                                     "outputSchema": {"type": "object", "properties": {"generation": {"const": value}}}}]}
            else:
                assert request["method"] == "tools/call", request
                fixture.remote_events.append(("call", value))
                result = {"content": [], "structuredContent": {"generation": value}}
        payload = json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Mcp-Session-Id", fixture.remote_active[0])
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.check()
        self.send_response(405)
        self.end_headers()

    def do_DELETE(self):
        fixture, value = self.check()
        fixture.remote_events.append(("delete", value))
        fixture.remote_active = None
        self.send_response(200)
        self.end_headers()

    def log_message(self, *_):
        pass


class Fixture:
    def __init__(self, root, http=False):
        self.root = root
        self.workspace = root / "workspace"
        self.workspace.mkdir()
        self.env = {key: os.environ[key] for key in ("PATH", "SHELL", "TERM", "COLORTERM") if key in os.environ}
        self.env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"), TMPDIR=str(root / "tmp"))
        for name in ("CONFIG", "CACHE", "DATA", "STATE"):
            self.env[f"XDG_{name}_HOME"] = str(root / name.lower())
        for key in ("HOME", "ION_HOME", "TMPDIR"):
            Path(self.env[key]).mkdir()
        self.failures, self.requests = [], []
        self.remote_active, self.remote_events = None, []
        self.remote = ThreadingHTTPServer(("127.0.0.1", 0), Remote) if http else None
        if self.remote:
            self.remote.fixture = self
            self.remote_thread = threading.Thread(target=self.remote.serve_forever)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        self.server.fixture = self
        self.thread = threading.Thread(target=self.server.serve_forever)
        self.value("one")
        self.save()
        try:
            subprocess.run([BINARY, "use", "fixture", "reconnect", "--wire", "chat-completions", "--endpoint",
                            f"http://127.0.0.1:{self.server.server_port}/v1"], env=self.env,
                           check=True, capture_output=True, timeout=15)
        except BaseException:
            self.server.server_close()
            if self.remote:
                self.remote.server_close()
            raise
        self.thread.start()
        if self.remote:
            self.remote_thread.start()

    def value(self, value):
        (self.workspace / "value").write_text(value)

    def save(self, steady=True):
        helper = "from pathlib import Path\nwith open('helpers','a') as out: out.write('capture\\n')\nprint(Path('value').read_text())"
        servers = {"peer": {"command": sys.executable, "args": [str(SCRIPT), "peer", "peer"],
                            "env": {"GENERATION": {"command": sys.executable, "args": ["-c", helper]}}}}
        if self.remote:
            servers["peer"] = {"url": f"http://127.0.0.1:{self.remote.server_port}/mcp",
                               "headers": {"X-Generation": {"command": sys.executable, "args": ["-c", helper]}}}
        if steady:
            servers["steady"] = {"command": sys.executable, "args": [str(SCRIPT), "peer", "steady"],
                                 "env": {"GENERATION": "stable"}}
        (Path(self.env["ION_HOME"]) / "mcp.json").write_text(json.dumps({"servers": servers}))

    def events(self, name="peer"):
        path = self.workspace / (name + "-events")
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def facts(self):
        result = {}
        for path in Path(self.env["ION_HOME"]).rglob("*.sqlite"):
            with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as db:
                result[str(path)] = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
        return result

    def close(self):
        self.server.shutdown()
        self.thread.join(timeout=5)
        self.server.server_close()
        assert not self.thread.is_alive()
        if self.remote:
            self.remote.shutdown()
            self.remote_thread.join(timeout=5)
            self.remote.server_close()
            assert not self.remote_thread.is_alive() and self.remote_active is None
        for name in ("peer", "steady"):
            for entry in self.events(name):
                if entry["event"] == "start":
                    observed = subprocess.run(["ps", "-p", str(entry["pid"]), "-o", "pid="], capture_output=True)
                    assert observed.returncode == 1 and not observed.stdout and not observed.stderr, entry
        assert not self.failures, self.failures


class Rpc:
    def __init__(self, fixture):
        self.process = subprocess.Popen([BINARY, "--cwd", fixture.workspace, "--completion-delivery", "inbox", "rpc"],
                                        env=fixture.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.queue, self.records, self.number = queue.Queue(), [], 0
        def read():
            for line in self.process.stdout:
                self.queue.put(json.loads(line))
            self.queue.put(None)
        self.reader = threading.Thread(target=read)
        self.reader.start()
        try:
            self.wait(lambda record: record["type"] == "ready")
        except BaseException:
            self.cleanup()
            raise

    def wait(self, predicate):
        for record in self.records:
            if predicate(record):
                return record
        deadline = time.monotonic() + 8
        while True:
            record = self.queue.get(timeout=max(.001, deadline - time.monotonic()))
            assert record is not None, (self.process.poll(), self.records[-5:])
            self.records.append(record)
            if predicate(record):
                return record

    def request(self, kind, **fields):
        self.number += 1
        identity = str(self.number)
        self.process.stdin.write(json.dumps({"id": identity, "type": kind, **fields}) + "\n")
        self.process.stdin.flush()
        result = self.wait(lambda record: record["type"] == "response" and record.get("id") == identity)
        assert result["success"], result
        return result

    def reconnect(self, success=True):
        ack = self.request("reconnect_mcp", server="peer")
        assert ack["data"]["disposition"] == "admitted", ack
        result = self.wait(lambda record: record["type"] == "mcp_reconnect_finished" and record.get("id") == ack["id"])
        assert result["operation"] == ack["data"]["operation"] and result["server"] == "peer", result
        assert result["success"] == success, result
        return result

    def turn(self, message, number):
        self.request("prompt", message=message)
        ended = self.wait(lambda record: record["type"] == "turn_end" and record["turn"] == number)
        assert ended["status"] == "completed", ended

    def close(self):
        if self.process.stdin:
            self.process.stdin.close()
            self.process.stdin = None
        self.process.wait(timeout=8)
        self.reader.join(timeout=5)
        assert not self.reader.is_alive()
        assert self.process.returncode == 0, self.process.stderr.read()

    def cleanup(self):
        if self.process.poll() is None:
            try:
                if self.process.stdin:
                    self.process.stdin.close()
                    self.process.stdin = None
                self.process.wait(timeout=8)
            except (subprocess.TimeoutExpired, BrokenPipeError):
                if self.process.poll() is None:
                    self.process.kill()
                self.process.wait(timeout=5)
        self.reader.join(timeout=5)
        assert not self.reader.is_alive()


def main():
    for case in ("shared", "unavailable", "http"):
        with tempfile.TemporaryDirectory(prefix="ion-mcp-reconnect-") as temporary:
            fixture = Fixture(Path(temporary), http=case == "http")
            rpc = None
            try:
                if case == "unavailable":
                    fixture.value("")
                    fixture.save(steady=False)
                rpc = Rpc(fixture)
                if case == "unavailable":
                    before = fixture.facts()
                    assert not fixture.events() and not fixture.requests
                    fixture.value("recovered")
                    rpc.reconnect()
                    assert fixture.facts() == before, "maintenance must not create Core facts"
                elif case == "http":
                    rpc.turn("PROBE-one", 1)
                    fixture.value("two")
                    rpc.reconnect()
                    rpc.turn("PROBE-two", 2)
                    rpc.close()
                    assert fixture.remote_events == [("initialize", "one"), ("call", "one"), ("delete", "one"),
                                                     ("initialize", "two"), ("call", "two"), ("delete", "two")], fixture.remote_events
                    assert (fixture.workspace / "helpers").read_text().splitlines() == ["capture", "capture"]
                else:
                    rpc.request("prompt", message="PARENT")
                    until(lambda: any(event["event"] == "waiting" for event in fixture.events()), "child did not issue shared request")
                    assert rpc.request("get_state")["data"]["operation"] == "turn"
                    fixture.value("two")
                    rpc.reconnect()
                    ended = rpc.wait(lambda record: record["type"] == "turn_end" and record["turn"] == 1)
                    assert ended["status"] == "completed", ended
                    assert (fixture.workspace / "child-survived").read_text() == "CHILD_CONTINUED"
                    rpc.turn("PROBE-two", 2)
                    fixture.value("")
                    rpc.reconnect(success=False)
                    rpc.turn("PROBE-unavailable", 3)
                    fixture.value("three")
                    rpc.reconnect()
                    rpc.turn("PROBE-three", 4)
                    events = fixture.events()
                    assert [(e["generation"], e["event"]) for e in events if e["event"] == "call"] == [("one", "call"), ("two", "call"), ("three", "call")], events
                    assert next(i for i, e in enumerate(events) if e["event"] == "exit" and e["generation"] == "one") < next(i for i, e in enumerate(events) if e["event"] == "start" and e["generation"] == "two"), events
                    assert len([e for e in fixture.events("steady") if e["event"] == "start"]) == 1
                    assert all(entry["data"]["reason"] == "completed" for entries in fixture.facts().values() for entry in entries if entry["kind"] == "turn_ended")
                    assert any("children" in path for path in fixture.facts()), "no real managed-child Session"
                    # EOF must join an admitted replacement still in initialization.
                    fixture.value("hang")
                    ack = rpc.request("reconnect_mcp", server="peer")
                    until(lambda: any(e["event"] == "waiting" and e["generation"] == "hang" for e in fixture.events()), "replacement did not enter initialization")
                    rpc.close()
                    finished = rpc.wait(lambda record: record["type"] == "mcp_reconnect_finished" and record.get("id") == ack["id"])
                    assert not finished["success"] and finished["stop_requested"], finished
                rpc.close()
            finally:
                if rpc is not None:
                    rpc.cleanup()
                fixture.close()
    print("MCP reconnect: shared child interrupted without cancelling Turns/replay; generations, failure/retry, unchanged peer, HTTP deletion/credential recapture, startup recovery and joined EOF verified: OK")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "peer":
        peer()
    else:
        main()
