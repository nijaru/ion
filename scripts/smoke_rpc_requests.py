"""Built RPC durable text admission, redelivery and actual process-loss checks."""

import json
import os
import select
import sqlite3
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []
gates = {text: threading.Event() for text in ("/check literal", "CUT_1", "CUT_2")}


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        users = [message for message in body["messages"] if message["role"] == "user"]
        content = users[-1]["content"]
        text = content if isinstance(content, str) else "".join(part.get("text", "") for part in content)
        requests.append(text)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        if text in gates and not gates[text].wait(15):
            return
        try:
            event = {"id": "request-smoke", "choices": [{"index": 0, "delta": {"content": "REQUEST_OK"}, "finish_reason": "stop"}]}
            self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\ndata: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_):
        pass


def facts(path):
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
        return [json.loads(body) for body, in db.execute("SELECT body FROM entries ORDER BY seq")]


def wait_for(predicate):
    deadline = time.monotonic() + 8
    while not predicate():
        assert time.monotonic() < deadline, "RPC admission/model boundary timed out"
        time.sleep(.01)


class Client:
    def __init__(self, workspace, env, state, session=None):
        before = set(state.rglob("*.sqlite"))
        args = [str(binary), "--cwd", str(workspace)]
        if session is not None:
            args += ["--session", str(session)]
        self.process = subprocess.Popen(args + ["rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        self.records = []
        try:
            assert self.read()["type"] == "ready"
            if session is None:
                paths = set(state.rglob("*.sqlite")) - before
                assert len(paths) == 1, paths
                self.path = paths.pop()
            else:
                self.path = session
        except BaseException:
            self.kill()
            raise

    def read(self):
        assert select.select([self.process.stdout], [], [], 8)[0], "RPC output timed out"
        line = self.process.stdout.readline()
        assert line, f"RPC closed unexpectedly: {self.process.poll()}"
        record = json.loads(line)
        self.records.append(record)
        return record

    def send(self, value):
        self.process.stdin.write(json.dumps(value).encode() + b"\n")
        self.process.stdin.flush()

    def until(self, predicate):
        deadline = time.monotonic() + 8
        while True:
            assert time.monotonic() < deadline, "RPC record boundary timed out"
            record = self.read()
            if predicate(record):
                return record

    def command(self, kind, id, **fields):
        self.send({"type": kind, "id": id, **fields})
        return self.until(lambda record: record.get("type") == "response" and record.get("id") == id)

    def close(self):
        self.process.stdin.close()
        # Drain while joining so publication is not mistaken for execution loss.
        deadline = time.monotonic() + 8
        while True:
            assert time.monotonic() < deadline, "RPC EOF shutdown timed out"
            assert select.select([self.process.stdout], [], [], 8)[0], "RPC EOF output timed out"
            line = self.process.stdout.readline()
            if not line:
                break
            self.records.append(json.loads(line))
        assert self.process.wait(timeout=8) == 0, self.process.stderr.read()
        self.process.stdout.close()
        self.process.stderr.close()

    def kill(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=8)
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            if not pipe.closed:
                pipe.close()


with tempfile.TemporaryDirectory(prefix="ion-rpc-requests-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    prompts = workspace / ".ion/prompts"
    prompts.mkdir(parents=True)
    (workspace / ".git").mkdir()
    (prompts / "check.md").write_text("EXPANDED $1\n")
    env = {name: os.environ[name] for name in ("PATH", "SHELL", "TMPDIR") if name in os.environ}
    state = work / "ion"
    env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(state))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    clients = []
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "rpc-requests", "request-model", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        key = {"scope": "offline-client", "id": "literal"}
        client = Client(workspace, env, state)
        clients.append(client)
        assert client.command("get_request", "missing", key=key)["data"] == {"receipt": None}
        first = client.command("submit_request", "first", key=key, message="/check literal")
        assert first["success"] and first["data"]["disposition"] == "started", first
        receipt = first["data"]["receipt"]
        assert receipt["turn"] == 1 and receipt["end"] is None and receipt["key"] == key
        wait_for(lambda: requests == ["/check literal"])
        before = facts(client.path)
        for id in ("duplicate", "duplicate-again"):
            duplicate = client.command("submit_request", id, key=key, message="/check literal")
            assert duplicate["data"] == {"disposition": "existing", "receipt": receipt}, duplicate
        assert not client.command("submit_request", "conflict", key=key, message="changed")["success"]
        assert client.command("get_request", "pending", key=key)["data"]["receipt"] == receipt
        assert facts(client.path) == before and requests == ["/check literal"]
        gates["/check literal"].set()
        ended = client.until(lambda record: record.get("type") == "turn_end")
        assert ended["turn"] == 1 and ended["status"] == "completed", ended
        settled = client.command("get_request", "settled", key=key)["data"]["receipt"]
        assert settled == dict(receipt, end="completed"), settled
        path = client.path
        client.close()
        assert sum(record.get("type") == "response" and record.get("id") == "first" for record in client.records) == 1
        reopened = Client(workspace, env, state, path)
        clients.append(reopened)
        before = facts(path)
        again = reopened.command("submit_request", "new-wire-id", key=key, message="/check literal")
        assert again["data"] == {"disposition": "existing", "receipt": settled}, again
        assert facts(path) == before and requests == ["/check literal"]
        reopened.close()

        # Real SIGKILL after committed admission, before any assistant response.
        # One caller reads its acknowledgement; the other loses it unread.
        for index, read_ack in enumerate((True, False), 1):
            text = f"CUT_{index}"
            key = {"scope": "offline-client", "id": text}
            client = Client(workspace, env, state)
            clients.append(client)
            client.send({"type": "submit_request", "id": "lost-wire-id", "key": key, "message": text})
            if read_ack:
                ack = client.until(lambda record: record.get("id") == "lost-wire-id")
                assert ack["success"] and ack["data"]["disposition"] == "started", ack
            wait_for(lambda: text in requests)
            before = facts(client.path)
            starts = [entry for entry in before if entry["kind"] == "turn_started"]
            assert len(starts) == 1 and starts[0]["data"]["request"] == key
            assert not any(entry["kind"] in ("assistant", "turn_ended") for entry in before)
            assert "lost-wire-id" not in json.dumps(before), "wire correlation entered durable facts"
            client.kill()
            gates[text].set()
            count = len(requests)
            reopened = Client(workspace, env, state, client.path)
            clients.append(reopened)
            assert facts(client.path) == before, "opening repaired history"
            receipt = reopened.command("get_request", "lookup", key=key)["data"]["receipt"]
            assert receipt["turn"] == 1 and receipt["end"] is None
            retry = reopened.command("submit_request", "redelivery", key=key, message=text)
            assert retry["data"] == {"disposition": "existing", "receipt": receipt}, retry
            assert not reopened.command("submit_request", "conflict", key=key, message="different")["success"]
            assert facts(client.path) == before and len(requests) == count, "redelivery restarted work"
            new = reopened.command("submit_request", "fresh", key={"scope": "offline-client", "id": "new"}, message="fresh explicit work")
            assert new["data"]["disposition"] == "started" and new["data"]["receipt"]["turn"] == 2, new
            ended = reopened.until(lambda record: record.get("type") == "turn_end")
            assert ended["turn"] == 2 and ended["status"] == "completed", ended
            old = reopened.command("get_request", "old-status", key=key)["data"]["receipt"]
            assert old == dict(receipt, end="interrupted"), old
            after = facts(client.path)
            assert after[:len(before)] == before
            repair = next(i for i, entry in enumerate(after) if entry["kind"] == "turn_ended" and entry["data"]["turn"] == 1)
            admission = next(i for i, entry in enumerate(after) if entry["kind"] == "turn_started" and entry["data"]["turn"] == 2)
            assert repair < admission and len(requests) == count + 1
            reopened.close()
    finally:
        for gate in gates.values():
            gate.set()
        for client in clients:
            client.kill()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()

print("RPC request smoke passed: literal admission, passive redelivery/status and read/unread-ack SIGKILL recovery")
