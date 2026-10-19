"""Headless SIGINT must settle Core while stdout or stderr is backpressured."""

import array
import fcntl
import json
import os
import select
import signal
import sqlite3
import subprocess
import tempfile
import termios
import threading
import time
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        prompt = str(body["messages"][-1]["content"])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def event(delta, reason=None):
            data = {"id": "headless", "choices": [{"index": 0, "delta": delta, "finish_reason": reason}]}
            self.wfile.write(b"data: " + json.dumps(data).encode() + b"\n\n")
            self.wfile.flush()

        try:
            if "BURST" in prompt:
                deltas = [{"content": "x"}] * 5000 + [{}]
                records = [b"data: " + json.dumps({"id": "burst", "choices": [{"index": 0, "delta": delta, "finish_reason": "stop" if index == 5000 else None}]}).encode() + b"\n\n" for index, delta in enumerate(deltas)]
                self.wfile.write(b"".join(records) + b"data: [DONE]\n\n")
                self.wfile.flush()
            elif "TEXT_PRESSURE" in prompt:
                commands = [
                    "printf 'ONCE\\n' >> effects; python3 -c \"print('x' * 100000)\"",
                    "printf '%s\\n' $$ > native.pid; touch native.ready; exec sleep 30",
                    "touch unused",
                ]
                calls = [{"index": index, "id": f"call-{index}", "type": "function", "function": {"name": "exec", "arguments": json.dumps({"command": command, "timeout_ms": 120000})}} for index, command in enumerate(commands)]
                event({"tool_calls": calls}, "tool_calls")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            else:
                # Small valid model deltas create enough JSONL framing to fill
                # a pipe without relying on an oversized model answer.
                for _ in range(5000):
                    event({"content": "x"})
                    time.sleep(0.001)
                event({}, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_args):
        pass


def available(pipe):
    count = array.array("i", [0])
    fcntl.ioctl(pipe.fileno(), termios.FIONREAD, count, True)
    return count[0]


def entries(database):
    with closing(sqlite3.connect(f"file:{database}?mode=ro", uri=True)) as connection:
        return [json.loads(row[0]) for row in connection.execute("SELECT body FROM entries ORDER BY seq")]


def session_database(state, workspace):
    matches = []
    for path in state.rglob("*.sqlite"):
        with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as connection:
            header = json.loads(connection.execute("SELECT header FROM session WHERE id=1").fetchone()[0])
        if Path(header["cwd"]).resolve() == workspace.resolve():
            matches.append(path)
    assert len(matches) == 1, (workspace, matches)
    return matches[0]


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def cleanup(child, workspace):
    if child.poll() is None:
        child.kill()
        child.communicate(timeout=8)
    pid_path = workspace / "native.pid"
    if pid_path.exists():
        pid = int(pid_path.read_text())
        if alive(pid):
            os.kill(pid, signal.SIGKILL)


with tempfile.TemporaryDirectory(prefix="ion-headless-") as temporary:
    work = Path(temporary)
    env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
    env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "headless-smoke", "model", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        burst_workspace = work / "burst"
        burst_workspace.mkdir()
        burst = subprocess.run([binary, "--cwd", burst_workspace, "--json", "run", "BURST"], env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=15)
        assert burst.returncode == 0, burst.stderr
        burst_records = [json.loads(line) for line in burst.stdout.splitlines()]
        assert burst_records[-1] == {"type": "run_end", "status": "completed"}, burst_records[-3:]
        visible = "".join(record["text"] for record in burst_records if record["type"] == "text_delta")
        gaps = [record for record in burst_records if record["type"] == "observation_gap"]
        # Provisional progress is bounded and may overflow even with a draining
        # reader if scheduling lets a burst get ahead. Loss must be explicit;
        # the committed final below must still be complete and exact.
        assert len(visible) <= 5000 and visible == "x" * len(visible), (len(visible), gaps)
        if len(visible) < 5000:
            assert gaps and sum(record["skipped"] for record in gaps) > 0, (len(visible), gaps)
        assert next(record["text"] for record in burst_records if record["type"] == "final") == "x" * 5000
        for json_output in (True, False):
            workspace = work / ("json" if json_output else "text")
            workspace.mkdir()
            command = [binary, "--cwd", workspace]
            if json_output:
                command.append("--json")
            child = subprocess.Popen(command + ["run", "JSON_PRESSURE" if json_output else "TEXT_PRESSURE"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            native_pid = None
            writer_probe = None
            try:
                pipe = child.stdout if json_output else child.stderr
                if hasattr(fcntl, "F_SETPIPE_SZ"):
                    # Change only this owned fixture pipe, not production limits.
                    fcntl.fcntl(pipe.fileno(), fcntl.F_SETPIPE_SZ, 4096)
                    # Linux pipe page/atomic-write rules can stall a writer well
                    # below byte capacity. Probe kernel writability, without
                    # writing data or consuming the recipient's buffered output.
                    descriptor = 1 if json_output else 2
                    writer_probe = os.open(f"/proc/{child.pid}/fd/{descriptor}", os.O_WRONLY | os.O_NONBLOCK | os.O_CLOEXEC)
                deadline = time.monotonic() + 8
                previous = -1
                unchanged_since = time.monotonic()
                while time.monotonic() < deadline:
                    assert child.poll() is None, "headless exited before output pressure"
                    count = available(pipe)
                    if count != previous:
                        previous = count
                        unchanged_since = time.monotonic()
                    blocked = writer_probe is None or not select.select([], [writer_probe], [], 0)[1]
                    # Plaintext stderr can fill with call-admission activity
                    # before either command starts. This case promises native
                    # cancellation, so establish the host effect and running
                    # second command before sending SIGINT, not just pipe load.
                    native_started = json_output or (workspace / "native.ready").exists()
                    if count > 0 and blocked and native_started and time.monotonic() - unchanged_since >= 0.2:
                        break
                    time.sleep(0.01)
                else:
                    raise AssertionError(f"fixture did not establish output pressure and required native work: json={json_output}, available={previous}, native_ready={(workspace / 'native.ready').exists()}")
                if not json_output:
                    # A prior cancelled Session must not become this test's
                    # oracle because a checkpoint/touch changed its file mtime.
                    previous = session_database(work / "state", work / "json")
                    stamp = time.time_ns() + 10_000_000_000
                    os.utime(previous, ns=(stamp, stamp))
                database = session_database(work / "state", workspace)
                child.send_signal(signal.SIGINT)
                deadline = time.monotonic() + 4
                facts = []
                while time.monotonic() < deadline:
                    facts = entries(database)
                    if facts and facts[-1]["kind"] == "turn_ended":
                        break
                    time.sleep(0.02)
                assert facts[-1]["kind"] == "turn_ended" and facts[-1]["data"]["reason"] == "cancelled", "output pressure prevented durable headless cancellation"
                if (workspace / "native.pid").exists():
                    native_pid = int((workspace / "native.pid").read_text())
                    assert not alive(native_pid), "headless settled before reaping owned native work"
                if writer_probe is not None:
                    os.close(writer_probe)
                    writer_probe = None
                stdout, stderr = child.communicate(timeout=8)
                assert child.returncode != 0, (stdout, stderr)
                if json_output:
                    records = [json.loads(line) for line in stdout.splitlines()]
                    assert records[0]["type"] == "session", records
                    assert records[-1]["type"] == "run_end" and records[-1]["status"] == "cancelled", records[-3:]
                    assert sum(record["type"] == "turn_ended" for record in records) == 1
                else:
                    assert stdout == b"", "headless published an uncommitted final answer"
                    assert (workspace / "effects").read_text() == "ONCE\n"
                    assert not (workspace / "unused").exists()
                    outcomes = [fact["data"]["result"]["outcome"]["state"] for fact in facts if fact["kind"] == "tool_result"]
                    assert outcomes[0] == "observed" and outcomes[-1] == "not_dispatched", outcomes
            finally:
                if writer_probe is not None:
                    os.close(writer_probe)
                cleanup(child, workspace)
        for json_output in (True, False):
            workspace = work / ("broken-json" if json_output else "broken-text")
            workspace.mkdir()
            command = [binary, "--cwd", workspace]
            if json_output:
                command.append("--json")
            child = subprocess.Popen(command + ["run", "TEXT_PRESSURE"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + 8
                while not (workspace / "native.ready").exists() and time.monotonic() < deadline:
                    assert child.poll() is None, "headless exited before native work started"
                    time.sleep(0.01)
                assert (workspace / "native.ready").exists(), "native fixture did not start"
                native_pid = int((workspace / "native.pid").read_text())
                if json_output:
                    child.stdout.close()
                    child.stdout = None
                else:
                    child.stderr.close()
                    child.stderr = None
                stdout, stderr = child.communicate(timeout=8)
                assert child.returncode == 1, (child.returncode, stdout, stderr)
                assert not alive(native_pid), "broken output dropped owned native work"
                database = session_database(work / "state", workspace)
                facts = entries(database)
                assert facts[-1]["kind"] == "turn_ended" and facts[-1]["data"]["reason"] == "cancelled", facts[-1]
                outcomes = [fact["data"]["result"]["outcome"] for fact in facts if fact["kind"] == "tool_result"]
                assert [outcome["state"] for outcome in outcomes] == ["observed", "observed", "not_dispatched"], outcomes
                assert outcomes[1]["output"]["value"]["cancelled"] is True, outcomes[1]
                assert (workspace / "effects").read_text() == "ONCE\n" and not (workspace / "unused").exists()
            finally:
                cleanup(child, workspace)
    finally:
        server.shutdown()
        server.server_close()
print("Ion headless backpressure cancellation and durable settlement: OK")
