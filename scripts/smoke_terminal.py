"""Exercise a coding turn through Ion's actual terminal client and a PTY."""

import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
deadline = time.monotonic() + 30
with tempfile.TemporaryDirectory(prefix="ion-terminal-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "data.txt").write_text("sample data\n")
    env = os.environ.copy()
    env.update(XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
    port_file, trace = work / "port", work / "requests"
    server = subprocess.Popen([sys.executable, str(root / "scripts/smoke_provider.py"), str(port_file), str(trace)], stderr=subprocess.PIPE)
    try:
        while not port_file.exists():
            assert time.monotonic() < deadline, "mock provider did not start"
            time.sleep(0.02)
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", f"http://127.0.0.1:{port_file.read_text()}/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        child = subprocess.Popen([binary, "--cwd", workspace, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        output = bytearray()
        sent_first = sent_second = sent_quit = False
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
                if b"\x1b[?1049h" in output and not sent_first:
                    os.write(master, b"Read data.txt, edit it, create created.txt, then verify both files with shell.\r")
                    sent_first = True
                if b"TASK_COMPLETE" in output and b"Ready" in output and not sent_second:
                    os.write(master, b"What did we finish previously?\r")
                    sent_second = True
                if sent_second and b"RESUMED" in output and not sent_quit:
                    time.sleep(0.2)
                    os.write(master, b"\x03")
                    sent_quit = True
                if child.poll() is not None:
                    break
            assert child.poll() == 0, f"terminal did not exit cleanly: {child.poll()}; tail={output[-2000:]!r}"
            assert b"\x1b[?1049h" in output and b"\x1b[?1049l" in output, "alternate screen was not restored"
            assert sent_first and sent_second and sent_quit, "terminal did not complete both prompts"
            assert (workspace / "data.txt").read_text() == "sample data updated\n"
            assert (workspace / "created.txt").read_text() == "created by ion\n"
            inspected = subprocess.run([binary, "--cwd", workspace, "inspect"], env=env, check=True, capture_output=True)
            entries = json.loads(inspected.stdout)["entries"]
            assert [entry["kind"] for entry in entries].count("turn_ended") == 2
            assert entries[-1]["data"]["reason"] == "completed"
            print("Ion terminal coding, continuation and restoration: OK")
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
    finally:
        server.terminate()
        server.wait(timeout=5)
