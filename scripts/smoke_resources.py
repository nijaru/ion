"""Exercise skill and prompt resource expansion through the built headless CLI."""

import json
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
    env.update(XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"))
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
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def attach_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        terminal_env = {**env, "TERM": "xterm-256color"}
        child = subprocess.Popen([binary, "--cwd", workspace, "chat"], env=terminal_env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
        os.close(slave)
        output = bytearray()
        sent = False
        sent_new = False
        sent_prompts = False
        quit_sent = False
        deadline = time.monotonic() + 10
        try:
            while time.monotonic() < deadline:
                readable, _, _ = select.select([master], [], [], 0.05)
                if readable:
                    data = os.read(master, 65536)
                    output.extend(data)
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[2;1R")
                if b"\x1b[?1049h" in output and not sent:
                    os.write(master, b"/check TUI\r")
                    sent = True
                if sent and b"RESOURCE_OK" in output and not sent_new:
                    (prompts / "late.md").write_text("A later prompt.\n")
                    os.write(master, b"/new\r")
                    sent_new = True
                if sent_new and b"Started a new session" in output and not sent_prompts:
                    os.write(master, b"/prompts\r")
                    sent_prompts = True
                if sent_prompts and b"/late" in output and not quit_sent:
                    os.write(master, b"\x03")
                    quit_sent = True
                if quit_sent and b"\x1b[?1049l" in output:
                    break
            child.wait(timeout=5)
            assert child.returncode == 0, output[-1000:]
            assert sent and sent_new and sent_prompts and b"/late" in output and b"\x1b[?1049l" in output
            assert len(requests) == 3, requests
            assert requests[2]["messages"][-1]["content"].strip() == "Check TUI; scope all."
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
        print("Ion headless and terminal skills and prompt templates: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
