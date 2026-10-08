"""Exercise a coding turn through Ion's actual terminal client and a PTY."""

import errno
import fcntl
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path


def drain_terminal(fd, output):
    """Collect queued restoration bytes after process exit, through PTY closure."""
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        readable, _, _ = select.select([fd], [], [], 0.05)
        if not readable:
            continue
        try:
            data = os.read(fd, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            return
        if not data:
            return
        output.extend(data)
    raise AssertionError("terminal PTY stayed open after process exit")


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
        sent_help = False
        help_start = 0
        opened_active = closed_active = False
        sent_selected = closed_selected = False
        active_start = tool_start = selected_start = 0
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
                if sent_first and trace.exists() and not opened_active:
                    active_start = len(output)
                    os.write(master, b"\x0f")
                    opened_active = True
                if opened_active and not closed_active and b"Conversation" in output[active_start:]:
                    assert b"Read @data.txt" in output[active_start:], "active first-Turn input missing from conversation detail"
                    os.write(master, b"\x0f")
                    closed_active = True
                if closed_active and b"Details closed" in output[active_start:] and not sent_steering:
                    assert "Working · Ctrl-C cancels · Details closed".encode() in output[active_start:], "detail-close notice hid the active operation"
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
                    tool_start = len(output)
                    os.write(master, b"\x0f")
                    sent_tool = True
                if sent_tool and b"Conversation" in output[tool_start:] and b"RESUMED" in output[tool_start:] and not closed_tool:
                    os.write(master, b"\x0f")
                    closed_tool = True
                if closed_tool and b"Details closed" in output[tool_start:] and not sent_selected:
                    selected_start = len(output)
                    os.write(master, b"/tool\r")
                    sent_selected = True
                if sent_selected and b"Tool 4" in output[selected_start:] and b"sample data updated" in output[selected_start:] and not closed_selected:
                    os.write(master, b"\x0f")
                    closed_selected = True
                if closed_selected and b"Details closed" in output[selected_start:] and not sent_compact:
                    time.sleep(0.2)
                    os.write(master, b"/compact\r")
                    sent_compact = True
                if sent_compact and b"Context summarized; raw history retained" in output and not sent_clone:
                    snapshot = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, check=True, capture_output=True)
                    source_before_clone = json.loads(snapshot.stdout)["entries"]
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
                if sent_logout and b"Removed saved smoke credential" in output and not sent_help:
                    help_start = len(output)
                    os.write(master, b"/help\r")
                    sent_help = True
                if sent_help and b"Ctrl-V pastes" in output[help_start:] and b"!!COMMAND" in output[help_start:] and not sent_copy:
                    os.write(master, b"\x18")
                    sent_copy = True
                if sent_copy and b"Copied last assistant answer" in output and not sent_quit:
                    os.write(master, b"\x03")
                    sent_quit = True
                if child.poll() is not None:
                    break
            if child.poll() is not None:
                drain_terminal(master, output)
            assert closed_active, "Ctrl-O did not open the first-Turn conversation"
            assert child.poll() == 0, f"terminal did not exit cleanly: {child.poll()}; tail={output[-2000:]!r}"
            alt_enters = output.count(b"\x1b[?1049h")
            alt_leaves = output.count(b"\x1b[?1049l")
            assert saw_inline_start, "inline composer never appeared before modal interaction"
            assert alt_enters > 0 and alt_enters == alt_leaves, (alt_enters, alt_leaves)
            plain_output = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output))
            assert b"MARKDOWN_BOLD and literal_code docs (https://example.org/ion)" in plain_output, "assistant Markdown was not rendered"
            assert b"-sample data" in plain_output and b"+sample data updated" in plain_output, "actual edit patch was not rendered"
            assert "• ".encode() in output, "semantic activity group header was not rendered"
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
            assert "**MARKDOWN_BOLD**" in str(entries), "formatted view replaced the original saved Markdown"
            edits = [entry["data"]["result"]["outcome"]["output"]["value"]["diff"] for entry in entries if entry["kind"] == "tool_result" and entry["data"]["result"]["outcome"]["state"] == "observed" and "diff" in entry["data"]["result"]["outcome"]["output"]["value"]]
            assert len(edits) == 1 and not edits[0]["truncated"], edits
            assert "-sample data\n-second token\n+sample data updated\n+second token updated\n" in edits[0]["text"], edits
            exported = subprocess.run([binary, "--cwd", workspace, "--session", session_id, "export"], env=env, check=True, capture_output=True, text=True).stdout
            assert "**MARKDOWN_BOLD**" in exported, "export lost the original Markdown source"
            source = [line for line in listing.splitlines() if "\t\t2 turn(s)" in line]
            assert len(source) == 1, listing
            original = subprocess.run([binary, "--cwd", workspace, "--session", source[0].split("\t")[0], "inspect"], env=env, check=True, capture_output=True)
            assert json.loads(original.stdout)["entries"] == source_before_clone, "clone changed the source transcript"
            assert entries == source_before_clone, "clone changed the copied transcript"
            assert [entry["kind"] for entry in entries].count("turn_ended") == 2
            assert [entry["kind"] for entry in entries].count("steering") == 1
            assert [entry["kind"] for entry in entries].count("compacted") == 1
            turns = [entry["data"]["input"]["content"][0]["Text"] for entry in entries if entry["kind"] == "turn_started"]
            assert len(turns) == 2 and turns[1] == "What did we finish previously?", turns
            assert json.loads(inspected.stdout)["last_model"] == {"provider": "smoke", "model": "smoke-model"}
            assert not (work / "config" / "ion" / "credentials" / "smoke.key").exists()

            fullscreen_master, fullscreen_slave = pty.openpty()
            fcntl.ioctl(fullscreen_slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

            def attach_fullscreen_terminal():
                os.setsid()
                fcntl.ioctl(fullscreen_slave, termios.TIOCSCTTY, 0)

            fullscreen = subprocess.Popen(
                [binary, "--cwd", workspace, "--tui-mode", "fullscreen", "chat"],
                env=env,
                stdin=fullscreen_slave,
                stdout=fullscreen_slave,
                stderr=fullscreen_slave,
                preexec_fn=attach_fullscreen_terminal,
            )
            os.close(fullscreen_slave)
            fullscreen_output = bytearray()
            sent_inline_mode = sent_fullscreen_mode = sent_fullscreen_quit = False
            fullscreen_deadline = time.monotonic() + 8
            try:
                while time.monotonic() < fullscreen_deadline:
                    readable, _, _ = select.select([fullscreen_master], [], [], 0.05)
                    if readable:
                        try:
                            data = os.read(fullscreen_master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            data = b""
                        fullscreen_output.extend(data)
                        if b"\x1b[6n" in data:
                            os.write(fullscreen_master, b"\x1b[2;1R")
                    if (
                        b"\x1b[?1049h" in fullscreen_output
                        and b"\xe2\x80\xba " in fullscreen_output
                        and not sent_inline_mode
                    ):
                        os.write(fullscreen_master, b"/tui inline\r")
                        sent_inline_mode = True
                    if (
                        sent_inline_mode
                        and b"Inline TUI" in fullscreen_output
                        and b"\x1b[?1049l" in fullscreen_output
                        and not sent_fullscreen_mode
                    ):
                        os.write(fullscreen_master, b"/tui fullscreen\r")
                        sent_fullscreen_mode = True
                    if (
                        sent_fullscreen_mode
                        and fullscreen_output.count(b"\x1b[?1049h") >= 2
                        and b"Fullscreen TUI" in fullscreen_output
                        and not sent_fullscreen_quit
                    ):
                        os.write(fullscreen_master, b"\x03")
                        sent_fullscreen_quit = True
                    if fullscreen.poll() is not None:
                        break
                if fullscreen.poll() is not None:
                    drain_terminal(fullscreen_master, fullscreen_output)
                assert fullscreen.poll() == 0, (
                    f"fullscreen terminal did not exit cleanly: {fullscreen.poll()}; "
                    f"tail={fullscreen_output[-2000:]!r}"
                )
                enters = fullscreen_output.count(b"\x1b[?1049h")
                leaves = fullscreen_output.count(b"\x1b[?1049l")
                assert sent_inline_mode and sent_fullscreen_mode and sent_fullscreen_quit
                assert enters >= 2 and enters == leaves, (enters, leaves)
            finally:
                if fullscreen.poll() is None:
                    fullscreen.send_signal(signal.SIGKILL)
                    fullscreen.wait()
                os.close(fullscreen_master)

            panic_master, panic_slave = pty.openpty()
            fcntl.ioctl(panic_slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

            def attach_panic_terminal():
                os.setsid()
                fcntl.ioctl(panic_slave, termios.TIOCSCTTY, 0)

            panic = subprocess.Popen(
                [binary, "--cwd", workspace, "--tui-mode", "fullscreen", "chat"],
                env={
                    **env,
                    "ION_SMOKE_PANIC_AFTER_FIRST_DRAW": "1",
                    "RUST_BACKTRACE": "0",
                },
                stdin=panic_slave,
                stdout=panic_slave,
                stderr=panic_slave,
                preexec_fn=attach_panic_terminal,
            )
            os.close(panic_slave)
            panic_output = bytearray()
            panic_deadline = time.monotonic() + 8
            try:
                while time.monotonic() < panic_deadline:
                    readable, _, _ = select.select([panic_master], [], [], 0.05)
                    if readable:
                        try:
                            data = os.read(panic_master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            data = b""
                        panic_output.extend(data)
                        if b"\x1b[6n" in data:
                            os.write(panic_master, b"\x1b[2;1R")
                    if panic.poll() is not None:
                        break
                status = panic.wait(timeout=2)
                drain_terminal(panic_master, panic_output)
                assert status != 0, "panic probe unexpectedly exited successfully"
                assert b"ION smoke panic after first terminal draw" in panic_output, panic_output[-2000:]
                for sequence, label in (
                    (b"\x1b[?1049h", "alternate screen enter"),
                    (b"\x1b[?1049l", "alternate screen leave"),
                    (b"\x1b[?2004l", "bracketed paste disable"),
                    (b"\x1b[?1000l", "mouse capture disable"),
                    (b"\x1b[?25h", "cursor show"),
                    (b"\x1b[0m", "terminal style reset"),
                ):
                    assert sequence in panic_output, f"missing {label}: {panic_output[-2000:]!r}"
                attrs = termios.tcgetattr(panic_master)
                assert attrs[3] & termios.ECHO, attrs
                assert attrs[3] & termios.ICANON, attrs
            finally:
                if panic.poll() is None:
                    panic.send_signal(signal.SIGKILL)
                    panic.wait()
                os.close(panic_master)

            print("Ion inline/fullscreen terminal, grouped activity, panic restoration, modals and restoration: OK")
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
    finally:
        server.terminate()
        server.wait(timeout=5)

def exercise_active_band():
    with tempfile.TemporaryDirectory(prefix="ion-active-band-") as temporary:
        work = Path(temporary)
        env = os.environ.copy()
        env.update(XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color", ION_SMOKE_SALIENCE="1")
        port, trace = work / "port", work / "trace"
        server = subprocess.Popen([sys.executable, str(root / "scripts/smoke_provider.py"), str(port), str(trace)], env=env, stderr=subprocess.PIPE)
        child = None
        master = None
        try:
            end = time.monotonic() + 20
            while not port.exists():
                assert time.monotonic() < end, "active-band provider did not start"
                time.sleep(0.02)
            subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", f"http://127.0.0.1:{port.read_text()}/v1/chat/completions", "--wire", "chat-completions"], env=env, capture_output=True, check=True)
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

            def attach():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

            child = subprocess.Popen([binary, "--cwd", work, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
            os.close(slave)
            output = bytearray()
            sent = repainted = checked = quit_sent = False
            repaint_time = 0
            while time.monotonic() < end:
                readable, _, _ = select.select([master], [], [], 0.03)
                if readable:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:  # Linux PTYs return EIO when their slave closes.
                            raise
                        break
                    output.extend(data)
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[2;1R")
                if not sent and "› ".encode() in output:
                    os.write(master, b"Exercise the busy activity band.\r")
                    sent = True
                if not repainted and (work / "active.ready").exists():
                    assert len(list(work.glob("mutation-*.txt"))) == 14
                    # Discard earlier output; force the CURRENT inline region to
                    # repaint, without opening an inspector or completing work.
                    while select.select([master], [], [], 0)[0]:
                        os.read(master, 65536)
                    output.clear()
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 99, 0, 0))
                    os.kill(child.pid, signal.SIGWINCH)
                    repaint_time = time.monotonic()
                    repainted = True
                if repainted and not checked and time.monotonic() - repaint_time > 0.4:
                    for fact in [b"1 failed", b"1 running", b"2 queued", b"14 write", b"FAILURE_MARKER", b"Current activity", b"Running touch active.ready"]:
                        assert fact in output, ("active normal band hid known work", fact, output)
                    assert b"SALIENCE_DONE" not in output and b"\x1b[?1049h" not in output
                    checked = True
                if checked and b"SALIENCE_DONE" in output and not quit_sent:
                    os.write(master, b"\x03")
                    quit_sent = True
                if child.poll() is not None:
                    break
            status = child.wait(timeout=2)
            assert checked and status == 0, ("active-band child ended before qualification", status, output[-2000:])
            print("Ion active tree distinguishes running and queued calls and retains mutation/exception facts: OK")
        finally:
            if child is not None and child.poll() is None:
                child.kill()
                child.wait()
            if master is not None:
                os.close(master)
            server.terminate()
            server.wait(timeout=5)

exercise_active_band()
