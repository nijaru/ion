"""Exercise visible shell outcomes, context choice and inspection in both TUIs."""

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
for mode in ("inline", "fullscreen"):
    with tempfile.TemporaryDirectory(prefix=f"ion-shell-{mode}-") as temporary:
        work = Path(temporary)
        workspace = work / "workspace"
        workspace.mkdir()
        env = {
            **os.environ,
            "XDG_CONFIG_HOME": str(work / "config"),
            "XDG_STATE_HOME": str(work / "state"),
            "TERM": "xterm-256color",
            "VISUAL": "/bin/sh -c 'printf edited-from-editor > \"$0\"'",
        }
        subprocess.run([binary, "use", "smoke", "shell-model", "--endpoint", "http://127.0.0.1:9/v1/chat/completions", "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def attach_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen([binary, "--cwd", workspace, "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
        os.close(slave)
        output = bytearray()
        step = 0
        segment_start = 0
        deadline = time.monotonic() + 20
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
                segment = output[segment_start:]
                if step == 0 and "› ".encode() in output:
                    assert (b"\x1b[?1049h" in output) == (mode == "fullscreen")
                    os.write(master, b"original draft\x07")
                    step = 1
                if step == 1 and b"Draft returned from editor" in output and b"edited-from-editor" in output:
                    segment_start = len(output)
                    # The complete markers below are not present in the commands:
                    # composer echo cannot satisfy visible-output assertions.
                    os.write(master, b"\x03!printf '%s%s\\n' VISIBLE_ OUTPUT\r")
                    step = 2
                if step == 2 and b"VISIBLE_OUTPUT" in segment and b"exit 0" in segment:
                    segment_start = len(output)
                    os.write(master, b"!printf '%s%s\\n' FAILED_ STDERR >&2; exit 7\r")
                    step = 3
                if step == 3 and b"FAILED_STDERR" in segment and b"exit 7" in segment:
                    segment_start = len(output)
                    os.write(master, b"!!printf '%s%s\\n' PRIVATE_ OUTPUT\r")
                    step = 4
                if step == 4 and b"PRIVATE_OUTPUT" in segment and b"not shared with model" in segment:
                    segment_start = len(output)
                    os.write(master, b"\x0f")
                    step = 5
                if step == 5 and b"Conversation" in segment and b"PRIVATE_OUTPUT" in segment and b"User shell" in segment:
                    os.write(master, b"\x0f")
                    step = 6
                if step == 6 and b"Details closed" in segment:
                    os.write(master, b"/export session.txt\r")
                    step = 7
                if step == 7 and b"Transcript saved to" in output:
                    os.write(master, b"\x03")
                    step = 8
                if child.poll() is not None:
                    break
            assert child.poll() == 0, f"{mode}: terminal failed: {child.poll()}, step={step}, {output[-1000:]!r}"
            assert step == 8, output[-1000:]
            assert b"\x1b[?1049l" in output
            listing = subprocess.run([binary, "--cwd", workspace, "sessions"], env=env, capture_output=True, text=True, check=True).stdout
            session_id = listing.split("\t")[0]
            inspect = subprocess.run([binary, "--cwd", workspace, "--session", session_id, "inspect"], env=env, capture_output=True, text=True, check=True).stdout
            entries = [entry["data"] for entry in json.loads(inspect)["entries"] if entry["kind"] == "user_shell"]
            assert len(entries) == 3, entries
            assert entries[0]["output"]["stdout"] == "VISIBLE_OUTPUT\n" and not entries[0]["exclude_from_context"]
            assert entries[1]["output"]["stderr"] == "FAILED_STDERR\n" and entries[1]["output"]["exit_code"] == 7 and entries[1]["is_error"]
            assert entries[2]["output"]["stdout"] == "PRIVATE_OUTPUT\n" and entries[2]["exclude_from_context"]
            transcript = (workspace / "session.txt").read_text()
            assert "VISIBLE_OUTPUT" in transcript and "PRIVATE_OUTPUT" in transcript and "not shared with model" in transcript
            assert (workspace / "session.txt").stat().st_mode & 0o777 == 0o600
            resumed = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, capture_output=True, text=True, check=True)
            assert len([entry for entry in json.loads(resumed.stdout)["entries"] if entry["kind"] == "user_shell"]) == 3
            exported = subprocess.run([binary, "--cwd", workspace, "--continue", "export"], env=env, capture_output=True, text=True, check=True)
            assert "User shell" in exported.stdout and "PRIVATE_OUTPUT" in exported.stdout
            refused = subprocess.run([binary, "--cwd", workspace, "--continue", "export", "session.txt"], env=env, capture_output=True, text=True)
            assert refused.returncode != 0 and "cannot save transcript" in refused.stderr
            print(f"Ion {mode} shell output, context choice and inspection: OK")
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
