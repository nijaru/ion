"""Exercise real shell job control, not an orphaned PTY process group."""

import errno
import fcntl
import http.server
import json
import os
import pty
import re
import select
import shlex
import signal
import sqlite3
import struct
import subprocess
import tempfile
import termios
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion")).resolve()
CSI = re.compile(rb"\x1b\[([0-?]*)([ -/]*)([@-~])")
PROMPT = b"ION_SHELL> "
CLEAR = b"\x1b]7501;state=clear\x1b\\"
DONE = b"\x1b]7501;state=done:app=ion\x1b\\"


class Terminal:
    def __init__(self, master):
        self.master = master
        self.output = bytearray()
        self.pending = b""
        self.alternate = False
        self.stacks = [[4], [8]]  # Caller-owned keyboard entries must survive.
        self.queries = 0

    def pump(self):
        if not select.select([self.master], [], [], 0.03)[0]:
            return
        try:
            data = os.read(self.master, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            return
        self.output.extend(data)
        self.pending += data
        while match := CSI.search(self.pending):
            parameters, _, final = match.groups()
            self.pending = self.pending[match.end():]
            if final == b"u":
                if parameters == b"?":
                    self.queries += 1
                    os.write(self.master, f"\x1b[?{self.stacks[self.alternate][-1]}u".encode())
                elif parameters.startswith(b">"):
                    self.stacks[self.alternate].append(int(parameters[1:] or b"0"))
                elif parameters.startswith(b"<"):
                    count = int(parameters[1:] or b"1")
                    assert len(self.stacks[self.alternate]) > count, "popped caller keyboard custody"
                    del self.stacks[self.alternate][-count:]
            elif final == b"h" and parameters == b"?1049":
                self.alternate = True
            elif final == b"l" and parameters == b"?1049":
                self.alternate = False
            elif final == b"n" and parameters == b"6":
                os.write(self.master, b"\x1b[2;1R")
        start = self.pending.rfind(b"\x1b")
        self.pending = self.pending[start:] if start >= 0 else b""

    def until(self, predicate):
        deadline = time.monotonic() + 10
        while not predicate():
            assert time.monotonic() < deadline, self.output[-3000:]
            self.pump()

    def send(self, data):
        os.write(self.master, data.encode() if isinstance(data, str) else data)


def exercise(mode):
    with tempfile.TemporaryDirectory(prefix="ion-job-control-") as temporary:
        work = Path(temporary)
        requests = []
        release = threading.Event()
        response_sent = threading.Event()

        class Provider(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                if len(requests) == 1:
                    assert release.wait(15), "response was not released"
                payload = (
                    b'data: {"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":null}]}\n\n'
                    b'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n'
                )
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                self.wfile.flush()
                response_sent.set()

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL") if key in os.environ}
        env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), TMPDIR=str(work),
                   XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"),
                   TERM="xterm-256color", PS1=PROMPT.decode(), PS2="CONT> ")
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        original = termios.tcgetattr(slave)

        def attach():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        shell = None
        job = None
        try:
            subprocess.run([BINARY, "use", "smoke", "fixture", "--endpoint",
                            f"http://127.0.0.1:{server.server_port}/v1", "--wire", "chat-completions"],
                           env=env, capture_output=True, check=True)
            # A parent shell in the same session makes Ion's group non-orphaned.
            # No editing/startup files: the shell's prompt leaves cooked modes observable.
            shell = subprocess.Popen(["/bin/bash", "--noprofile", "--norc", "--noediting", "-i"],
                                     env=env, cwd=work, stdin=slave, stdout=slave, stderr=slave,
                                     preexec_fn=attach)
            terminal = Terminal(master)
            terminal.until(lambda: PROMPT in terminal.output)
            command = shlex.join([str(BINARY), "--cwd", str(work), "chat", "--tui-mode", mode])
            terminal.send(command + "\n")
            terminal.until(lambda: "›".encode() in terminal.output and terminal.queries == 1)
            job = os.tcgetpgrp(master)
            assert job != shell.pid and job > 0, "Ion is not a distinct foreground job"

            def suspend(key):
                checkpoint = len(terminal.output)
                terminal.send(key)
                terminal.until(lambda: PROMPT in terminal.output[checkpoint:])
                state = subprocess.check_output(["ps", "-o", "stat=", "-p", str(job)]).strip()
                assert b"T" in state, (job, state)
                assert os.tcgetpgrp(master) == shell.pid
                assert termios.tcgetattr(slave) == original, "shell did not regain cooked termios"
                assert terminal.stacks == [[4], [8]] and not terminal.alternate
                assert CLEAR in terminal.output[checkpoint:], "suspended job left a stale status record"
                checkpoint = len(terminal.output)
                terminal.send("printf 'SHELL_%s\\n' INPUT_OK\n")
                terminal.until(lambda: b"SHELL_INPUT_OK" in terminal.output[checkpoint:])

            def foreground():
                queries = terminal.queries
                terminal.send("fg\n")
                terminal.until(lambda: terminal.queries == queries + 1 and
                               terminal.stacks[terminal.alternate][-1] == 1)
                assert os.tcgetpgrp(master) == job, "fg replaced the original job"

            terminal.send("\x1b[200~  keep 🦀\ntail\x1b[201~")
            terminal.send(b"\x1b[D" * 4)
            suspend(b"\x1a")
            assert not requests, "suspension admitted an unsent draft"
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 84, 0, 0))
            # `bg` must not let an interactive job steal the shell terminal.
            checkpoint = len(terminal.output)
            terminal.send("bg\n")
            terminal.until(lambda: PROMPT in terminal.output[checkpoint:])
            terminal.until(lambda: b"T" in subprocess.check_output(
                ["ps", "-o", "stat=", "-p", str(job)]))
            assert termios.tcgetattr(slave) == original
            assert terminal.stacks == [[4], [8]] and not terminal.alternate
            foreground()
            terminal.send("X\r")
            terminal.until(lambda: len(requests) == 1)
            assert requests[0]["messages"][-1]["content"] == "  keep 🦀\nXtail", requests[0]

            terminal.send("follow 🦀\x1b[D")
            suspend(b"\x1b[122;5u")  # Enhanced-keyboard Ctrl-Z while a Turn is running.
            release.set()
            assert response_sent.wait(5), "fixture response did not finish while Ion was stopped"
            checkpoint = len(terminal.output)
            foreground()
            terminal.until(lambda: DONE in terminal.output[checkpoint:])
            assert len(requests) == 1, "resuming replayed the running request"
            terminal.send("X\r")
            terminal.until(lambda: len(requests) == 2 and terminal.output[checkpoint:].count(DONE) == 2)
            assert requests[-1]["messages"][-1]["content"] == "follow X🦀", requests[-1]
            checkpoint = len(terminal.output)
            terminal.send(b"\x04")
            terminal.until(lambda: PROMPT in terminal.output[checkpoint:])
            assert terminal.stacks == [[4], [8]] and not terminal.alternate
            terminal.send("exit\n")
            terminal.until(lambda: shell.poll() is not None)
            assert shell.wait() == 0, terminal.output[-2000:]
            job = None
            database, = (work / "ion").rglob("*.sqlite")
            with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
                facts = [json.loads(body) for body, in connection.execute("SELECT body FROM entries ORDER BY seq")]
            endings = [entry for entry in facts if entry["kind"] == "turn_ended"]
            assert len(endings) == 2 and all(entry["data"]["reason"] == "completed" for entry in endings), endings
            assert len([entry for entry in facts if entry["kind"] == "turn_started"]) == 2
            print(f"Ion {mode} Ctrl-Z/fg: cooked shell, retained cursor, resize, busy no-replay: OK")
        finally:
            release.set()
            if job is not None:
                try:
                    os.killpg(job, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if shell is not None:
                if shell.poll() is None:
                    shell.kill()
                shell.wait(timeout=5)
            os.close(master)
            os.close(slave)
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    exercise("inline")
    exercise("fullscreen")
