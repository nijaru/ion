"""Exercise direct user shell commands in the terminal and persisted Session."""

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
import time
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
with tempfile.TemporaryDirectory(prefix="ion-shell-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "TERM": "xterm-256color"}
    subprocess.run([binary, "use", "smoke", "shell-model", "--endpoint", "http://127.0.0.1:9/v1/chat/completions", "--wire", "chat-completions"], env=env, capture_output=True, check=True)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

    def attach_terminal():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    child = subprocess.Popen([binary, "--cwd", workspace, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
    os.close(slave)
    output = bytearray()
    step = 0
    deadline = time.monotonic() + 12
    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select([master], [], [], 0.05)
            if readable:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    data = b""
                output.extend(data)
                if b"\x1b[6n" in data:
                    os.write(master, b"\x1b[2;1R")
            if step == 0 and b"\x1b[?1049h" in output:
                os.write(master, b"!printf 'visible\\n'\r")
                step = 1
            if step == 1 and b"Shell finished" in output:
                os.write(master, b"!!printf 'private\\n'\r")
                step = 2
            if step == 2 and output.count(b"Shell finished") >= 2:
                os.write(master, b"\x03")
                step = 3
            if child.poll() is not None:
                break
        assert child.poll() == 0, f"terminal failed: {child.poll()}, {output[-1000:]!r}"
        assert step == 3, output[-1000:]
        assert b"\x1b[?1049l" in output
        listing = subprocess.run([binary, "--cwd", workspace, "sessions"], env=env, capture_output=True, text=True, check=True).stdout
        session_id = listing.split("\t")[0]
        inspect = subprocess.run([binary, "--cwd", workspace, "--session", session_id, "inspect"], env=env, capture_output=True, text=True, check=True).stdout
        entries = [entry["data"] for entry in json.loads(inspect)["entries"] if entry["kind"] == "user_shell"]
        assert len(entries) == 2, entries
        assert entries[0]["output"]["stdout"] == "visible\n" and not entries[0]["exclude_from_context"]
        assert entries[1]["output"]["stdout"] == "private\n" and entries[1]["exclude_from_context"]
        resumed = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, capture_output=True, text=True, check=True)
        assert len([entry for entry in json.loads(resumed.stdout)["entries"] if entry["kind"] == "user_shell"]) == 2
        print("Ion direct shell and context choice: OK")
    finally:
        if child.poll() is None:
            child.send_signal(signal.SIGKILL)
            child.wait()
        os.close(master)
