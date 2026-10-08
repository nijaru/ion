"""Qualify actual client surface transitions against independent Kitty stacks."""
import errno
import fcntl
import http.server
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))
CSI = re.compile(rb"\x1b\[([0-?]*)([ -/]*)([@-~])")


class KeyboardTerminal:
    def __init__(self, master, supported, typeahead=False):
        self.master, self.supported = master, supported
        self.typeahead = typeahead
        self.keyboard_queries = self.cursor_queries = 0
        self.alternate = False
        # The caller already owns entries on both independent surfaces.
        self.stacks = [[4], [8]]
        self.pending = b""

    def accept(self, data):
        self.pending += data
        while match := CSI.search(self.pending):
            parameters, _, final = match.groups()
            self.pending = self.pending[match.end():]
            if final == b"u":
                if parameters == b"?":
                    self.keyboard_queries += 1
                    if self.supported:
                        if self.typeahead and self.keyboard_queries == 1:
                            os.write(self.master, b"START_")
                        os.write(self.master, f"\x1b[?{self.stacks[self.alternate][-1]}u".encode())
                elif parameters.startswith(b">"):
                    self.stacks[self.alternate].append(int(parameters[1:] or b"0"))
                elif parameters.startswith(b"<"):
                    count = int(parameters[1:] or b"1")
                    assert len(self.stacks[self.alternate]) > count, "popped caller's keyboard custody"
                    del self.stacks[self.alternate][-count:]
            elif final == b"h" and parameters == b"?1049":
                self.alternate = True
            elif final == b"l" and parameters == b"?1049":
                self.alternate = False
            elif final == b"n" and parameters == b"6":
                self.cursor_queries += 1
                if self.typeahead and self.cursor_queries == 2:
                    os.write(self.master, b"RESUME_")
                os.write(self.master, b"\x1b[2;1R")
            elif final == b"c" and parameters in (b"", b"0") and not self.supported:
                os.write(self.master, b"\x1b[?1;2c")
        # Preserve an incomplete escape sequence, not arbitrary rendered text.
        start = self.pending.rfind(b"\x1b")
        self.pending = self.pending[start:] if start >= 0 else b""


def exercise(mode, supported=True, panic=False, editor_fault=None):
    with tempfile.TemporaryDirectory(prefix="ion-keyboard-") as temporary:
        work = Path(temporary)
        requests = []

        class Provider(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                body = b'data: {"choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n'
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "TMPDIR") if key in os.environ}
        env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
        marker = work / "editor-flags"
        editor = work / "editor"
        editor.write_text(
            "#!/usr/bin/env python3\n"
            "import os,re,termios,tty\n"
            "old=termios.tcgetattr(0)\n"
            "try:\n"
            " tty.setraw(0); os.write(1,b'\\x1b[?u'); data=b''\n"
            " while not re.search(rb'\\x1b\\[\\?(\\d+)u',data): data+=os.read(0,128)\n"
            f" open({str(marker)!r},'w').write(re.search(rb'\\x1b\\[\\?(\\d+)u',data)[1].decode())\n"
            "finally: termios.tcsetattr(0,termios.TCSANOW,old)\n"
        )
        if editor_fault:
            with editor.open("a") as script:
                script.write(
                    "import sys\n"
                    "path=sys.argv[-1]; os.unlink(path)\n"
                    f"pipe=path+'.pipe' if {editor_fault!r} == 'symlink-fifo' else path\n"
                    "os.mkfifo(pipe)\n"
                    "if pipe != path: os.symlink(pipe,path)\n"
                )
        editor.chmod(0o755)
        env.update(EDITOR=str(editor), VISUAL=str(editor), TMPDIR=str(work))
        if panic:
            env["ION_SMOKE_PANIC_AFTER_FIRST_DRAW"] = "1"
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = None
        try:
            subprocess.run([BINARY, "use", "smoke", "fixture", "--endpoint", f"http://127.0.0.1:{server.server_port}/v1", "--wire", "chat-completions"], env=env, capture_output=True, check=True)
            child = subprocess.Popen([BINARY, "--cwd", work, "chat", "--tui-mode", mode], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=controlling_terminal)
            os.close(slave)
            slave = None
            terminal = KeyboardTerminal(master, supported, supported and not panic)
            output = bytearray()

            def drain(seconds=0.1):
                end = time.monotonic() + seconds
                while time.monotonic() < end:
                    if select.select([master], [], [], min(0.02, end - time.monotonic()))[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            return
                        if not data:
                            return
                        output.extend(data)
                        terminal.accept(data)

            def until(predicate):
                deadline = time.monotonic() + 8
                while not predicate():
                    assert time.monotonic() < deadline, output[-2000:]
                    drain()

            if panic:
                until(lambda: child.poll() is not None)
                drain()
                assert child.wait() != 0
                assert b"ION smoke panic" in output
            else:
                until(lambda: "› ".encode() in output)
                expected = 1 if supported else (8 if mode == "fullscreen" else 4)
                assert terminal.stacks[terminal.alternate][-1] == expected, "composer lacks its own keyboard mode"
                if supported:
                    os.write(master, b"first\x1b[13;2usecond")
                    drain(0.25)
                    assert not requests, "Shift-Enter admitted a Turn instead of a draft newline"
                    os.write(master, b"\r")
                    until(lambda: len(requests) == 1 and b"done" in output)
                    drain()
                    users = [message for message in requests[0]["messages"] if message["role"] == "user"]
                    assert users[-1]["content"] == "START_first\nsecond", users[-1]
                    if editor_fault:
                        # Keep a multiline literal and mid-line cursor through
                        # editor rejection; resume type-ahead inserts at it.
                        os.write(master, b"  retained\x1b[13;2udraft\x1b[D\x07")
                    else:
                        os.write(master, b"/editor\r")
                    until(lambda: marker.exists() and terminal.stacks[terminal.alternate][-1] == 1)
                    assert marker.read_text() == "4", "editor inherited Ion's keyboard push"
                    until(lambda: terminal.cursor_queries == 2)
                    checkpoint = len(output)
                    os.write(master, b"after\r")
                    until(lambda: len(requests) == 2 and b"done" in output[checkpoint:])
                    users = [message for message in requests[-1]["messages"] if message["role"] == "user"]
                    expected_input = "  retained\ndrafRESUME_aftert" if editor_fault else "RESUME_after"
                    assert users[-1]["content"] == expected_input, users[-1]
                    if editor_fault:
                        assert b"not a regular file" in output, "editor special-file failure was not reported"
                if mode == "inline":
                    os.write(master, b"\x0f")
                    until(lambda: terminal.alternate)
                    drain()
                    assert terminal.stacks[1][-1] == (1 if supported else 8)
                    os.write(master, b"\x0f")
                    until(lambda: not terminal.alternate)
                os.write(master, b"\x03")
                until(lambda: child.poll() is not None)
                drain()
                assert child.wait() == 0, output[-2000:]
            assert not terminal.alternate
            assert terminal.stacks == [[4], [8]], terminal.stacks
            outcome = editor_fault or ('panic' if panic else 'draft/editor/modal/exit')
            print(f"Ion {mode} {'supported' if supported else 'unsupported'} keyboard custody {outcome}: OK")
        finally:
            if child is not None and child.poll() is None:
                child.kill()
            if slave is not None:
                os.close(slave)
            os.close(master)
            if child is not None:
                child.wait(timeout=5)
            server.shutdown()
            server.server_close()
            thread.join()


exercise("inline")
exercise("fullscreen")
exercise("fullscreen", panic=True)
exercise("fullscreen", supported=False, panic=True)
exercise("inline", editor_fault="fifo")
exercise("fullscreen", editor_fault="symlink-fifo")
