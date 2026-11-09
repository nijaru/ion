"""Real native PTY attachment, paste, input custody and process lifecycle checks."""

import errno
import fcntl
import json
import os
import pty
import select
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

from terminal_style import painted_text, program_ready

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion")).resolve()
PAYLOAD = ("x" * 16383 + "🦀\ncafé " + "y" * 16384).encode()
OPEN, CLOSE = b"\x1b[200~", b"\x1b[201~"
PROGRAM = '''import os, sys, tty
from pathlib import Path
name = sys.argv[1]
tty.setraw(0)
Path(name + ".pid").write_text(str(os.getpid()))
os.write(1, b"\\x1b[?2004hATTACH_READY\\r\\n")
Path(name + ".ready").touch()
wire = bytearray()
while True:
    data = os.read(0, 65536)
    if not data:
        break
    wire.extend(data)
    if b"!" in data:
        snapshot = Path(name + ".wire.tmp")
        snapshot.write_bytes(wire)
        snapshot.replace(name + ".wire")
        os.write(1, b"INPUT_SAVED\\r\\n")
    if b"m" in data:
        Path(name + ".model").write_bytes(data)
        os.write(1, b"MODEL_INPUT_RECEIVED\\r\\n")
    if b"\\x03" in data:
        Path(name + ".interrupt").write_bytes(b"\\x03")
    if b"q" in data:
        Path(name + ".ended").touch()
        break
'''


def until(predicate, detail=lambda: "condition timed out"):
    deadline = time.monotonic() + 15
    while not predicate():
        assert time.monotonic() < deadline, detail()
        time.sleep(0.02)


class Case:
    def __init__(self, root):
        self.root = root
        self.workspace = root / "workspace"
        (self.workspace / ".git").mkdir(parents=True)
        (root / "home").mkdir()
        (root / "tmp").mkdir()
        self.env = {name: os.environ[name] for name in ("PATH",) if name in os.environ}
        self.env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"),
                        XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                        TMPDIR=str(root / "tmp"), TERM="xterm-256color", LANG="C.UTF-8")
        self.program = root / "program.py"
        self.program.write_text(PROGRAM)
        self.requests, self.errors, self.receipts = [], [], []
        self.handle = None

    def run(self, *args):
        return subprocess.run([str(BINARY), *map(str, args)], cwd=self.workspace,
                              env=self.env, check=True, capture_output=True, timeout=15)

    def command(self, name):
        return shlex.join([sys.executable, "-u", str(self.program), name])

    def handler(self):
        case = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                step = len(case.requests)
                case.requests.append(body)
                try:
                    assert {tool["function"]["name"] for tool in body["tools"]} == {"process"}
                    result = None
                    if body["messages"][-1]["role"] == "tool":
                        envelope = json.loads(body["messages"][-1]["content"])
                        assert not envelope["is_error"], envelope
                        result = envelope["result"]
                        case.receipts.append(result)
                    action = None
                    if step in (0, 8):
                        name = "natural" if step == 0 else "stopped"
                        action = {"action": "start", "backend": "pty", "command": case.command(name)}
                    elif step in (1, 9):
                        assert result["backend"] == "pty" and result["running"], result
                        case.handle = result["id"]
                        name = "natural" if step == 1 else "stopped"
                        until(lambda: (case.workspace / (name + ".ready")).exists())
                        delta = {"content": "STARTED_FOR_ATTACH" if step == 1 else "STARTED_FOR_STOP"}
                    elif step == 2:
                        action = {"action": "inspect"}
                    elif step == 3:
                        assert result["running"], result
                        action = {"action": "write", "data": "m"}
                    elif step == 4:
                        assert result["bytes_written"] == 1 and not result.get("cancelled"), result
                        delta = {"content": "MODEL_CUSTODY_RESTORED"}
                    elif step == 5:
                        action = {"action": "wait", "timeout_ms": 3000}
                    elif step in (6, 11):
                        assert result["joined"] and not result["running"], result
                        if step == 6:
                            assert result["exit"]["exit_code"] == 0, result
                        action = {"action": "acknowledge"}
                    elif step in (7, 12):
                        assert result["acknowledged"], result
                        delta = {"content": "NATURAL_JOINED" if step == 7 else "STOPPED_AND_JOINED"}
                    else:
                        assert step == 10, (step, body)
                        action = {"action": "close"}
                    if action:
                        if action["action"] != "start":
                            action["id"] = case.handle
                        delta = {"tool_calls": [{"index": 0, "id": f"process-{step}", "type": "function",
                                                 "function": {"name": "process", "arguments": json.dumps(action)}}]}
                    events = [
                        {"id": "process", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                        {"id": "process", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if action else "stop"}]},
                    ]
                    payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except Exception as error:
                    case.errors.append(repr(error))
                    self.send_error(500, str(error))

        return Handler

    def check_finished(self):
        assert not self.errors, self.errors
        assert len(self.requests) == 13, len(self.requests)
        assert (self.workspace / "natural.model").read_bytes() == b"m"
        assert (self.workspace / "natural.interrupt").read_bytes() == b"\x03"
        assert (self.workspace / "natural.ended").exists()
        assert not (self.workspace / "stopped.ended").exists(), "stop did not use the owner lifecycle"
        for name in ("natural", "stopped"):
            pid = int((self.workspace / (name + ".pid")).read_text())
            status = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, timeout=5)
            assert not status.stdout.strip(), (name, pid, status.stdout)
        sessions = list((self.root / "ion").rglob("*.sqlite"))
        assert len(sessions) == 1, sessions
        view = json.loads(self.run("--session", sessions[0], "inspect").stdout)
        assert view["unfinished_turn"] is None, view
        ends = [entry["data"] for entry in view["entries"] if entry["kind"] == "turn_ended"]
        assert len(ends) == 5 and all(entry["reason"] == "completed" for entry in ends), ends
        results = [entry["data"]["result"] for entry in view["entries"] if entry["kind"] == "tool_result"]
        assert len(results) == 8 and all(result["outcome"]["state"] == "observed" for result in results), results
        return view


@contextmanager
def process_case():
    with tempfile.TemporaryDirectory(prefix="ion-process-terminal-") as temporary:
        case = Case(Path(temporary))
        server = HTTPServer(("127.0.0.1", 0), case.handler())
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            case.run("use", "smoke", "process-model", "--endpoint",
                     f"http://127.0.0.1:{server.server_port}/v1", "--wire", "chat-completions")
            yield case
        finally:
            # The caller first stops/joins Ion. Only our fixture PIDs are eligible
            # for fallback cleanup; never infer process identity from a tool UUID.
            try:
                for path in case.workspace.glob("*.pid"):
                    pid = int(path.read_text())
                    command = subprocess.run(["ps", "-o", "command=", "-p", str(pid)],
                                             capture_output=True, timeout=5).stdout
                    if str(case.program).encode() not in command:
                        continue
                    try:
                        assert os.getpgid(pid) == pid
                        os.killpg(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    def stopped():
                        state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                               capture_output=True, timeout=5).stdout.strip()
                        return not state or state.startswith(b"Z")
                    until(stopped, lambda: f"fixture {pid} remains active")
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)
                assert not thread.is_alive()


def qualify(mode):
    with process_case() as case:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
        before = termios.tcgetattr(slave)

        def attach():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen([str(BINARY), "--cwd", str(case.workspace), "--tools", "process",
                                  "--tui-mode", mode, "chat"], env=case.env,
                                 stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
        output, pending = bytearray(), b""

        def collect():
            nonlocal pending
            if not select.select([master], [], [], 0.05)[0]:
                return
            try:
                data = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                return
            output.extend(data)
            pending += data
            while b"\x1b[6n" in pending:
                pending = pending.split(b"\x1b[6n", 1)[1]
                os.write(master, b"\x1b[2;1R")
            pending = pending[-8:]

        def wait(predicate):
            deadline = time.monotonic() + 15
            while not predicate():
                assert child.poll() is None and time.monotonic() < deadline, (mode, case.errors, output[-4000:])
                collect()

        def message(text, marker):
            output.clear()
            os.write(master, text.encode() + b"\r")
            wait(lambda: marker in output and program_ready(output))

        try:
            wait(lambda: program_ready(output))
            message("Start an interactive program.", b"STARTED_FOR_ATTACH")
            output.clear()
            os.write(master, b"/attach\r")
            wait(lambda: b"ATTACH_READY" in output and b"Ctrl-] detach" in output)
            os.write(master, OPEN + PAYLOAD + CLOSE + b"!")
            wait(lambda: (case.workspace / "natural.wire").exists())
            actual = (case.workspace / "natural.wire").read_bytes()
            failures = []
            if actual != OPEN + PAYLOAD + CLOSE + b"!":
                failures.append(f"paste changed: {actual.count(OPEN)} opening / {actual.count(CLOSE)} closing frames; {len(actual)} bytes")
            output.clear()
            os.write(master, b"\x1d" if mode == "inline" else b"\x1b[93;5u")
            wait(lambda: b"Detached;" in output and "› " in painted_text(output))
            assert len(case.requests) == 2, "human attachment dispatched model work"
            message("Inspect and write to the detached program.", b"MODEL_CUSTODY_RESTORED")
            wait(lambda: (case.workspace / "natural.model").exists())
            output.clear()
            os.write(master, b"/attach\r")
            wait(lambda: b"MODEL_INPUT_RECEIVED" in output and b"Ctrl-] detach" in output)
            os.write(master, b"\x03")
            wait(lambda: (case.workspace / "natural.interrupt").exists())
            output.clear()
            os.write(master, b"q")
            wait(lambda: (b"Program ended" in output or b"Detached;" in output) and "› " in painted_text(output))
            if b"Program ended" not in output or b"keeps running" in output:
                failures.append("ended program reported as detached / still running")
            message("Wait and acknowledge the ended program.", b"NATURAL_JOINED")
            message("Start another interactive program.", b"STARTED_FOR_STOP")
            message("Stop, join and acknowledge that program.", b"STOPPED_AND_JOINED")
            os.write(master, b"/exit\r")
            deadline = time.monotonic() + 10
            while child.poll() is None:
                assert time.monotonic() < deadline, output[-4000:]
                collect()
            assert child.returncode == 0, output[-4000:]
            after = termios.tcgetattr(master)
            if os.uname().sysname == "Darwin":
                for attrs in (before, after):
                    attrs[3] &= ~0x20000000  # Dynamic PENDIN, not terminal configuration.
            assert after == before, (before, after)
            case.check_finished()
            assert not failures, (mode, failures)
            print(f"Ion {mode} native process: one Unicode paste / detach / model custody / exit / stop / join: OK")
        finally:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)
            os.close(slave)
            os.close(master)


if __name__ == "__main__":
    for mode in ("inline", "fullscreen"):
        qualify(mode)
