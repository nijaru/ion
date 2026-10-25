"""Exercise draft/history and pre-admission recovery through the actual terminal."""

import errno
import fcntl
import json
import os
import pty
import re
import select
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
unexpected_connections = []
release = threading.Event()


def user_text(body):
    content = body["messages"][-1]["content"]
    return content if isinstance(content, str) else "".join(part["text"] for part in content if part["type"] == "text")


class Handler(BaseHTTPRequestHandler):
    def do_CONNECT(self):
        unexpected_connections.append(self.path)
        self.send_error(502, "Model requests are forbidden during setup")

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        last_user = next(message for message in reversed(body["messages"]) if message["role"] == "user")
        prompt = last_user["content"]
        if isinstance(prompt, list):
            prompt = "".join(part["text"] for part in prompt if part["type"] == "text")
        delta = None
        if prompt.startswith("TASK_PARENT_") or prompt.startswith("TASK_CHILD_"):
            fresh = body["messages"][-1]["role"] == "user"
            if fresh:
                if prompt.startswith("TASK_PARENT_"):
                    name, arguments = "subagent", {"action": "start", "mode": "background", "task": prompt.replace("TASK_PARENT_", "TASK_CHILD_")}
                else:
                    name, arguments = "exec", {"command": "printf '%s' $$ > task.pid; touch task.ready; while [ ! -f task.release ]; do sleep 0.01; done; printf TASK_EFFECT > task.effect", "timeout_ms": 120000}
                delta = {"tool_calls": [{"index": 0, "id": name, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
            else:
                delta = {"content": "TASK_ACCEPTED" if prompt.startswith("TASK_PARENT_") else "TASK_FINAL_🦀"}
        if prompt in ("WAIT_HISTORY", "ACCEPTED_VIEW_FAULT"):
            assert release.wait(10), "fixture did not release the waiting response"
        delta = delta or {"content": "DONE_" + prompt}
        events = [
            {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if "tool_calls" in delta else "stop"}]},
        ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass


class Terminal:
    def __init__(self, cwd, env, mode, args=None, redirected_tty=False):
        self.master, slave = pty.openpty()
        os.set_blocking(self.master, False)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def attach():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        self.original_modes = termios.tcgetattr(slave)
        self.redirected_master = None
        if redirected_tty:
            self.redirected_master, redirected_slave = pty.openpty()
            self.redirected_modes = termios.tcgetattr(redirected_slave)
        command = [binary, "--cwd", cwd, "--tui-mode", mode, "chat"] if args is None else [binary, *args]
        self.child = subprocess.Popen(command, cwd=cwd, env=env, stdin=redirected_slave if redirected_tty else slave, stdout=slave, stderr=slave, preexec_fn=attach)
        os.close(slave)
        if redirected_tty:
            os.close(redirected_slave)
        self.output = bytearray()

    def send(self, data):
        data = data.encode() if isinstance(data, str) else data
        deadline = time.monotonic() + 10
        while data:
            assert time.monotonic() < deadline and self.child.poll() is None, "terminal write stalled"
            readable, writable, _ = select.select([self.master], [self.master], [], 0.03)
            if readable:
                self.pump()
            if writable:
                try:
                    written = os.write(self.master, data[:4096])
                except BlockingIOError:
                    continue
                data = data[written:]

    def pump(self):
        if select.select([self.master], [], [], 0.03)[0]:
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                data = b""
            self.output.extend(data)
            if b"\x1b[6n" in data:
                self.send(b"\x1b[2;1R")

    def resize(self, rows, columns, settle=True):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        os.kill(self.child.pid, signal.SIGWINCH)
        if settle:
            self.paint()

    def wait(self, predicate):
        end = time.monotonic() + 10
        while not predicate():
            assert time.monotonic() < end and self.child.poll() is None, (self.child.poll(), self.output[-2500:])
            self.pump()

    def text(self):
        return re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", self.output).decode("utf-8", errors="replace")

    def paint(self):
        # Drain while the settled projection replaces the live response.
        end = time.monotonic() + 0.2
        while time.monotonic() < end:
            self.pump()

    def finish(self):
        self.send(b"\x03")
        self.wait_exit()

    def wait_exit(self):
        end = time.monotonic() + 10
        while self.child.poll() is None:
            assert time.monotonic() < end, self.output[-2500:]
            self.pump()
        assert self.child.returncode == 0, self.output[-2500:]

    def close(self):
        if self.child.poll() is None:
            self.child.kill()
        os.close(self.master)
        if self.redirected_master is not None:
            os.close(self.redirected_master)
        self.child.wait(timeout=5)


def exercise_tasks(work, env):
    def alive(pid):
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False

    for mode in ("inline", "fullscreen"):
        for stop in (False, True):
            cwd = work / f"tasks-{mode}-{stop}"
            cwd.mkdir()
            before_paths = set((Path(env["ION_HOME"]) / "sessions").rglob("*.sqlite"))
            terminal = Terminal(cwd, env, mode)
            pid = None
            try:
                terminal.paint()
                terminal.send(f"TASK_PARENT_{mode}_{stop}\r")
                terminal.wait(lambda: (cwd / "task.ready").exists() and b"state=done" in terminal.output)
                pid = int((cwd / "task.pid").read_text())
                child_paths = set((Path(env["ION_HOME"]) / "sessions").rglob("children/*.sqlite")) - before_paths
                assert len(child_paths) == 1, child_paths
                before = len(requests)
                terminal.output.clear()
                terminal.send("/tasks\r")
                terminal.wait(lambda: "Background tasks" in terminal.text())
                terminal.paint()
                terminal.send("smoke\r")
                terminal.wait(lambda: "Task actions" in terminal.text())
                terminal.paint()
                assert "Stop task" in terminal.text() and "Remove finished" not in terminal.text(), terminal.text()
                terminal.send(b"\x1b")
                terminal.paint()
                assert alive(pid) and len(requests) == before, "browsing/dismissal stopped or consumed work"
                terminal.output.clear()
                terminal.send("/tasks\r")
                terminal.wait(lambda: "Background tasks" in terminal.text())
                terminal.paint()
                terminal.send("\r")
                terminal.wait(lambda: "Task actions" in terminal.text())
                terminal.paint()
                if stop:
                    terminal.send("Stop\r")
                    terminal.wait(lambda: not alive(pid) and "Show result" in terminal.text())
                    assert not (cwd / "task.effect").exists(), "stop was treated as successful work"
                else:
                    # Completion must update the OPEN action list, not leave a stale stop/remove target.
                    (cwd / "task.release").touch()
                    terminal.wait(lambda: not alive(pid) and "Show result" in terminal.text())
                    assert (cwd / "task.effect").read_text() == "TASK_EFFECT"
                terminal.paint()
                terminal.output.clear()
                terminal.send("Show result\r")
                terminal.wait(lambda: "observed outcome" in terminal.text())
                terminal.paint()
                if not stop:
                    assert "Final answer" in terminal.text() and "TASK_FINAL_🦀" in terminal.text(), terminal.text()
                else:
                    assert "cancelled" in terminal.text() and "Final answer" not in terminal.text(), terminal.text()
                assert '"kind": "joined"' not in terminal.text(), "result is still a JSON protocol envelope"
                count = len(requests)
                # Merely reading the result must leave it selectable until explicit removal.
                terminal.output.clear()
                terminal.send("/tasks\r")
                terminal.wait(lambda: "Background tasks" in terminal.text())
                terminal.paint()
                terminal.send("\r")
                terminal.wait(lambda: "Remove finished task" in terminal.text())
                terminal.paint()
                terminal.send("Remove\r")
                terminal.wait(lambda: "Removed from task list" in terminal.text())
                terminal.output.clear()
                terminal.send("/tasks\r")
                terminal.wait(lambda: "No retained background tasks" in terminal.text())
                assert len(requests) == count, "task inspection/removal issued model work"
                children = []
                for path in child_paths:
                    assert path.exists(), "task removal deleted its child Session"
                    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
                    try:
                        entries = [json.loads(row[0]) for row in connection.execute("SELECT body FROM entries")]
                        if path.parent.name == "children" and any(entry["kind"] == "turn_ended" for entry in entries):
                            children.append(path)
                    finally:
                        connection.close()
                assert children, "task removal deleted child Session facts"
                terminal.finish()
            finally:
                (cwd / "task.release").touch()
                if pid:
                    deadline = time.monotonic() + 10
                    while alive(pid):
                        assert time.monotonic() < deadline, "owned task process survived cleanup"
                        terminal.pump()
                terminal.close()
            print(f"Tasks {mode} stop={stop}: live picker, no implicit cancellation, readable retained result, explicit removal and saved facts: OK", flush=True)


def exercise_orientation(work, env, endpoint):
    subprocess.run([binary, "use", "other", "vendor/model", "--endpoint", endpoint,
                    "--wire", "chat-completions"], env=env, check=True, capture_output=True)
    subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", endpoint,
                    "--wire", "chat-completions"], env=env, check=True, capture_output=True)
    defaults = (work / "ion/models.json").read_bytes()
    for mode in ("inline", "fullscreen"):
        cwd = work / (mode + "-orientation")
        cwd.mkdir()
        previous = set((work / "ion").rglob("*.sqlite"))
        before = len(requests)
        terminal = Terminal(cwd, env, mode, ["--cwd", str(cwd), "--tui-mode", mode,
                                            "--model", "other/vendor/model", "chat"])
        try:
            terminal.wait(lambda: "other/vendor/model · /help" in terminal.text())
            assert str(cwd) in terminal.text() and "Session " in terminal.text(), terminal.text()
            database, = set((work / "ion").rglob("*.sqlite")) - previous
            with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
                assert connection.execute("SELECT count(*) FROM entries").fetchone()[0] == 0
            terminal.send("/name Release\r")
            terminal.wait(lambda: " · Release" in terminal.text())
            terminal.send("/help model\r")
            terminal.wait(lambda: "Usage: /model [PROVIDER/MODEL]" in terminal.text())
            terminal.output.clear()
            terminal.send("/model smoke/smoke-model\r")
            terminal.wait(lambda: "smoke/smoke-model · /help" in terminal.text())
            previous = set((work / "ion").rglob("*.sqlite"))
            terminal.output.clear()
            terminal.send("/new\r")
            terminal.wait(lambda: "Started a new session" in terminal.text())
            database, = set((work / "ion").rglob("*.sqlite")) - previous
            with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
                header = json.loads(connection.execute("SELECT header FROM session WHERE id=1").fetchone()[0])
            terminal.wait(lambda: "Session " + header["id"][:8] in terminal.text())
            terminal.output.clear()
            terminal.send("ORIENTATION_🦀\x01")
            # Inline CPR is synthetic in this fixture: it cannot model native
            # reflow. Qualify inline resizing in a real emulator, not with a
            # guessed cursor report. Fullscreen has no such dependency.
            if mode == "fullscreen":
                for rows, columns in ((1, 12), (4, 30), (30, 100)):
                    terminal.resize(rows, columns)
                terminal.wait(lambda: "smoke/smoke-model · /help" in terminal.text())
            terminal.wait(lambda: "ORIENTATION_🦀" in terminal.text())
            assert len(requests) == before, "presentation admitted model input"
            terminal.send("PREFIX_\r")
            terminal.wait(lambda: len(requests) == before + 1)
            assert user_text(requests[-1]) == "PREFIX_ORIENTATION_🦀", user_text(requests[-1])
            assert requests[-1]["model"] == "smoke-model", requests[-1]
            terminal.wait(lambda: b"state=done" in terminal.output)
            terminal.finish()
        finally:
            terminal.close()
        assert (work / "ion/models.json").read_bytes() == defaults, "Session model changed the global default"
        print(f"Orientation {mode}: live binding, name/new, detailed help and exact draft (tiny resize in fullscreen): OK", flush=True)


def exercise_configuration(work, base_env, endpoint):
    home = work / "configuration-ion"
    env = dict(base_env, ION_HOME=str(home), HTTP_PROXY=endpoint, HTTPS_PROXY=endpoint, ALL_PROXY=endpoint,
               NO_PROXY="127.0.0.1,localhost")
    for provider, extra in (("login-only", []),):
        subprocess.run([binary, "use", provider, "model", "--endpoint", endpoint,
                        "--wire", "chat-completions", *extra], env=env, check=True, capture_output=True)
    subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", endpoint,
                    "--wire", "chat-completions"], env=env, check=True, capture_output=True)
    defaults = (home / "models.json").read_bytes()
    for mode in ("inline", "fullscreen"):
        credential = home / "credentials/openai.key"
        credential.unlink(missing_ok=True)
        cwd = work / (mode + "-configuration")
        cwd.mkdir()
        before = len(requests)
        terminal = Terminal(cwd, env, mode)
        try:
            terminal.wait(lambda: "smoke/smoke-model · /help" in terminal.text())
            release.clear()
            terminal.send("WAIT_HISTORY\r")
            terminal.wait(lambda: len(requests) == before + 1)
            terminal.output.clear()
            terminal.send("/model openai/gpt-5.4-mini\r")
            terminal.wait(lambda: "This action is available after the operation" in terminal.text())
            assert "API key:" not in terminal.text() and not credential.exists(), "busy model selection solicited a key"
            terminal.send("\x15")
            release.set()
            terminal.wait(lambda: b"state=done" in terminal.output)
            before = len(requests)
            terminal.send("/settings\r")
            terminal.wait(lambda: "Settings · Enter/Space" in terminal.text())
            terminal.send(" ")
            terminal.wait(lambda: "Tool output: expanded" in terminal.text())
            terminal.send("Thinking")
            terminal.paint()
            terminal.send("\r")
            terminal.wait(lambda: "presentation only; native history unchanged" in terminal.text())
            terminal.send("\x1b")
            terminal.paint()
            terminal.output.clear()
            terminal.send("/reasoning\r")
            terminal.wait(lambda: "Generation effort" in terminal.text())
            terminal.send("high")
            terminal.paint()
            terminal.send("\r")
            terminal.wait(lambda: "Generation effort: high" in terminal.text())
            terminal.send("/login\r")
            terminal.wait(lambda: "API-key provider" in terminal.text())
            terminal.send("login-only")
            terminal.paint()
            terminal.send("\r")
            terminal.wait(lambda: "login-only API key:" in terminal.text())
            assert not termios.tcgetattr(terminal.master)[3] & termios.ECHO
            terminal.send("DISPOSABLE_LOGIN_KEY\r")
            terminal.wait(lambda: "Saved login-only API key" in terminal.text())
            terminal.output.clear()
            terminal.send("/model openai/gpt-5.4-mini\r")
            terminal.wait(lambda: "openai API key:" in terminal.text())
            terminal.send("\x03")
            terminal.wait(lambda: "read login credential" in terminal.text())
            assert not credential.exists(), "cancelled model auth saved a key"
            terminal.wait(lambda: "smoke/smoke-model · /help" in terminal.text())
            terminal.send("\x15")
            terminal.paint()
            terminal.output.clear()
            # The shortcut must retain the literal and cursor. Native route
            # selection is local; return to loopback before any model request.
            terminal.send("DRAFT_🦀\x01\x0c")
            terminal.wait(lambda: "Choose model" in terminal.text())
            terminal.send("openai/gpt-5.4-mini")
            terminal.paint()
            terminal.send("\r")
            terminal.wait(lambda: "openai API key:" in terminal.text())
            terminal.send("DISPOSABLE_MODEL_KEY\rSECRET_TAIL_SHOULD_NOT_SUBMIT\r")
            terminal.wait(lambda: "openai/gpt-5.4-mini · /help" in terminal.text())
            assert credential.read_text() == "DISPOSABLE_MODEL_KEY"
            assert credential.stat().st_mode & 0o777 == 0o600
            assert len(requests) == before, "configuration or secret typeahead admitted input"
            assert "DISPOSABLE_" not in terminal.text(), "credential was echoed"
            terminal.output.clear()
            terminal.send("\x0c")
            terminal.wait(lambda: "Choose model" in terminal.text())
            terminal.send("smoke/smoke-model")
            terminal.paint()
            terminal.send("\r")
            terminal.wait(lambda: "smoke/smoke-model · /help" in terminal.text())
            terminal.output.clear()
            terminal.send("PREFIX_\r")
            terminal.wait(lambda: len(requests) == before + 1)
            assert user_text(requests[-1]) == "PREFIX_DRAFT_🦀", requests[-1]
            assert requests[-1]["model"] == "smoke-model" and requests[-1]["reasoning_effort"] == "high"
            terminal.wait(lambda: b"state=done" in terminal.output)
            terminal.finish()
        finally:
            release.set()
            terminal.close()
        assert (home / "models.json").read_bytes() == defaults, "configuration changed global defaults"
        assert not unexpected_connections, "configuration dispatched a native provider request: " + repr(unexpected_connections)
        print(f"Configuration {mode}: live settings, effort, API-key provider, model auth/cancel, secret quarantine and draft: OK", flush=True)


def exercise_resume(work, base_env, endpoint):
    def selected(terminal):
        # The query also starts with ›; only a rendered Session ID is a result.
        return re.search(r"› [0-9a-f]{8}-[0-9a-f]{3}  RESUME_BETA", terminal.text())

    for mode in ("inline", "fullscreen"):
        sandbox = work / (mode + "-resume")
        sandbox.mkdir()
        a, b, empty = [sandbox / name for name in ("alpha", "beta", "empty")]
        for cwd in (a, b, empty):
            cwd.mkdir()
        home = sandbox / "ion"
        home.mkdir()
        env = dict(base_env, HOME=str(sandbox / "home"), ION_HOME=str(home),
                   XDG_CONFIG_HOME=str(sandbox / "config"), XDG_STATE_HOME=str(sandbox / "state"),
                   HTTPS_PROXY=endpoint, HTTP_PROXY=endpoint, ALL_PROXY=endpoint,
                   NO_PROXY="127.0.0.1,localhost")
        marker = sandbox / "mcp-started"
        mcp = home / "mcp.json"
        mcp.write_text(json.dumps({"servers": {"must-not-start": {
            "command": sys.executable,
            "args": ["-c", f"from pathlib import Path; Path({str(marker)!r}).touch()"],
        }}}))
        before_requests = len(requests)
        terminal = Terminal(empty, env, mode, ["-r", "--tui-mode", mode])
        try:
            terminal.wait(lambda: "No saved Sessions for this working directory" in terminal.text())
            terminal.send(b"\t")
            terminal.wait(lambda: "No saved coding Sessions" in terminal.text())
            terminal.send(b"\x1b")
            terminal.wait_exit()
            assert termios.tcgetattr(terminal.master) == terminal.original_modes
            assert not list(home.rglob("*.sqlite")) and not (home / "models.json").exists()
            assert not marker.exists() and len(requests) == before_requests
        finally:
            terminal.close()
        mcp.unlink()
        subprocess.run([binary, "use", "resume-saved", "saved-model", "--endpoint", endpoint,
                        "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        for cwd, prompt in ((a, "RESUME_ALPHA"), (b, "RESUME_BETA")):
            subprocess.run([binary, "--cwd", cwd, "-p", prompt], env=env, capture_output=True, check=True)
        paths = set(home.rglob("*.sqlite"))
        assert len(paths) == 2
        subprocess.run([binary, "use", "resume-default", "default-model", "--endpoint", endpoint,
                        "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        defaults = (home / "models.json").read_bytes()
        before_requests = len(requests)

        # Preserve a filtered query across scope changes; browsing/cancel never
        # opens a Session or picks a model, even with a working saved default.
        terminal = Terminal(a, env, mode, ["-r", "--tui-mode", mode])
        try:
            terminal.wait(lambda: "RESUME_ALPHA" in terminal.text())
            terminal.send("RESUME_BETA")
            terminal.paint()
            terminal.send(b"\t")
            terminal.wait(lambda: selected(terminal))
            terminal.finish()
            assert set(home.rglob("*.sqlite")) == paths
            assert (home / "models.json").read_bytes() == defaults
            assert len(requests) == before_requests
        finally:
            terminal.close()

        # Implicit launch cwd can change through an explicit All-folders choice.
        # Stored model and history win over the global default; selection itself
        # does not submit input. The next real prompt goes into the same Session.
        terminal = Terminal(a, env, mode, ["--resume", "--tui-mode", mode])
        try:
            terminal.wait(lambda: "RESUME_ALPHA" in terminal.text())
            terminal.send("RESUME_BETA\t")
            terminal.wait(lambda: selected(terminal))
            terminal.paint()
            terminal.output.clear()
            terminal.send(b"\r")
            terminal.wait(lambda: "resume-saved/saved-model" in terminal.text())
            assert "default-model" not in terminal.text()
            assert len(requests) == before_requests
            terminal.send("RESUME_CHECK_BETA\r")
            terminal.wait(lambda: "DONE_RESUME_CHECK_BETA" in terminal.text())
            terminal.wait(lambda: b"\x1b]7501;state=done:app=ion\x1b\\" in terminal.output)
            terminal.finish()
            assert termios.tcgetattr(terminal.master) == terminal.original_modes
        finally:
            terminal.close()
        assert len(requests) == before_requests + 1
        body = requests[-1]
        assert body["model"] == "saved-model" and user_text(body) == "RESUME_CHECK_BETA"
        assert "RESUME_BETA" in json.dumps(body) and "RESUME_ALPHA" not in json.dumps(body)
        views = [json.loads(subprocess.run([binary, "--session", path, "inspect"], env=env,
                                          capture_output=True, check=True).stdout) for path in paths]
        target, = [view for view in views if view["cwd"] == str(b.resolve())]
        assert sum(entry["kind"] == "turn_ended" for entry in target["entries"]) == 2
        assert set(home.rglob("*.sqlite")) == paths
        assert (home / "models.json").read_bytes() == defaults

        # An explicit directory is still a fence, not permission to adopt another
        # workspace. Read-only browsing needs neither the route nor credentials.
        terminal = Terminal(a, env, mode, ["--cwd", a, "--resume", "inspect"])
        try:
            terminal.wait(lambda: "RESUME_ALPHA" in terminal.text())
            terminal.send("RESUME_BETA\t")
            terminal.wait(lambda: selected(terminal))
            terminal.paint()
            terminal.output.clear()
            terminal.send(b"\r")
            end = time.monotonic() + 10
            while terminal.child.poll() is None:
                assert time.monotonic() < end, terminal.text()
                terminal.pump()
            terminal.paint()
            assert terminal.child.returncode == 1 and "--cwd does not match" in terminal.text()
            assert termios.tcgetattr(terminal.master) == terminal.original_modes
        finally:
            terminal.close()
        for args in (["--resume", "-p", "NEVER_SEND"], ["rpc", "--resume"]):
            result = subprocess.run([binary, *args], cwd=a, env=env, capture_output=True)
            assert result.returncode != 0 and not result.stdout
        assert len(requests) == before_requests + 1
        assert set(home.rglob("*.sqlite")) == paths

        # Persist a native model choice with a disposable environment credential,
        # but never call that provider. Its later repair is Session-local, not
        # fresh-launch default onboarding, even after cancelling the key reader.
        target_path, = [path for path in paths if json.loads(subprocess.run(
            [binary, "--session", path, "inspect"], env=env, capture_output=True,
            check=True).stdout)["cwd"] == str(b.resolve())]
        keyed_env = dict(env, OPENAI_API_KEY="DISPOSABLE_NOT_SENT")
        terminal = Terminal(b, keyed_env, mode, ["--session", target_path, "--tui-mode", mode])
        try:
            terminal.wait(lambda: "resume-saved/saved-model" in terminal.text())
            terminal.send("/model openai/gpt-5.4-mini\r")
            terminal.wait(lambda: "Selected openai/gpt-5.4-mini" in terminal.text())
            terminal.finish()
        finally:
            terminal.close()
        terminal = Terminal(b, env, mode, ["--resume", "--tui-mode", mode])
        try:
            terminal.wait(lambda: selected(terminal))
            terminal.send(b"\r")
            terminal.wait(lambda: "openai API key:" in terminal.text())
            terminal.send(b"\x03")
            terminal.wait(lambda: "choose a model to retry" in terminal.text())
            terminal.send("resume-saved/saved-model")
            terminal.paint()
            terminal.output.clear()
            terminal.send(b"\r")
            terminal.wait(lambda: "resume-saved/saved-model" in terminal.text())
            terminal.finish()
        finally:
            terminal.close()
        assert (home / "models.json").read_bytes() == defaults, "resumed model repair rewrote the global default"
        assert not (home / "credentials/openai.key").exists()
        mcp.write_text(json.dumps({"servers": {"must-not-start": {
            "command": sys.executable,
            "args": ["-c", f"from pathlib import Path; Path({str(marker)!r}).touch()"],
        }}}))
        terminal = Terminal(b, env, mode, ["inspect", "--resume"])
        try:
            terminal.wait(lambda: selected(terminal))
            terminal.output.clear()
            terminal.send(b"\r")
            terminal.wait_exit()
            terminal.paint()
            assert '"last_model"' in terminal.text() and '"gpt-5.4-mini"' in terminal.text()
            assert "API key:" not in terminal.text() and not marker.exists()
            assert termios.tcgetattr(terminal.master) == terminal.original_modes
        finally:
            terminal.close()
        mcp.unlink()

        # A discovery snapshot is not permission to create a replacement when
        # its file vanishes, including after metadata was read for key setup.
        for stage, cwd, prompt in (("selection", a, "RESUME_ALPHA"), ("key setup", b, "RESUME_BETA")):
            path, = [path for path in paths if json.loads(subprocess.run(
                [binary, "--session", path, "inspect"], env=env, capture_output=True,
                check=True).stdout)["cwd"] == str(cwd.resolve())]
            backup = path.with_suffix(".held")
            terminal = Terminal(cwd, env, mode, ["--resume", "--tui-mode", mode])
            try:
                terminal.wait(lambda: re.search(r"› [0-9a-f]{8}-[0-9a-f]{3}  " + prompt, terminal.text()))
                if stage == "key setup":
                    terminal.send(b"\r")
                    terminal.wait(lambda: "openai API key:" in terminal.text())
                path.rename(backup)
                terminal.output.clear()
                if stage == "selection":
                    terminal.send(b"\r")
                else:
                    terminal.send(b"\x03")
                    terminal.wait(lambda: "choose a model to retry" in terminal.text())
                    terminal.send("resume-saved/saved-model")
                    terminal.paint()
                    terminal.output.clear()
                    terminal.send(b"\r")
                end = time.monotonic() + 10
                while terminal.child.poll() is None and not path.exists():
                    assert time.monotonic() < end, terminal.text()
                    terminal.pump()
                assert not path.exists(), f"missing saved Session was recreated during {stage}"
                assert terminal.child.wait(timeout=5) == 1
                assert termios.tcgetattr(terminal.master) == terminal.original_modes
            finally:
                terminal.close()
                if backup.exists():
                    for leftover in (path, Path(str(path) + "-wal"), Path(str(path) + "-shm")):
                        leftover.unlink(missing_ok=True)
                    backup.rename(path)
        assert (home / "models.json").read_bytes() == defaults
        assert len(requests) == before_requests + 1 and not unexpected_connections
        requests.clear()
        print(f"Resume {mode}: empty/cancel, scoped search, cross-workspace reopen, model/history/default, cwd fence and terminal restoration: OK", flush=True)


def exercise_onboarding(work, base_env, endpoint):
    for mode in ("inline", "fullscreen"):
        cwd = work / (mode + "-onboarding")
        cwd.mkdir()
        home = cwd / "ion"
        env = dict(base_env, HOME=str(cwd / "home"), ION_HOME=str(home),
                   XDG_CONFIG_HOME=str(cwd / "config"), XDG_STATE_HOME=str(cwd / "state"),
                   HTTPS_PROXY=endpoint, HTTP_PROXY=endpoint, ALL_PROXY=endpoint,
                   NO_PROXY="127.0.0.1,localhost")
        home.mkdir()
        marker = cwd / "mcp-started"
        mcp = home / "mcp.json"
        mcp.write_text(json.dumps({"servers": {"must-not-start": {
            "command": sys.executable,
            "args": ["-c", f"from pathlib import Path; Path({str(marker)!r}).touch()"],
        }}}))
        # No credentials, model calls or Session are needed to reach setup.
        result = subprocess.run([binary, "-p", "NEVER_SEND"], env=env, capture_output=True)
        assert result.returncode == 1 and b"no model selected" in result.stderr
        terminal = Terminal(cwd, env, mode)
        try:
            terminal.wait(lambda: "Choose default model" in terminal.text())
            assert not list(home.rglob("*.sqlite")), "setup allocated a Session"
            terminal.send("openai/gpt-5.4-mini")
            terminal.paint()
            terminal.send(b"\rPRE_READ_DISPOSABLE\r")
            terminal.wait(lambda: "openai API key:" in terminal.text())
            assert not termios.tcgetattr(terminal.master)[3] & (termios.ECHO | termios.ECHONL)
            terminal.send(b"CANCELLED_DISPOSABLE\x03PROMPT_DISPOSABLE\r")
            terminal.wait(lambda: "choose a model to retry" in terminal.text())
            assert "DISPOSABLE" not in terminal.text(), "setup credential leaked"
            assert not list(home.rglob("*.sqlite")), "cancelled login allocated a Session"
            assert not (home / "models.json").exists(), "cancelled choice changed the default"
            assert not (home / "credentials/openai.key").exists()
            assert not marker.exists(), "cancelled setup launched MCP"
            terminal.finish()
            assert termios.tcgetattr(terminal.master) == terminal.original_modes
        finally:
            terminal.close()

        mcp.unlink()
        # A disposable key is saved locally; no prompt is submitted to OpenAI.
        # Startup must not issue any model request or admit credential type-ahead.
        terminal = Terminal(cwd, env, mode)
        try:
            terminal.wait(lambda: "Choose default model" in terminal.text())
            terminal.send("openai/gpt-5.4-mini")
            terminal.paint()
            terminal.send(b"\r")
            terminal.wait(lambda: "openai API key:" in terminal.text())
            terminal.output.clear()
            terminal.send(b"ACTUAL_DISPOSABLE\rSUFFIX_DISPOSABLE\r")
            terminal.wait(lambda: "›" in terminal.text())
            terminal.paint()
            assert "DISPOSABLE" not in terminal.text(), "secret became visible input"
            saved = json.loads((home / "models.json").read_text())
            assert saved["default"] == {"provider": "openai", "model": "gpt-5.4-mini"}
            key = home / "credentials/openai.key"
            assert key.read_bytes() == b"ACTUAL_DISPOSABLE"
            assert key.stat().st_mode & 0o777 == 0o600
            terminal.finish()
        finally:
            terminal.close()
        sessions = list(home.rglob("*.sqlite"))
        assert len(sessions) == 1
        inspection = subprocess.run([binary, "--session", sessions[0], "inspect"], env=env, capture_output=True, check=True)
        assert b"DISPOSABLE" not in inspection.stdout and b"NEVER_SEND" not in inspection.stdout
        turns = subprocess.run([binary, "--session", sessions[0], "turns"], env=env, capture_output=True, check=True)
        assert not turns.stdout.strip(), "onboarding admitted model input"

        # Invalid explicit routes must fail, not silently offer a different model.
        terminal = Terminal(cwd, env, mode, ["--provider", "missing", "--model", "unknown"])
        try:
            deadline = time.monotonic() + 5
            while terminal.child.poll() is None:
                assert time.monotonic() < deadline
                terminal.pump()
            terminal.paint()
            assert terminal.child.returncode == 1 and "Choose default model" not in terminal.text()
        finally:
            terminal.close()

        # A known model with a missing credential goes directly to masked entry,
        # even when reopening a Session. Cancellation does not modify its facts.
        subprocess.run([binary, "logout", "openai"], env=env, capture_output=True, check=True)
        before = sessions[0].read_bytes()
        terminal = Terminal(cwd, env, mode, ["--session", sessions[0], "--tui-mode", mode])
        try:
            terminal.wait(lambda: "openai API key:" in terminal.text())
            terminal.send(b"\x03")
            terminal.wait(lambda: "choose a model to retry" in terminal.text())
            terminal.finish()
            assert sessions[0].read_bytes() == before, "cancelled setup changed the saved Session"
        finally:
            terminal.close()
        assert not unexpected_connections, unexpected_connections
        assert not requests, "setup issued a model request"

        # Configured keyless endpoints use the same picker and admission path.
        subprocess.run([binary, "use", "local", "fixture", "--endpoint", endpoint,
                        "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        config = json.loads((home / "models.json").read_text())
        config["default"] = None
        (home / "models.json").write_text(json.dumps(config))
        terminal = Terminal(cwd, env, mode)
        try:
            terminal.wait(lambda: "Choose default model" in terminal.text())
            terminal.send("local/fixture")
            terminal.paint()
            terminal.output.clear()
            terminal.send(b"\r")
            terminal.wait(lambda: "›" in terminal.text())
            assert "API key:" not in terminal.text(), "keyless route requested a credential"
            prompt = "ONBOARD_LOCAL_" + mode
            terminal.send(prompt + "\r")
            terminal.wait(lambda: "DONE_" + prompt in terminal.text())
            assert len(requests) == 1 and user_text(requests[0]) == prompt, requests
            assert "DISPOSABLE" not in json.dumps(requests)
            # Answer text can precede settlement; Ctrl-C would still cancel then.
            terminal.wait(lambda: b"\x1b]7501;state=done:app=ion\x1b\\" in terminal.output)
            terminal.finish()
        finally:
            terminal.close()
        requests.clear()
        print(f"Onboarding {mode}: picker, masked key, quarantine, default, cancellation, MCP fence, reopen and keyless admission: OK", flush=True)


with tempfile.TemporaryDirectory(prefix="ion-input-") as temporary:
    work = Path(temporary)
    env = {name: value for name, value in os.environ.items() if name in ("PATH", "SHELL", "TMPDIR")}
    env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        exercise_onboarding(work, env, f"http://127.0.0.1:{server.server_port}/v1")
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", f"http://127.0.0.1:{server.server_port}/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        exercise_resume(work, env, f"http://127.0.0.1:{server.server_port}/v1/chat/completions")
        exercise_orientation(work, env, f"http://127.0.0.1:{server.server_port}/v1/chat/completions")
        exercise_configuration(work, env, f"http://127.0.0.1:{server.server_port}/v1/chat/completions")
        exercise_tasks(work, env)
        # Display sanitation must not bypass the secret reader's input guard.
        terminal = Terminal(work, env, "inline", ["login", "unsafe\x1b[31mprovider"])
        try:
            deadline = time.monotonic() + 3
            while terminal.child.poll() is None:
                assert time.monotonic() < deadline, "control-bearing provider entered secret input"
                terminal.pump()
            terminal.paint()
            assert terminal.child.returncode == 1, terminal.text()
            assert "secret prompt contains control characters" in terminal.text(), terminal.text()
            assert "API key:" not in terminal.text(), "transformed provider label solicited a secret"
            assert not (work / "ion/credentials").exists(), "rejected prompt changed credentials"
        finally:
            terminal.close()

        # A long prompt blocks its writer before rpassword could disable echo.
        # Inspect the real kernel state at the visible prefix, not a timing race.
        terminal = Terminal(work, env, "inline", ["login", "smoke API key: " + "x" * 65536])
        try:
            terminal.wait(lambda: "smoke API key:" in terminal.text())
            assert not termios.tcgetattr(terminal.master)[3] & (termios.ECHO | termios.ECHONL), "visible secret prompt still enables echo"
        finally:
            terminal.close()

        for ending, redirected in ((b"\r", False), (b"\x03", False), (b"\x04", False), (b"\x03", True)):
            terminal = Terminal(work, env, "inline", ["login", "smoke"], redirected_tty=redirected)
            try:
                terminal.wait(lambda: "smoke API key:" in terminal.text())
                assert not termios.tcgetattr(terminal.master)[3] & (termios.ECHO | termios.ECHONL)
                terminal.send(b"CLI_DISPOSABLE" + ending if ending != b"\x04" else ending)
                deadline = time.monotonic() + 10
                while terminal.child.poll() is None:
                    assert time.monotonic() < deadline, "CLI secret entry did not settle"
                    terminal.pump()
                terminal.paint()
                assert terminal.child.returncode == (0 if ending == b"\r" else 1), terminal.text()
                assert termios.tcgetattr(terminal.master) == terminal.original_modes, "CLI secret entry did not restore terminal"
                if redirected:
                    assert termios.tcgetattr(terminal.redirected_master) == terminal.redirected_modes, "login changed an unrelated stdin TTY"
                assert "DISPOSABLE" not in terminal.text(), "CLI credential leaked"
                key_path = work / "ion/credentials/smoke.key"
                assert key_path.read_bytes() == b"CLI_DISPOSABLE", "cancel/EOF changed the saved key"
            finally:
                terminal.close()
        print("CLI secret visibility, success/cancel/EOF and termios restoration: OK", flush=True)

        for mode in ("inline", "fullscreen"):
            cwd = work / (mode + "-intake")
            cwd.mkdir()
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "›" in terminal.text())
                before = len(requests)
                print(f"Intake {mode}: malformed paste", flush=True)
                terminal.send(b"BASE\x1b[H\x1b[200")
                terminal.paint()  # Split the opener beyond Escape grace.
                terminal.send(b"~body\r\xff\x1b[201~")
                terminal.wait(lambda: "invalid UTF-8" in terminal.text())
                assert len(requests) == before, "malformed paste submitted input"
                print(f"Intake {mode}: oversized paste", flush=True)
                terminal.send(b"\x1b[200~" + b"x" * (64 * 1024 + 1))
                terminal.wait(lambda: "Paste exceeds terminal input limit" in terminal.text())
                assert len(requests) == before, "overflow submitted a truncated paste"
                print(f"Intake {mode}: submit preserved draft", flush=True)
                terminal.send(b"\x1b[201~PREFIX_\r")
                terminal.wait(lambda: len(requests) == before + 1)
                assert user_text(requests[-1]) == "PREFIX_BASE", user_text(requests[-1])
                terminal.wait(lambda: "DONE_PREFIX_BASE" in terminal.text())
                terminal.paint()
                terminal.output.clear()
                print(f"Intake {mode}: credential quarantine", flush=True)
                terminal.send(b"/login smoke\rPRE_READ_DISPOSABLE\r")
                terminal.wait(lambda: "smoke API key:" in terminal.text())
                assert not termios.tcgetattr(terminal.master)[3] & (termios.ECHO | termios.ECHONL)
                terminal.send(b"ACTUAL_DISPOSABLE\rPOST_READ_DISPOSABLE\r")
                terminal.wait(lambda: "Credential saved" in terminal.text())
                terminal.paint()
                terminal.send(b"\r")
                terminal.paint()
                assert (work / "ion/credentials/smoke.key").read_bytes() == b"ACTUAL_DISPOSABLE", "credential entry lost bytes or saved type-ahead"
                assert len(requests) == before + 1, "pre-read credential became chat input"
                assert "DISPOSABLE" not in terminal.text(), "credential leaked to terminal"
                for ending in (b"\x03", b"\x04", b"\x1b[200~bad\nvalue\x1b[201~"):
                    terminal.output.clear()
                    terminal.send(b"/login smoke\r")
                    terminal.wait(lambda: "smoke API key:" in terminal.text())
                    terminal.send(ending + b"CANCELLED_DISPOSABLE\r")
                    terminal.wait(lambda: "read login credential" in terminal.text())
                    terminal.paint()
                    assert "DISPOSABLE" not in terminal.text(), "cancelled/rejected credential became chat input"
                    assert len(requests) == before + 1, "secret suffix was admitted"
                terminal.output.clear()
                terminal.send(b"/login smoke\r")
                terminal.wait(lambda: "smoke API key:" in terminal.text())
                terminal.send(b"\x1b[200~" + b"x" * (64 * 1024 + 1))
                terminal.wait(lambda: "Secret paste discarded; waiting for its closing marker" in terminal.text())
                terminal.send(b"DELAYED_DISPOSABLE\r")
                terminal.paint()
                assert len(requests) == before + 1, "overflow secret tail submitted a model request"
                assert "DISPOSABLE" not in terminal.text(), "overflow secret tail escaped discard boundary"
                terminal.send(b"\x1b[201~")
                terminal.wait(lambda: "read login credential" in terminal.text())
                terminal.paint()
                assert (work / "ion/credentials/smoke.key").read_bytes() == b"ACTUAL_DISPOSABLE", "rejected/cancelled entry replaced the saved credential"
                terminal.finish()
                assert termios.tcgetattr(terminal.master) == terminal.original_modes, "chat secret entry did not restore terminal"
            finally:
                terminal.close()

            # File suggestions never own literal input. Browse/accept/dismiss
            # through the real editor, with the model request as prompt oracle.
            cwd = work / (mode + "-files")
            (cwd / "src").mkdir(parents=True)
            (cwd / "src/main.rs").write_text("fn main() {}\n")
            (cwd / "plain.rs").write_text("fixture\n")
            home_paths = Path(env["HOME"]) / f"paths-{mode}"
            home_paths.mkdir(parents=True)
            (home_paths / "main.rs").write_text("home fixture\n")
            (cwd / "space dir").mkdir()
            (cwd / "space dir/café🦀.rs").write_text("fixture\n")
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "›" in terminal.text())
                cases = [
                    ("nick@example.com", None, "nick@example.com"),
                    ("Read @m next", "dismiss", "Read @ma next"),
                    ("Read @smrs next", "accept", "Read @src/main.rs next"),
                    ("Read @ma", "paste", "Read @src/main.rs "),
                    ("Read @ma next", "undo", "Read @ma next"),
                    ("Read pl next", "bare", "Read plain.rs next"),
                    ("Read ./src/ma next", "accept", "Read ./src/main.rs next"),
                    (f"Read {cwd}/src/ma next", "accept", f"Read {cwd}/src/main.rs next"),
                    (f"@{cwd}/src/ma next", "accept", f"@{cwd}/src/main.rs next"),
                    (f'"{cwd}/src/ma next', "accept", f'"{cwd}/src/main.rs" next'),
                    (f"Read ~/paths-{mode}/ma next", "accept", f"Read ~/paths-{mode}/main.rs next"),
                    ('Read "space dir/ca next', "accept", 'Read "space dir/café🦀.rs" next'),
                    ('Read @"space d next', "directory", 'Read @"space dir/café🦀.rs" next'),
                ]
                # This fixture's synthetic CPR cannot establish native inline
                # reflow. Qualify inline resize with an actual emulator instead.
                if mode == "fullscreen":
                    cases.append(("Read @ma next", "clipped", "Read @ma next"))
                for prompt, action, expected in cases:
                    terminal.output.clear()
                    before = len(requests)
                    terminal.send(b"\x1b[200~" + prompt.encode() + b"\x1b[201~" if action == "paste" else prompt)
                    terminal.paint()
                    if action:
                        if action != "paste":
                            terminal.send(b"\x1b[D" * len(" next"))
                        if action == "bare":
                            terminal.send(b"\t")
                            terminal.paint()
                            terminal.send("a")  # Forced completion survives ordinary typing.
                        terminal.wait(lambda: "Files ·" in terminal.text())
                        terminal.paint()
                        if action == "clipped":
                            # Submit DURING resize settling, before a new frame
                            # can publish. Old visible rows authorize nothing in
                            # the now-clipped/reconciling viewport.
                            terminal.resize(30, 2, settle=False)
                            terminal.send(b"\r")
                        elif action == "dismiss":
                            terminal.send("a")
                            terminal.paint()
                            terminal.send(b"\x1b")
                        else:
                            terminal.send(b"\t" if action == "directory" else b"\r")
                            terminal.paint()
                            if action == "directory":
                                terminal.send(b"\t")
                                terminal.paint()
                            if action == "undo":
                                terminal.send(b"\x1f")
                                terminal.paint()
                                terminal.send(b"\x1b")
                        terminal.paint()
                        if action != "clipped":
                            assert len(requests) == before, ("completion/dismissal sent a prompt", prompt, action, user_text(requests[-1]))
                    if action != "clipped":
                        terminal.send(b"\r")
                    terminal.wait(lambda: len(requests) == before + 1)
                    assert user_text(requests[-1]) == expected, user_text(requests[-1])
                    if action == "clipped":
                        terminal.resize(30, 100)
                    terminal.wait(lambda: b"state=done" in terminal.output)
                terminal.finish()
            finally:
                terminal.close()
            print(f"File input {mode}: literal email, fuzzy references, bare/relative/absolute/home paths, dismiss/accept/undo and quoted directories" + ("; clipped menu during resize" if mode == "fullscreen" else "") + ": OK", flush=True)

            # Stopping an observed shell withdraws automatic queued dispatch;
            # a normal exit 7 does not. Neither changes the saved native outcome.
            for cancelled in (True, False):
                cwd = work / f"{mode}-shell-queue-{cancelled}"
                cwd.mkdir()
                previous = set((work / "ion").rglob("*.sqlite"))
                terminal = Terminal(cwd, env, mode)
                pid = None
                try:
                    terminal.wait(lambda: "›" in terminal.text())
                    database, = set((work / "ion").rglob("*.sqlite")) - previous
                    before = len(requests)
                    command = "printf '%s\\n' $$ > native.pid; printf 'OBSERVED_QUEUE\\n'; touch native.ready; " + (
                        "exec sleep 30" if cancelled else
                        "while ! test -f native.release; do sleep 0.02; done; exit 7"
                    )
                    terminal.send("!" + command + "\r")
                    terminal.wait(lambda: (cwd / "native.ready").exists())
                    pid = int((cwd / "native.pid").read_text())
                    terminal.output.clear()
                    terminal.send("QUEUED🦀\x01\r")
                    terminal.wait(lambda: "1 follow-up(s) queued" in terminal.text())
                    terminal.send("unsent")
                    if cancelled:
                        terminal.send(b"\x03")
                    else:
                        (cwd / "native.release").touch()

                    def shell_facts():
                        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
                            return [json.loads(body) for body, in connection.execute("SELECT body FROM entries ORDER BY seq")]

                    terminal.wait(lambda: any(entry["kind"] == "user_shell_settled" for entry in shell_facts()))
                    terminal.paint()
                    facts = shell_facts()
                    outcome = next(entry["data"]["outcome"] for entry in facts if entry["kind"] == "user_shell_settled")
                    assert outcome["kind"] == "observed" and outcome["is_error"], outcome
                    assert outcome["output"]["stdout"] == "OBSERVED_QUEUE\n", outcome
                    assert outcome["output"]["cancelled"] == cancelled, outcome
                    try:
                        os.kill(pid, 0)
                    except ProcessLookupError:
                        pid = None
                    else:
                        raise AssertionError("shell process survived settlement")
                    if cancelled:
                        assert len(requests) == before, "shell stop dispatched an unaccepted queued prompt"
                        assert not any(entry["kind"] == "turn_started" for entry in facts), facts
                        # Returned inputs preserve their own literal/cursor; never
                        # merge separate prompts or attachments with the editor.
                        terminal.send(b"\x1b[1;3A")  # Alt-Up recalls the returned input.
                        terminal.paint()
                        terminal.send("PREFIX_\r")
                        terminal.wait(lambda: len(requests) == before + 1)
                        assert user_text(requests[-1]) == "PREFIX_QUEUED🦀", user_text(requests[-1])
                        terminal.wait(lambda: "DONE_PREFIX_QUEUED" in terminal.text())
                        terminal.wait(lambda: any(entry["kind"] == "turn_ended" for entry in shell_facts()))
                        terminal.paint()
                        terminal.send(b"\x1b[1;3A\r")
                        terminal.wait(lambda: len(requests) == before + 2)
                        assert user_text(requests[-1]) == "unsent", user_text(requests[-1])
                        terminal.wait(lambda: "DONE_unsent" in terminal.text())
                    else:
                        assert outcome["output"]["exit_code"] == 7, outcome
                        terminal.wait(lambda: len(requests) == before + 1)
                        assert user_text(requests[-1]) == "QUEUED🦀", user_text(requests[-1])
                        terminal.wait(lambda: "DONE_QUEUED" in terminal.text())
                    terminal.paint()
                    terminal.send(b"\x15")
                    terminal.paint()
                    terminal.finish()
                    assert termios.tcgetattr(terminal.master) == terminal.original_modes, "shell queue exit did not restore terminal"
                finally:
                    terminal.close()
                    if pid is not None:
                        try:
                            os.kill(pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
            print(f"Ion {mode} shell stop preserves separate drafts; nonzero exit retains follow-up intent: OK", flush=True)

            cwd = work / (mode + "-shell-refusal")
            cwd.mkdir()
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "›" in terminal.text())
                before = len(requests)
                release.clear()
                terminal.send("WAIT_HISTORY\r")
                terminal.wait(lambda: len(requests) == before + 1)
                terminal.send(b"  !touch MUST_NOT_EXIST  \x1b[H\r")
                terminal.wait(lambda: "Shell was not started:" in terminal.text())
                terminal.output.clear()
                release.set()
                terminal.wait(lambda: "DONE_WAIT_HISTORY" in terminal.text())
                terminal.paint()
                terminal.send("SAFE_\r")
                terminal.wait(lambda: len(requests) == before + 2 or bool(list(cwd.glob("MUST_NOT_EXIST*"))))
                assert not list(cwd.glob("MUST_NOT_EXIST*")), "refused shell input was executed"
                assert len(requests) == before + 2, "preserved literal was not submitted"
                assert user_text(requests[-1]) == "SAFE_  !touch MUST_NOT_EXIST  ", user_text(requests[-1])
                terminal.wait(lambda: "DONE_SAFE_" in terminal.text())
                terminal.paint()
                terminal.finish()
                print(f"Ion {mode} busy shell literal/cursor refusal: OK")
            finally:
                release.set()
                terminal.close()

            cwd = work / (mode + "-editing")
            cwd.mkdir()
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "›" in terminal.text())
                before = len(requests)
                release.clear()
                terminal.send("WAIT_HISTORY\r")
                terminal.wait(lambda: len(requests) == before + 1)
                # Edit during generation without using Ctrl-C (which cancels).
                terminal.send("discard café 👩‍💻\x15kept e\u0301 👩‍💻 tail\x17\x17界")
                terminal.send("\x01PREFIX_\x05\x1b[1;5D\x1bd🦀")
                terminal.wait(lambda: "PREFIX_kept e\u0301 🦀" in terminal.text())
                assert len(requests) == before + 1, "editing shortcuts admitted input"
                release.set()
                terminal.wait(lambda: "DONE_WAIT_HISTORY" in terminal.text())
                terminal.paint()
                terminal.send("\r")
                terminal.wait(lambda: len(requests) == before + 2)
                assert user_text(requests[-1]) == "PREFIX_kept e\u0301 🦀", user_text(requests[-1])
                terminal.wait(lambda: "DONE_PREFIX_kept" in terminal.text())
                terminal.paint()
                terminal.finish()
                print(f"Ion {mode} busy Unicode line/word editing: OK")
            finally:
                release.set()
                terminal.close()

            cwd = work / (mode + "-history")
            cwd.mkdir()
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "›" in terminal.text())
                before = len(requests)
                terminal.send("EARLIER\r")
                terminal.wait(lambda: "DONE_EARLIER" in terminal.text())
                terminal.paint()
                release.clear()
                terminal.output.clear()
                terminal.send("WAIT_HISTORY\r")
                terminal.wait(lambda: len(requests) == before + 2)
                terminal.send("ORIGINAL🦀\x1b[H\x1b[A")
                terminal.wait(lambda: "› EARLIER" in terminal.text())
                terminal.output.clear()
                release.set()
                terminal.wait(lambda: "DONE_WAIT_HISTORY" in terminal.text())
                terminal.paint()
                terminal.send("\x1b[B")
                terminal.paint()
                terminal.send("PREFIX_\r")
                terminal.wait(lambda: len(requests) == before + 3)
                assert user_text(requests[-1]) == "PREFIX_ORIGINAL🦀", user_text(requests[-1])
                terminal.wait(lambda: "DONE_PREFIX_ORIGINAL" in terminal.text())
                terminal.paint()
                terminal.finish()
            finally:
                release.set()
                terminal.close()

            cwd = work / (mode + "-admission")
            cwd.mkdir()
            previous = set((work / "ion").rglob("*.sqlite"))
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "›" in terminal.text())
                database, = set((work / "ion").rglob("*.sqlite")) - previous
                connection = sqlite3.connect(database)
                try:
                    seq = connection.execute("SELECT COALESCE(MAX(seq), -1) + 1 FROM entries").fetchone()[0]
                    connection.execute("INSERT INTO entries VALUES (?, ?)", (seq, b'{"kind":"invalid_qualification_entry"}'))
                    connection.execute("CREATE TRIGGER reject_admission BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'INPUT_QUALIFICATION_REFUSAL'); END")
                    connection.commit()
                    before = len(requests)
                    terminal.output.clear()
                    terminal.send("KEEP_ORIGINAL_INPUT\r")
                    terminal.wait(lambda: "Session history refresh failed" in terminal.text())
                    assert "› KEEP_ORIGINAL_INPUT" in terminal.text(), terminal.output[-2500:]
                    assert len(requests) == before, "failed admission issued a request"
                    connection.execute("DROP TRIGGER reject_admission")
                    connection.execute("DELETE FROM entries WHERE seq=?", (seq,))
                    connection.commit()
                    terminal.output.clear()
                    terminal.send("\r")
                    terminal.wait(lambda: len(requests) == before + 1)
                    assert user_text(requests[-1]) == "KEEP_ORIGINAL_INPUT"
                    terminal.wait(lambda: "DONE_KEEP_ORIGINAL_INPUT" in terminal.text())
                    terminal.paint()
                    terminal.finish()
                finally:
                    connection.close()
            finally:
                terminal.close()
            cwd = work / (mode + "-accepted")
            cwd.mkdir()
            previous = set((work / "ion").rglob("*.sqlite"))
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "›" in terminal.text())
                database, = set((work / "ion").rglob("*.sqlite")) - previous
                before = len(requests)
                release.clear()
                terminal.output.clear()
                terminal.send("ACCEPTED_VIEW_FAULT\r")
                terminal.wait(lambda: len(requests) == before + 1)
                connection = sqlite3.connect(database)
                try:
                    seq = connection.execute("SELECT COALESCE(MAX(seq), -1) + 1 FROM entries").fetchone()[0]
                    connection.execute("INSERT INTO entries VALUES (?, ?)", (seq, b'{"kind":"invalid_qualification_entry"}'))
                    connection.commit()
                    release.set()
                    terminal.wait(lambda: "Session history refresh failed" in terminal.text())
                    terminal.paint()
                    terminal.send("\r")
                    terminal.paint()
                    assert len(requests) == before + 1, "accepted input was restored and replayed"
                    connection.execute("DELETE FROM entries WHERE seq=?", (seq,))
                    connection.commit()
                    terminal.finish()
                finally:
                    connection.close()
            finally:
                release.set()
                terminal.close()
            print(f"Ion {mode} history/cursor and typed admission recovery/no replay: OK")
    finally:
        release.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
