"""Exercise shared resource expansion and terminal command discovery/completion."""

import json
import errno
import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import tempfile
import threading
import termios
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        events = [
            {"id": "resources", "choices": [{"index": 0, "delta": {"content": "RESOURCE_OK"}, "finish_reason": None}]},
            {"id": "resources", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format, *args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-resources-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    skill = workspace / ".agents" / "skills" / "audit"
    prompts = workspace / ".ion" / "prompts"
    skill.mkdir(parents=True)
    prompts.mkdir(parents=True)
    (workspace / ".git").mkdir()
    (workspace / "AGENTS.md").write_text("PROJECT_MARKER\n")
    (skill / "SKILL.md").write_text(
        "---\nname: audit\ndescription: Audit code when checking a change.\n"
        "disable-model-invocation: true\n---\nAUDIT_BODY_MARKER\n"
    )
    (prompts / "check.md").write_text(
        "---\ndescription: Check a named concern\n---\nCheck $1; scope ${2:-all}.\n"
    )
    env = os.environ.copy()
    env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        listing = subprocess.run([binary, "--cwd", workspace, "resources"], env=env, check=True, capture_output=True, text=True)
        assert "skill\taudit\t" in listing.stdout and "prompt\tcheck\t" in listing.stdout
        for command in ("/check Rust", "/skill:audit src/lib.rs"):
            result = subprocess.run([binary, "--cwd", workspace, "run", command], env=env, check=True, capture_output=True, text=True)
            assert result.stdout.strip() == "RESOURCE_OK", result
        assert len(requests) == 2, requests
        instructions = requests[0]["messages"][0]["content"]
        assert "PROJECT_MARKER" in instructions
        assert "Audit code when checking a change" not in instructions
        assert requests[0]["messages"][-1]["content"].strip() == "Check Rust; scope all."
        assert "AUDIT_BODY_MARKER" in requests[1]["messages"][-1]["content"]
        assert "User request: src/lib.rs" in requests[1]["messages"][-1]["content"]
        for mode in ("inline", "fullscreen"):
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

            def attach_terminal():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

            terminal_env = {**env, "TERM": "xterm-256color"}
            child = subprocess.Popen([binary, "--cwd", workspace, "--tui-mode", mode, "chat"], env=terminal_env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
            os.close(slave)
            output = bytearray()
            phase = 0
            checkpoint = 0
            before = len(requests)
            deadline = time.monotonic() + 15
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
                    current = output[checkpoint:]
                    if phase == 0 and b"\xe2\x80\xba " in output:
                        checkpoint = len(output)
                        os.write(master, b"/che")
                        phase = 1
                    elif phase == 1 and b"Check a named concern" in current and b"\xe2\x80\xba /che" in current:
                        assert len(requests) == before, "discovery issued a model request"
                        if mode == "inline":
                            assert b"\x1b[?1049h" not in output, "completion took the inline client into alternate screen"
                        os.write(master, b"\x1b")
                        escape_ready = time.monotonic() + 0.12
                        phase = 11
                    elif phase == 11 and time.monotonic() >= escape_ready:
                        # Keep draining output while Escape resolves. Blocking
                        # the PTY consumer can stall redraw and merge queued
                        # Escape/Tab bytes into an Alt-Tab sequence.
                        os.write(master, b"\tTUI\r")
                        phase = 2
                    elif phase == 2 and len(requests) == before + 1 and b"RESOURCE_OK" in current:
                        (prompts / "late.md").write_text("A later prompt.\n")
                        checkpoint = len(output)
                        os.write(master, b"/reload\r")
                        phase = 3
                    elif phase == 3 and b"Reloaded resources" in current:
                        checkpoint = len(output)
                        os.write(master, b"/la")
                        phase = 4
                    elif phase == 4 and b"A later prompt" in current:
                        assert len(requests) == before + 1, "reload/discovery issued a model request"
                        checkpoint = len(output)
                        os.write(master, b"\t\r")
                        phase = 5
                    elif phase == 5 and len(requests) == before + 2 and b"RESOURCE_OK" in current:
                        os.write(master, b"\x03")
                        phase = 6
                    if child.poll() is not None:
                        break
                assert phase == 6, f"{mode} command completion stopped in phase {phase}: {output[-1000:]!r}"
                child.wait(timeout=5)
                assert child.returncode == 0, output[-1000:]
                assert len(requests) == before + 2, "completion duplicated submission"
                assert requests[before]["messages"][-1]["content"].strip() == "Check TUI; scope all."
                assert requests[before + 1]["messages"][-1]["content"].strip() == "A later prompt."
            finally:
                if child.poll() is None:
                    child.send_signal(signal.SIGKILL)
                    child.wait()
                os.close(master)
        print("Ion headless resources and both-mode non-executing command completion/reload: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
