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
    (workspace / "data.txt").write_text("sample data\nsecond token\n")
    copy_bin = work / "copy-bin"
    copy_bin.mkdir()
    copy_file = work / "copied.txt"
    for program in ("pbcopy", "wl-copy", "xclip", "xsel"):
        wrapper = copy_bin / program
        wrapper.write_text('#!/bin/sh\ncat > "$ION_SMOKE_CLIPBOARD"\n')
        wrapper.chmod(0o755)
    env = os.environ.copy()
    env.update(XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color", ION_SMOKE_STEERING="1", SSH_CONNECTION="ion-smoke", WAYLAND_DISPLAY="ion-smoke", ION_SMOKE_CLIPBOARD=str(copy_file), PATH=f"{copy_bin}:{env['PATH']}")
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
        sent_file_start = selected_file = sent_first = sent_steering = sent_second = resized = sent_tool = closed_tool = sent_compact = sent_clone = sent_controls = sent_login = sent_key = sent_logout = sent_copy = sent_quit = False
        saw_inline_start = False
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
                if b"\xe2\x80\xba " in output and not sent_file_start:
                    assert b"\x1b[?1049h" not in output, "chat entered alternate screen before showing the inline composer"
                    saw_inline_start = True
                    os.write(master, b"Read @")
                    sent_file_start = True
                if sent_file_start and b"Choose file" in output and not selected_file:
                    os.write(master, b"data.txt\r")
                    selected_file = True
                if selected_file and b"Read @data.txt" in output and not sent_first:
                    os.write(master, b", edit it, create created.txt, then verify both files with shell.\r")
                    sent_first = True
                if sent_first and trace.exists() and not sent_steering:
                    os.write(master, b"Also check that the updated file has two lines.\r")
                    sent_steering = True
                if sent_steering and not sent_second:
                    time.sleep(0.1)
                    os.write(master, b"What did we finish previously?\x1b")
                    time.sleep(0.02)
                    os.write(master, b"[13;3u")
                    sent_second = True
                if sent_second and b"RESUMED" in output and not resized:
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 7, 24, 0, 0))
                    os.kill(child.pid, signal.SIGWINCH)
                    time.sleep(0.1)
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
                    os.kill(child.pid, signal.SIGWINCH)
                    resized = True
                if resized and not sent_tool:
                    time.sleep(0.2)
                    os.write(master, b"\x0f")
                    sent_tool = True
                if sent_tool and b"Tool 4: exec" in output and not closed_tool:
                    os.write(master, b"\x0f")
                    closed_tool = True
                if closed_tool and b"Tool output closed" in output and not sent_compact:
                    time.sleep(0.2)
                    os.write(master, b"/compact\r")
                    sent_compact = True
                if sent_compact and b"Context summarized; raw history retained" in output and not sent_clone:
                    os.write(master, b"/clone\r")
                    sent_clone = True
                if sent_clone and b"Cloned conversation as" in output and not sent_controls:
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
                if sent_logout and b"Removed saved smoke credential" in output and not sent_copy:
                    os.write(master, b"\x18")
                    sent_copy = True
                if sent_copy and b"Copied last assistant answer" in output and not sent_quit:
                    os.write(master, b"\x03")
                    sent_quit = True
                if child.poll() is not None:
                    break
            assert child.poll() == 0, f"terminal did not exit cleanly: {child.poll()}; tail={output[-2000:]!r}"
            alt_enters = output.count(b"\x1b[?1049h")
            alt_leaves = output.count(b"\x1b[?1049l")
            assert saw_inline_start, "inline composer never appeared before modal interaction"
            assert alt_enters > 0 and alt_enters == alt_leaves, (alt_enters, alt_leaves)
            assert b"\xe2\x97\x8f " in output, "semantic activity group header was not rendered"
            assert b"\xe2\x94\x9c " in output or b"\xe2\x94\x94 " in output, "grouped tool tree was not rendered"
            assert sent_file_start and selected_file and sent_first and sent_steering and sent_second and resized and sent_tool and closed_tool and sent_compact and sent_clone and sent_controls and sent_key and sent_logout and sent_copy and sent_quit, "terminal did not complete the session/model/login workflow"
            assert "RESUMED" in copy_file.read_text(), "copy did not use the committed assistant answer"
            assert b"disposable-smoke-key" not in output, "masked key leaked to terminal output"
            assert (workspace / "data.txt").read_text() == "sample data updated\nsecond token updated\n"
            assert (workspace / "created.txt").read_text() == "created by ion\n"
            listing = subprocess.run([binary, "--cwd", workspace, "sessions"], env=env, check=True, capture_output=True, text=True).stdout
            named = [line for line in listing.splitlines() if "Smoke repair" in line]
            assert len(named) == 1 and len(listing.splitlines()) == 2, listing
            session_id = named[0].split("\t")[0]
            inspected = subprocess.run([binary, "--cwd", workspace, "--session", session_id, "inspect"], env=env, check=True, capture_output=True)
            entries = json.loads(inspected.stdout)["entries"]
            source = [line for line in listing.splitlines() if "\t\t2 turn(s)" in line]
            assert len(source) == 1, listing
            original = subprocess.run([binary, "--cwd", workspace, "--session", source[0].split("\t")[0], "inspect"], env=env, check=True, capture_output=True)
            assert json.loads(original.stdout)["entries"] == entries[:-1], "clone changed the source transcript"
            assert [entry["kind"] for entry in entries].count("turn_ended") == 2
            assert [entry["kind"] for entry in entries].count("steering") == 1
            assert [entry["kind"] for entry in entries].count("compacted") == 1
            turns = [entry["data"]["input"]["content"][0]["Text"] for entry in entries if entry["kind"] == "turn_started"]
            assert len(turns) == 2 and turns[1] == "What did we finish previously?", turns
            assert entries[-1]["kind"] == "model_selected", entries[-1]
            assert not (work / "config" / "ion" / "credentials" / "smoke.key").exists()
            print("Ion inline terminal, grouped activity, transient modals and restoration: OK")
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
    finally:
        server.terminate()
        server.wait(timeout=5)
