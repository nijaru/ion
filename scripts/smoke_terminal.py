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
    env.update(XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color", ION_SMOKE_STEERING="1")
    port_file, trace = work / "port", work / "requests"
    server = subprocess.Popen([sys.executable, str(root / "scripts/smoke_provider.py"), str(port_file), str(trace)], env=env, stderr=subprocess.PIPE)
    try:
        while not port_file.exists():
            assert time.monotonic() < deadline, "mock provider did not start"
            time.sleep(0.02)
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", f"http://127.0.0.1:{port_file.read_text()}/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        def attach_controlling_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen([binary, "--cwd", workspace, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_controlling_terminal)
        os.close(slave)
        output = bytearray()
        sent_first = sent_steering = sent_second = sent_compact = sent_controls = sent_login = sent_key = sent_logout = sent_quit = False
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
                if sent_first and trace.exists() and not sent_steering:
                    os.write(master, b"Also check that the updated file has one line.\r")
                    sent_steering = True
                if sent_steering and not sent_second:
                    time.sleep(0.1)
                    os.write(master, b"What did we finish previously?\x1b[13;3u")
                    sent_second = True
                if sent_second and b"RESUMED" in output and not sent_compact:
                    time.sleep(0.2)
                    os.write(master, b"/compact\r")
                    sent_compact = True
                if sent_compact and b"Context summarized; raw history retained" in output and not sent_controls:
                    time.sleep(0.2)
                    os.write(master, b"/name Smoke repair\r")
                    time.sleep(0.2)
                    os.write(master, b"/new\r")
                    time.sleep(0.2)
                    os.write(master, b"/resume\r")
                    time.sleep(0.2)
                    os.write(master, b"Smoke repair\r")
                    time.sleep(0.2)
                    os.write(master, b"/model\r")
                    time.sleep(0.2)
                    os.write(master, b"smoke\r")
                    sent_controls = True
                if sent_controls and b"Choose model" in output and b"Resume session" in output and not sent_login:
                    time.sleep(0.2)
                    os.write(master, b"/login smoke\r")
                    sent_login = True
                if sent_login and b"smoke API key:" in output and not sent_key:
                    os.write(master, b"disposable-smoke-key\r")
                    sent_key = True
                if sent_key and b"Credential saved" in output and not sent_logout:
                    os.write(master, b"/logout smoke\r")
                    sent_logout = True
                if sent_logout and b"Removed saved smoke credential" in output and not sent_quit:
                    os.write(master, b"\x03")
                    sent_quit = True
                if child.poll() is not None:
                    break
            assert child.poll() == 0, f"terminal did not exit cleanly: {child.poll()}; tail={output[-2000:]!r}"
            assert b"\x1b[?1049h" in output and b"\x1b[?1049l" in output, "alternate screen was not restored"
            assert sent_first and sent_steering and sent_second and sent_compact and sent_controls and sent_key and sent_logout and sent_quit, "terminal did not complete the session/model/login workflow"
            assert b"disposable-smoke-key" not in output, "masked key leaked to terminal output"
            assert (workspace / "data.txt").read_text() == "sample data updated\n"
            assert (workspace / "created.txt").read_text() == "created by ion\n"
            listing = subprocess.run([binary, "--cwd", workspace, "sessions"], env=env, check=True, capture_output=True, text=True).stdout
            named = [line for line in listing.splitlines() if "Smoke repair" in line]
            assert len(named) == 1 and len(listing.splitlines()) == 2, listing
            session_id = named[0].split("\t")[0]
            inspected = subprocess.run([binary, "--cwd", workspace, "--session", session_id, "inspect"], env=env, check=True, capture_output=True)
            entries = json.loads(inspected.stdout)["entries"]
            assert [entry["kind"] for entry in entries].count("turn_ended") == 2
            assert [entry["kind"] for entry in entries].count("steering") == 1
            assert [entry["kind"] for entry in entries].count("compacted") == 1
            turns = [entry["data"]["prompt"] for entry in entries if entry["kind"] == "turn_started"]
            assert len(turns) == 2 and turns[1] == "What did we finish previously?", turns
            assert entries[-1]["kind"] == "model_selected", entries[-1]
            assert not (work / "config" / "ion" / "credentials" / "smoke.key").exists()
            print("Ion terminal coding, session/model/login and restoration: OK")
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
    finally:
        server.terminate()
        server.wait(timeout=5)
