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
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []
release = threading.Event()


def user_text(body):
    content = body["messages"][-1]["content"]
    return content if isinstance(content, str) else "".join(part["text"] for part in content if part["type"] == "text")


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        prompt = user_text(body)
        if prompt in ("WAIT_HISTORY", "ACCEPTED_VIEW_FAULT"):
            assert release.wait(10), "fixture did not release the waiting response"
        events = [
            {"choices": [{"index": 0, "delta": {"content": "DONE_" + prompt}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
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
    def __init__(self, cwd, env, mode):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def attach():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        self.child = subprocess.Popen([binary, "--cwd", cwd, "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
        os.close(slave)
        self.output = bytearray()

    def send(self, data):
        os.write(self.master, data.encode() if isinstance(data, str) else data)

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
        end = time.monotonic() + 10
        while self.child.poll() is None:
            assert time.monotonic() < end, self.output[-2500:]
            self.pump()
        assert self.child.returncode == 0, self.output[-2500:]

    def close(self):
        if self.child.poll() is None:
            self.child.kill()
            self.child.wait(timeout=5)
        os.close(self.master)


with tempfile.TemporaryDirectory(prefix="ion-input-") as temporary:
    work = Path(temporary)
    env = {name: value for name, value in os.environ.items() if name in ("PATH", "SHELL", "TMPDIR")}
    env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", f"http://127.0.0.1:{server.server_port}/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        for mode in ("inline", "fullscreen"):
            cwd = work / (mode + "-history")
            cwd.mkdir()
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "› " in terminal.text())
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
            previous = set((work / "state").rglob("*.sqlite"))
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "› " in terminal.text())
                database, = set((work / "state").rglob("*.sqlite")) - previous
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
            previous = set((work / "state").rglob("*.sqlite"))
            terminal = Terminal(cwd, env, mode)
            try:
                terminal.wait(lambda: "› " in terminal.text())
                database, = set((work / "state").rglob("*.sqlite")) - previous
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
