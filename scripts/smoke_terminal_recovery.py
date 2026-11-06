"""Resume interrupted coding work with an ordinary message, never automatic replay."""

import errno
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

from terminal_style import program_ready


ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion")).resolve()
PROMPT = "Apply one effect and wait."
FOLLOWUP = "Continue — inspect café 🦀 effect before changing anything."
ANSWER = "CONTINUED_WITH_CURRENT_RUNTIME"
HINT = b"Send a message to continue"


def wait_for(predicate, detail=lambda: "condition timed out"):
    deadline = time.monotonic() + 12
    while not predicate():
        assert time.monotonic() < deadline, detail()
        time.sleep(0.02)


class Case:
    def __init__(self, root):
        self.root = root
        self.workspace = root / "workspace"
        self.workspace.mkdir()
        (root / "home").mkdir()
        (root / "tmp").mkdir()
        self.env = {name: os.environ[name] for name in ("PATH",) if name in os.environ}
        self.env.update(
            HOME=str(root / "home"), ION_HOME=str(root / "ion"),
            XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
            TMPDIR=str(root / "tmp"), TERM="xterm-256color", LANG="C.UTF-8",
        )
        self.requests = {"before": [], "after": []}
        self.errors = []
        self.worker_pid = None
        self.worker_stopped = False
        self.child = None
        self.session = None

    def run(self, *args):
        return subprocess.run(
            [str(BINARY), *map(str, args)], cwd=self.workspace,
            env=self.env, check=True, capture_output=True, timeout=12,
        )

    def use(self, server):
        self.run("use", "smoke", "recovery-model", "--endpoint",
                 f"http://127.0.0.1:{server.server_port}/v1", "--wire", "chat-completions")

    def inspect(self):
        return json.loads(self.run("--session", self.session, "inspect").stdout)

    def seed(self, server):
        self.use(server)
        with (self.root / "crashed.log").open("wb") as log:
            self.child = subprocess.Popen(
                [str(BINARY), "--cwd", str(self.workspace), "--tools", "exec,read", "run", PROMPT],
                env=self.env, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            )
        wait_for(lambda: (self.workspace / "ready").exists(),
                 lambda: (self.child.poll(), (self.root / "crashed.log").read_text()))
        self.worker_pid = int((self.workspace / "worker.pid").read_text())
        assert os.getpgid(self.worker_pid) == self.worker_pid
        assert (self.workspace / "effects").read_text() == "effect\n"
        self.child.kill()
        self.child.wait(timeout=5)
        # Ion's SIGKILL cannot establish descendant quiescence. Terminate our
        # orphan explicitly before acquiring the Session again.
        self.stop_worker()
        sessions = list((self.root / "ion").rglob("*.sqlite"))
        assert len(sessions) == 1, sessions
        self.session = sessions[0]
        self.before = self.inspect()
        assert self.before["unfinished_turn"] == 1, self.before
        assert not any(entry["kind"] == "tool_result" for entry in self.before["entries"])
        assert len(self.requests["before"]) == 1, self.requests

    def stop_worker(self):
        if self.worker_stopped:
            return
        if self.worker_pid is None and (self.workspace / "worker.pid").exists():
            self.worker_pid = int((self.workspace / "worker.pid").read_text())
        if self.worker_pid is None:
            return
        try:
            assert os.getpgid(self.worker_pid) == self.worker_pid
            os.killpg(self.worker_pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

        def stopped():
            state = subprocess.run(["ps", "-o", "stat=", "-p", str(self.worker_pid)],
                                   capture_output=True, check=False, timeout=5)
            # An orphan cannot be waitpid-joined here. A zombie is quiescent;
            # the host's reaper owns its final collection.
            return not state.stdout.strip() or state.stdout.strip().startswith(b"Z")

        wait_for(stopped, lambda: f"owned worker {self.worker_pid} remains active")
        self.worker_stopped = True

    def check_passive(self):
        assert not self.errors, self.errors
        assert self.inspect()["entries"] == self.before["entries"], "reopen changed the journal"
        assert not self.requests["after"], "reopen dispatched a model request"
        assert len(self.requests["before"]) == 1, "reopen used the historical route"
        assert (self.workspace / "effects").read_text() == "effect\n", "reopen replayed the effect"

    def check_finished(self):
        assert not self.errors, self.errors
        assert len(self.requests["before"]) == 1 and len(self.requests["after"]) == 2, self.requests
        assert (self.workspace / "effects").read_text() == "effect\n", "continuation replayed the effect"
        view = self.inspect()
        entries = view["entries"]
        assert entries[:len(self.before["entries"])] == self.before["entries"]
        assert view["unfinished_turn"] is None, view
        starts = [entry["data"] for entry in entries if entry["kind"] == "turn_started"]
        assert [entry["turn"] for entry in starts] == [1, 2], starts
        assert starts[1]["input"]["content"] == [{"Text": FOLLOWUP}], starts[1]
        ends = [entry["data"] for entry in entries if entry["kind"] == "turn_ended"]
        assert [(entry["turn"], entry["reason"]) for entry in ends] == [(1, "interrupted"), (2, "completed")], ends
        results = [entry["data"]["result"] for entry in entries if entry["kind"] == "tool_result"]
        assert [(result["call_id"], result["outcome"]["state"]) for result in results] == [
            ("old-effect", "unknown"), ("fresh-observation", "observed"),
        ], results
        observed = results[1]["outcome"]["output"]
        assert observed["is_error"] is False and observed["value"]["content"] == "effect\n", observed
        assert not any(entry["kind"] == "turn_resumed" for entry in entries)
        return view

    def handler(self, route):
        case = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                case.requests[route].append(body)
                try:
                    count = len(case.requests[route])
                    if route == "before":
                        assert count == 1, "historical model request replayed"
                        delta = self.call("old-effect", "exec", {
                            "command": "printf 'effect\\n' >> effects; printf '%s' $$ > worker.pid; touch ready; exec sleep 90",
                        })
                    else:
                        assert [message["content"] for message in body["messages"] if message["role"] == "user"] == [PROMPT, FOLLOWUP], body
                        old = [message for message in body["messages"] if message["role"] == "tool" and message["tool_call_id"] == "old-effect"]
                        assert len(old) == 1 and "unknown" in old[0]["content"].lower(), old
                        tools = {tool["function"]["name"] for tool in body["tools"]}
                        assert "read" in tools and "exec" not in tools, tools
                        if count == 1:
                            delta = self.call("fresh-observation", "read", {"path": "effects"})
                        else:
                            assert count == 2, "unexpected continuation request"
                            result = body["messages"][-1]
                            assert result["role"] == "tool" and result["tool_call_id"] == "fresh-observation", result
                            envelope = json.loads(result["content"])
                            assert envelope["is_error"] is False and envelope["result"]["content"] == "effect\n", envelope
                            delta = {"content": ANSWER}
                    events = [
                        {"id": "recovery", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                        {"id": "recovery", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if "tool_calls" in delta else "stop"}]},
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

            @staticmethod
            def call(call_id, name, arguments):
                return {"tool_calls": [{"index": 0, "id": call_id, "type": "function", "function": {
                    "name": name, "arguments": json.dumps(arguments),
                }}]}

        return Handler


@contextmanager
def interrupted_case():
    with tempfile.TemporaryDirectory(prefix="ion-terminal-recovery-") as temporary:
        case = Case(Path(temporary))
        servers = [HTTPServer(("127.0.0.1", 0), case.handler(route)) for route in ("before", "after")]
        threads = [threading.Thread(target=server.serve_forever) for server in servers]
        for thread in threads:
            thread.start()
        try:
            case.seed(servers[0])
            # Same logical model, changed route and a read-only runtime. Ordinary
            # new input uses today's runtime; it does not reconstruct old grants.
            case.use(servers[1])
            yield case
        finally:
            try:
                if case.child is not None and case.child.poll() is None:
                    case.child.kill()
                    case.child.wait(timeout=5)
                case.stop_worker()
            finally:
                for server, thread in zip(servers, threads):
                    server.shutdown()
                    server.server_close()
                    thread.join(timeout=5)
                    assert not thread.is_alive()


def qualify(mode):
    with interrupted_case() as case:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
        before_terminal = termios.tcgetattr(slave)

        def attach():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        # Exercise both direct startup and the interactive saved-Session picker.
        selection = ["--session", str(case.session)] if mode == "inline" else ["--resume"]
        child = subprocess.Popen(
            [str(BINARY), "--cwd", str(case.workspace), "--read-only", "--tui-mode", mode, *selection, "chat"],
            env=case.env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach,
        )
        output = bytearray()
        pending = b""

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
            deadline = time.monotonic() + 12
            while not predicate():
                assert child.poll() is None and time.monotonic() < deadline, (mode, case.errors, output[-4000:])
                collect()

        try:
            if mode == "fullscreen":
                wait(lambda: b"Resume session" in output and PROMPT.encode() in output and program_ready(output))
                os.write(master, case.before["id"].encode())
                wait(lambda: b"1 match(es)" in output)
                os.write(master, b"\r")
            wait(lambda: HINT in output and program_ready(output))
            case.check_passive()
            output.clear()
            os.write(master, b"\x1b[200~" + FOLLOWUP.encode() + b"\x1b[201~\r")
            wait(lambda: ANSWER.encode() in output and program_ready(output))
            case.check_finished()
            os.write(master, b"/exit\r")
            deadline = time.monotonic() + 8
            while child.poll() is None:
                assert time.monotonic() < deadline, output[-4000:]
                collect()
            assert child.returncode == 0, output[-4000:]
            after_terminal = termios.tcgetattr(master)
            # macOS PENDIN is dynamic kernel input state, not configuration.
            if os.uname().sysname == "Darwin":
                for attributes in (before_terminal, after_terminal):
                    attributes[3] &= ~0x20000000
            assert after_terminal == before_terminal, (before_terminal, after_terminal)
            print(f"Ion {mode} passive interrupted Session / fresh current-runtime input / unknown / no replay: OK")
        finally:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)
            os.close(slave)
            os.close(master)


if __name__ == "__main__":
    for mode in ("inline", "fullscreen"):
        qualify(mode)
