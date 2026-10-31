"""Exercise a coding turn through Ion's actual terminal client and a PTY."""

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
import sys
import tempfile
import termios
import time
from pathlib import Path

from terminal_style import has_foreground


class CursorReplies:
    """Track positioned CPR reference points, not a rendered screen/reflow model."""
    def __init__(self):
        self.position = (2, 1)  # The fixture's initial shell cursor, one-based.
        self.saved = self.position
        self.pending = b""
        self.keyboard_replied = False
        self.device_replied = False

    def resize(self, rows, columns):
        row, column = self.position
        self.position = (min(row, rows), min(column, columns))

    def feed(self, data):
        data = self.pending + data
        replies = []
        if b"\x1b[?u" in data and not self.keyboard_replied:
            # Answer the keyboard capability query so startup exercises a
            # supporting terminal instead of waiting for the 500 ms fallback.
            replies.append(b"\x1b[?0u")
            self.keyboard_replied = True
        if b"\x1b[c" in data and not self.device_replied:
            replies.append(b"\x1b[?1;2c")
            self.device_replied = True
        end = 0
        for token in re.finditer(rb"\x1b\[([0-9;?]*)([@-~])", data):
            parameters, final = token.groups()
            if final == b"H":
                coordinates = parameters.split(b";")
                self.position = (int(coordinates[0] or b"1"), int(coordinates[1] or b"1") if len(coordinates) > 1 else 1)
            elif parameters == b"?1049":
                if final == b"h":
                    self.saved = self.position
                    self.position = (1, 1)
                elif final == b"l":
                    self.position = self.saved
            elif final == b"n" and parameters == b"6":
                row, column = self.position
                replies.append(f"\x1b[{row};{column}R".encode())
            end = token.end()
        tail = data[end:]
        escape = tail.rfind(b"\x1b")
        self.pending = tail[escape:] if escape >= 0 else b""
        return replies


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


def exercise_unicode_resume(workspace, env, target, mode, session_id):
    """Select a Unicode-named Session after its database filename changes."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 240, 0, 0))

    def attach():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    child = subprocess.Popen([binary, "--cwd", workspace, "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
    os.close(slave)
    output = bytearray()
    cursor = CursorReplies()

    def collect():
        if select.select([master], [], [], 0.05)[0]:
            try:
                data = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                return
            output.extend(data)
            for reply in cursor.feed(data):
                os.write(master, reply)

    def wait(predicate):
        end = time.monotonic() + 8
        while not predicate():
            assert child.poll() is None and time.monotonic() < end, (mode, child.poll(), output[-2500:])
            collect()

    try:
        wait(lambda: "›".encode() in output)
        output.clear()
        os.write(master, b"/resume\r")
        wait(lambda: b"Resume session" in output and b"01234567890" in output)
        os.write(master, b"01234567890")
        wait(lambda: b"1 match(es)" in output)
        output.clear()
        os.write(master, b"\r")
        wait(lambda: b"Ready" in output and "›".encode() in output)
        # The reopened browser marks the now-current Session and exposes the
        # existing recorded facts before a second selection.
        output.clear()
        os.write(master, b"/resume\r")
        wait(lambda: b"Resume session" in output)
        os.write(master, b"01234567890")
        wait(lambda: b"1 match(es)" in output
             and b"[current]" in output
             and session_id.encode() in output
             and b"2 turn(s)" in output
             and b"smoke/smoke-model" in output)
        output.clear()
        os.write(master, b"\r")
        # Selecting the same Session only reloads resources; the unchanged
        # Ready status is not repainted, so let the close settle before /session.
        settle = time.monotonic() + 0.5
        while time.monotonic() < settle:
            collect()
        output.clear()
        os.write(master, b"/session\r")
        wait(lambda: str(target).encode() in output)
        os.write(master, b"\x03")
        end = time.monotonic() + 5
        while child.poll() is None:
            assert time.monotonic() < end, (mode, "exit timed out", output[-2500:])
            collect()
        drain_terminal(master, output)
        assert child.returncode == 0, output[-2500:]
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=5)
        os.close(master)


def exercise_file_discovery():
    """Hold a real traversal read; input and settlement must not wait for it."""
    with tempfile.TemporaryDirectory(prefix="ion-file-discovery-") as temporary:
        work = Path(temporary)
        env = {name: value for name, value in os.environ.items() if name in ("PATH", "SHELL", "TMPDIR")}
        env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
        # Selection alone does not contact this endpoint; these are offline
        # composer/direct-shell checks, not provider qualification.
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", "http://127.0.0.1:1/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        for mode in ("inline", "fullscreen"):
            workspace = work / mode
            workspace.mkdir()
            (workspace / "data.txt").write_text("sample\n")
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

            def attach():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

            child = subprocess.Popen([binary, "--cwd", workspace, "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
            os.close(slave)
            output = bytearray()
            cursor = CursorReplies()
            writer = None

            def pump():
                if select.select([master], [], [], 0.03)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        data = b""
                    output.extend(data)
                    for reply in cursor.feed(data):
                        os.write(master, reply)

            def wait(predicate):
                end = time.monotonic() + 5
                while not predicate():
                    assert child.poll() is None and time.monotonic() < end, (child.poll(), output[-2500:])
                    pump()

            def hold_read():
                # A FIFO ignore file blocks inside the real WalkBuilder I/O.
                # Opening its writer confirms a reader, then withholding EOF
                # keeps the step held without production fault hooks.
                end = time.monotonic() + 5
                while True:
                    try:
                        return os.open(workspace / ".ignore", os.O_WRONLY | os.O_NONBLOCK)
                    except OSError as error:
                        if error.errno != errno.ENXIO:
                            raise
                        assert time.monotonic() < end and child.poll() is None, output[-2500:]
                        pump()

            try:
                wait(lambda: cursor.position[1] == 3 and "›".encode() in output)
                # Create it only after host startup resource discovery settles.
                os.mkfifo(workspace / ".ignore")
                for busy in (False, True):
                    output.clear()
                    if busy:
                        os.write(master, b"!sleep 30\r")
                        wait(lambda: b"Running shell" in output)
                    os.write(master, b"Read @")
                    writer = hold_read()
                    wait(lambda: "discovering paths…".encode() in output)
                    # Tab cannot accept an unseen file while the read is held.
                    # The final character acknowledges processing that Tab, not
                    # merely a query repaint before the attempted completion.
                    os.write(master, b"data.tx\tt")
                    wait(lambda: "› Read @data.txt".encode() in output)
                    output.clear()
                    os.close(writer)
                    writer = None
                    # Completion must paint without any additional input.
                    wait(lambda: "› data.txt".encode() in output)
                    # Acceptance adds only an already-blank cell plus a cursor
                    # move. A following real draft edit acknowledges it without
                    # demanding a redundant repaint from the diff renderer.
                    os.write(master, b"\rselected")
                    wait(lambda: b"Read @data.txt selected" in output)
                    if not busy:
                        output.clear()
                        os.write(master, b"\x03")  # Explicitly clear the draft.
                        wait(lambda: cursor.position[1] == 3 and "›".encode() in output)
                    else:
                        output.clear()
                        # Return from the inserted trailing space into the
                        # mention, starting a fresh automatic discovery episode.
                        os.write(master, b"\x17\x1b[D")  # Kill marker, enter mention.
                        writer = hold_read()
                        wait(lambda: "discovering paths…".encode() in output)
                        # Escape, then leave the token and visibly acknowledge
                        # the editor action without issuing any provider input.
                        os.write(master, b"\x1b\x1b[Cdraft")
                        wait(lambda: b"Read @data.txt draft" in output)
                        output.clear()
                        os.write(master, b"\x03")
                        # Shell cancellation/settlement cannot await discovery.
                        wait(lambda: b"Shell finished" in output)
                        output.clear()
                        os.close(writer)
                        writer = None
                        end = time.monotonic() + 0.2
                        while time.monotonic() < end:
                            pump()
                        assert "Files ·".encode() not in output, "retired discovery reopened suggestions"
                        output.clear()
                        os.write(master, b"\x03")
                        wait(lambda: cursor.position[1] == 3 and "›".encode() in output)
                os.write(master, b"\x03")
                end = time.monotonic() + 5
                while child.poll() is None:
                    assert time.monotonic() < end, ("file-discovery terminal did not settle", output[-2500:])
                    pump()
                drain_terminal(master, output)
                assert child.returncode == 0, output[-2500:]
            finally:
                if writer is not None:
                    os.close(writer)
                if child.poll() is None:
                    child.kill()
                os.close(master)
                child.wait(timeout=5)
    print("Ion owned file discovery: idle/busy wake, pending input, dismissal and shell cancellation: OK")


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
if sys.argv[1:] == ["--file-discovery-only"]:
    exercise_file_discovery()
    raise SystemExit(0)
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
        wrapper.write_text('#!/bin/sh\n/bin/cat > "$ION_SMOKE_CLIPBOARD"\n')
        wrapper.chmod(0o755)
    env = os.environ.copy()
    env.update(ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color", ION_SMOKE_STEERING="1", SSH_CONNECTION="ion-smoke", WAYLAND_DISPLAY="ion-smoke", ION_SMOKE_CLIPBOARD=str(copy_file), PATH=f"{copy_bin}:{env['PATH']}")
    port_file, trace = work / "port", work / "requests"
    server = subprocess.Popen([sys.executable, str(root / "scripts/smoke_provider.py"), str(port_file), str(trace)], env=env | {"ION_SMOKE_TERMINAL": "1"}, stderr=subprocess.PIPE)
    try:
        while not port_file.exists():
            assert time.monotonic() < deadline, "mock provider did not start"
            time.sleep(0.02)
        subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", f"http://127.0.0.1:{port_file.read_text()}/v1/chat/completions", "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        def attach_controlling_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen([binary, "--cwd", workspace, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_controlling_terminal)
        os.close(slave)
        output = bytearray()
        cursor = CursorReplies()
        file_query_sent = False
        sent_file_start = selected_file = sent_first = sent_steering = sent_second = resized = sent_tool = closed_tool = sent_compact = sent_clone = sent_controls = sent_login = sent_key = sent_logout = sent_copy = sent_quit = False
        saw_inline_start = False
        sent_help = sent_help_keys = sent_preparation = False
        help_start = preparation_start = 0
        preparation_requests = None
        opened_active = closed_active = False
        sent_selected = closed_selected = False
        active_start = tool_start = selected_start = 0
        control_phase = None
        control_start = 0
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
                    for reply in cursor.feed(data):
                        os.write(master, reply)
                if b"\xe2\x80\xba " in output and not sent_file_start:
                    assert b"\x1b[?1049h" not in output, "chat entered alternate screen before showing the inline composer"
                    saw_inline_start = True
                    # @ opens suggestions automatically. A Tab here can accept
                    # the first candidate before the query below is typed.
                    os.write(master, b"Read @")
                    sent_file_start = True
                if sent_file_start and "Files ·".encode() in output and not file_query_sent:
                    os.write(master, b"data.txt")
                    file_query_sent = True
                if file_query_sent and "› data.txt".encode() in output and not selected_file:
                    os.write(master, b"\r")
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
                    assert "Ctrl-C cancels · Working · Details closed".encode() in output[active_start:], "detail-close notice hid the active operation"
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
                    cursor.resize(7, 24)
                    os.kill(child.pid, signal.SIGWINCH)
                    time.sleep(0.1)
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
                    cursor.resize(30, 100)
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
                if sent_compact and b"Context summarized; raw history retained" in output and not sent_preparation:
                    preparation_start = len(output)
                    preparation_requests = trace.read_bytes()
                    os.write(master, b"/image missing.png\rstatus probe\r")
                    sent_preparation = True
                if sent_preparation and b"Prompt returned: preparation failed" in output[preparation_start:] and b"\x1b]7501;state=done:app=ion\x1b\\" in output[preparation_start:] and not sent_clone:
                    assert trace.read_bytes() == preparation_requests, "failed preparation reached the model"
                    # Clear the recovered literal; commands remain UI-only.
                    os.write(master, b"\x03")
                    snapshot = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, check=True, capture_output=True)
                    source_before_clone = json.loads(snapshot.stdout)["entries"]
                    os.write(master, b"/clone\r")
                    sent_clone = True
                if sent_clone and b"Cloned conversation as" in output and control_phase is None:
                    os.write(master, "/name Smoke repair 01234567890🦀\r".encode())
                    time.sleep(0.2)
                    os.write(master, b"/new\r")
                    time.sleep(0.2)
                    control_start = len(output)
                    os.write(master, b"/resume\r")
                    control_phase = "resume_open"
                # Confirmation is not queued while geometry/discovery is pending.
                # Wait for the actual loaded surface and then the painted query,
                # rather than combining typing and Enter behind a fixed sleep.
                current_control = output[control_start:]
                if control_phase == "resume_open" and b"Resume session" in current_control and b"match(es)" in current_control:
                    control_start = len(output)
                    os.write(master, b"Smoke repair")
                    control_phase = "resume_query"
                elif control_phase == "resume_query" and "› Smoke repair".encode() in current_control:
                    control_start = len(output)
                    os.write(master, b"\r")
                    control_phase = "resumed"
                elif control_phase == "resumed" and b"Ready" in current_control:
                    control_start = len(output)
                    os.write(master, b"/model\r")
                    control_phase = "model_open"
                elif control_phase == "model_open" and b"Choose model" in current_control and b"match(es)" in current_control:
                    control_start = len(output)
                    os.write(master, b"\r")  # The current model is visibly preselected.
                    control_phase = "model_selected"
                elif control_phase == "model_selected" and b"Ready" in current_control:
                    sent_controls = True
                    control_phase = "done"
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
                if sent_help and b"/help COMMAND" in output[help_start:] and b"Models & credentials" in output[help_start:] and not sent_help_keys:
                    help_start = len(output)
                    os.write(master, b"/help keys\r")
                    sent_help_keys = True
                if sent_help_keys and b"app.clipboard.pasteImage" in output[help_start:] and b"app.details.nextPage" in output[help_start:] and not sent_copy:
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
            for state in (b"idle", b"working", b"done"):
                assert b"\x1b]7501;state=" + state + b":app=ion\x1b\\" in output, state
            assert b"\x1b]7501;state=blocked:kind=auth:app=ion\x1b\\" in output, "credential prompt did not publish auth block"
            plain_output = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output))
            assert b"MARKDOWN_BOLD and literal_code docs (https://example.org/ion)" in plain_output, "assistant Markdown was not rendered"
            assert b"TABLE_CELL \xe2\x94\x82 TABLE_VISIBLE" in plain_output, "assistant table was not laid out"
            assert b"fn syntax_probe()" in plain_output and b'"SYNTAX_LITERAL"; // syntax comment' in plain_output, "syntax rendering changed code text"
            # No palette queries are answered: the fallback selects ANSI slot 2.
            assert has_foreground(bytes(output), b'"SYNTAX_LITERAL"', ("ansi", 2)), "syntax string color did not reach terminal publication"
            assert b"-sample data" in plain_output and b"+sample data updated" in plain_output, "actual edit patch was not rendered"
            assert b"+created by ion" in plain_output, "actual write patch was not rendered"
            assert "• ".encode() in output, "semantic activity group header was not rendered"
            assert b"\xe2\x94\x9c " in output or b"\xe2\x94\x94 " in output, "grouped tool tree was not rendered"
            assert sent_file_start and selected_file and sent_first and sent_steering and sent_second and resized and sent_tool and closed_tool and sent_compact and sent_preparation and sent_clone and sent_controls and sent_key and sent_logout and sent_copy and sent_quit, "terminal did not complete the session/model/login workflow"
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
            code_source = '```rust\nfn syntax_probe() {\n    let message = "SYNTAX_LITERAL"; // syntax comment\n}\n```'
            assert any(code_source in part.get("Text", "") for entry in entries if entry["kind"] == "assistant" for part in entry["data"]["message"]["content"]), "syntax rendering changed committed code"
            diffs = [entry["data"]["result"]["outcome"]["output"]["value"]["diff"] for entry in entries if entry["kind"] == "tool_result" and entry["data"]["result"]["outcome"]["state"] == "observed" and "diff" in entry["data"]["result"]["outcome"]["output"]["value"]]
            assert len(diffs) == 2 and all(not diff["truncated"] for diff in diffs), diffs
            assert "-sample data\n-second token\n+sample data updated\n+second token updated\n" in diffs[0]["text"], diffs
            assert "+created by ion\n" in diffs[1]["text"], diffs
            exported = subprocess.run([binary, "--cwd", workspace, "--session", session_id, "export"], env=env, check=True, capture_output=True, text=True).stdout
            assert "**MARKDOWN_BOLD**" in exported, "export lost the original Markdown source"
            assert "| TABLE_CELL | TABLE_VISIBLE |" in exported, "export lost the original table source"
            assert "\n".join(f"  {line}" for line in code_source.splitlines()) in exported, "export lost code source"
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
            assert not (work / "ion" / "credentials" / "smoke.key").exists()

            # The terminal has exited and all inspect/export processes have
            # settled: rename the owned completed Session, never copy a live
            # SQLite main file away from its WAL.
            def logical_id(path):
                with sqlite3.connect(path) as connection:
                    return json.loads(connection.execute("SELECT header FROM session WHERE id=1").fetchone()[0])["id"]
            saved = next(path for path in (work / "ion").rglob("*.sqlite") if logical_id(path) == session_id)
            renamed = saved.with_name("01234567890🦀.sqlite")
            saved.rename(renamed)
            unicode_listing = subprocess.run([binary, "--cwd", workspace, "sessions"], env=env, check=True, capture_output=True, text=True).stdout
            assert f"{session_id}\t" in unicode_listing, unicode_listing
            assert all(line.split("\t", 1)[0] != "01234567890🦀" for line in unicode_listing.splitlines()), "filename became execution identity"
            reopened = subprocess.run([binary, "--cwd", workspace, "--session", renamed, "inspect"], env=env, check=True, capture_output=True)
            assert json.loads(reopened.stdout) == json.loads(inspected.stdout), "filename rename changed Session facts"
            for mode in ("inline", "fullscreen"):
                exercise_unicode_resume(workspace, env, renamed, mode, session_id)
            after_resume = subprocess.run([binary, "--cwd", workspace, "--session", renamed, "inspect"], env=env, check=True, capture_output=True)
            assert json.loads(after_resume.stdout) == json.loads(inspected.stdout), "picker resume changed passive Session facts"

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
            fullscreen_cursor = CursorReplies()
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
                        for reply in fullscreen_cursor.feed(data):
                            os.write(fullscreen_master, reply)
                    if (
                        b"\x1b[?1049h" in fullscreen_output
                        and b"\xe2\x80\xba" in fullscreen_output
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
                [binary, "--cwd", workspace, "--tui-mode", "fullscreen", "chat", "NEVER_START_AFTER_DRAW_FAULT"],
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
            panic_cursor = CursorReplies()
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
                        for reply in panic_cursor.feed(data):
                            os.write(panic_master, reply)
                    if panic.poll() is not None:
                        break
                status = panic.wait(timeout=2)
                drain_terminal(panic_master, panic_output)
                assert status != 0, "panic probe unexpectedly exited successfully"
                assert b"ION smoke panic after first terminal draw" in panic_output, panic_output[-2000:]
                assert "NEVER_START_AFTER_DRAW_FAULT" not in trace.read_text(), "failed startup dispatched its initial prompt"
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
        env.update(ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color", ION_SMOKE_SALIENCE="1")
        port, trace = work / "port", work / "trace"
        server = subprocess.Popen([sys.executable, str(root / "scripts/smoke_provider.py"), str(port), str(trace)], env=env | {"ION_SMOKE_TERMINAL": "1"}, stderr=subprocess.PIPE)
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
            cursor = CursorReplies()
            sent = repainted = checked = quit_sent = False
            repaint_time = quit_time = 0
            completed = 0
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
                    for reply in cursor.feed(data):
                        os.write(master, reply)
                if not sent and "› ".encode() in output:
                    os.write(master, b"Exercise the busy activity band.\r")
                    sent = True
                if not repainted and (work / "active.ready").exists():
                    assert len(list(work.glob("mutation-*.txt"))) == 14
                    # Discard earlier output; force the CURRENT inline region to
                    # repaint, without opening an inspector or completing work.
                    while select.select([master], [], [], 0)[0]:
                        for reply in cursor.feed(os.read(master, 65536)):
                            os.write(master, reply)
                    output.clear()
                    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
                    os.kill(child.pid, signal.SIGWINCH)
                    repaint_time = time.monotonic()
                    repainted = True
                if repainted and not checked and time.monotonic() - repaint_time > 0.4:
                    for fact in [b"1 failed", b"1 running", b"2 queued", b"14 write", b"FAILURE_MARKER", b"Current activity", b"Running touch active.ready"]:
                        assert fact in output, ("active normal band hid known work", fact, output)
                    assert b"SALIENCE_DONE" not in output and b"\x1b[?1049h" not in output
                    checked = True
                if checked and b"SALIENCE_DONE" in output and not quit_sent:
                    # Provisional text can arrive before Core completion. Quit
                    # only after the owned Turn has actually committed success.
                    sessions = list((work / "ion/sessions").rglob("*.sqlite"))
                    assert len(sessions) == 1, sessions
                    with sqlite3.connect(f"file:{sessions[0]}?mode=ro", uri=True) as db:
                        completed = db.execute("SELECT count(*) FROM entries WHERE json_extract(body,'$.kind')='turn_ended' AND json_extract(body,'$.data.reason')='completed'").fetchone()[0]
                    if completed == 1:
                        os.write(master, b"\x03")
                        quit_sent = True
                        quit_time = time.monotonic()
                elif quit_sent and time.monotonic() - quit_time >= 0.1:
                    # The UI can harvest the joined result after its first key.
                    # Keep draining and finish the bounded idle-quit request.
                    os.write(master, b"\x03")
                    quit_time = time.monotonic()
                if child.poll() is not None:
                    break
            try:
                status = child.wait(timeout=2)
            except subprocess.TimeoutExpired as error:
                raise AssertionError(("active-band child did not exit", checked, quit_sent, completed, output[-2000:])) from error
            assert checked and quit_sent and completed == 1 and status == 0, ("active-band child ended before qualification", status, output[-2000:])
            print("Ion active tree distinguishes running and queued calls and retains mutation/exception facts: OK")
        finally:
            if child is not None and child.poll() is None:
                child.kill()
                child.wait()
            if master is not None:
                os.close(master)
            server.terminate()
            server.wait(timeout=5)

exercise_file_discovery()
exercise_active_band()


def exercise_questions():
    """Real client single/multi questions: choices, empty, custom, cancel and stop."""
    with tempfile.TemporaryDirectory(prefix="ion-questions-") as temporary:
        work = Path(temporary)
        env = {name: value for name, value in os.environ.items() if name in ("PATH", "SHELL", "TMPDIR")}
        env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), TERM="xterm-256color")
        for mode in ("select", "custom", "cancel", "stop", "multi", "multi-empty", "multi-custom", "multi-cancel", "multi-stop"):
            workspace = work / mode
            workspace.mkdir()
            port_file, trace = work / f"port-{mode}", work / f"requests-{mode}"
            server_env = env | {"ION_SMOKE_QUESTIONS": mode}
            server = subprocess.Popen([sys.executable, str(root / "scripts/smoke_provider.py"), str(port_file), str(trace)], env=server_env, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + 20
                while not port_file.exists():
                    assert time.monotonic() < deadline and server.poll() is None, (mode, "mock provider did not start")
                    time.sleep(0.02)
                subprocess.run([binary, "use", "smoke", "smoke-model", "--endpoint", f"http://127.0.0.1:{port_file.read_text()}/v1/chat/completions", "--wire", "chat-completions"], env=env, check=True, capture_output=True)
                master, slave = pty.openpty()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

                def attach():
                    os.setsid()
                    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

                child = subprocess.Popen([binary, "--cwd", workspace, "chat", "Ask the fixture question"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
                os.close(slave)
                output = bytearray()
                cursor = CursorReplies()

                def pump():
                    if select.select([master], [], [], 0.03)[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            data = b""
                        output.extend(data)
                        for reply in cursor.feed(data):
                            os.write(master, reply)

                def wait(predicate, label):
                    end = time.monotonic() + 8
                    while not predicate():
                        assert child.poll() is None and time.monotonic() < end, (mode, label, child.poll(), bytes(output[-2500:]))
                        pump()

                def plain():
                    return re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", bytes(output))

                try:
                    wait(lambda: b"Question 1/1" in plain() and b"Type something." in plain(), "question modal")
                    assert b"\x1b]7501;state=blocked:kind=question:app=ion\x1b\\" in output, "question modal did not publish blocked status"
                    if mode == "select":
                        wait(lambda: b"ALPHA_PREVIEW" in plain(), "initial code preview")
                        assert b"alpha-line-39" not in plain(), "long preview was not windowed"
                        os.write(master, b"\x1b[6~\x1b[6~")
                        wait(lambda: b"alpha-line-39" in plain(), "paged preview tail")
                        assert not (workspace / "question-effect.txt").exists()
                        assert len(trace.read_text().splitlines()) == 1, "preview paging issued a model request"
                        os.write(master, b"\x1b[B")
                        wait(lambda: b"BETA_PREVIEW" in plain(), "focused alternative preview")
                        os.write(master, b"\r")
                        wait(lambda: b"QUESTION_DONE" in plain(), "selected answer effect")
                    elif mode == "multi":
                        os.write(master, b"\x1b[B ")  # Beta first.
                        wait(lambda: b"[x] Beta" in plain(), "multi checkbox")
                        assert not (workspace / "question-effect.txt").exists()
                        os.write(master, b"\x1b[A\r\r ")  # Toggle Alpha on/off/on.
                        os.write(master, b"\x1b[B\x1b[B\x1b[B\r")  # Explicit Next.
                        wait(lambda: b"QUESTION_DONE" in plain(), "multiple answer effect")
                    elif mode == "multi-empty":
                        os.write(master, b"\x1b[B\x1b[B\x1b[B\r")
                        wait(lambda: b"QUESTION_DONE" in plain(), "explicit empty answer effect")
                    elif mode in ("custom", "multi-custom"):
                        if mode == "multi-custom":
                            os.write(master, b" ")
                            wait(lambda: b"[x] Alpha" in plain(), "checked before custom")
                        os.write(master, b"\x1b[B\x1b[B\r")
                        wait(lambda: b"Enter answer" in plain(), "custom editor")
                        os.write(master, "café 🦀\r".encode())
                        wait(lambda: b"QUESTION_DONE" in plain(), "custom answer effect")
                    elif mode in ("cancel", "multi-cancel"):
                        if mode == "multi-cancel":
                            os.write(master, b" ")
                            wait(lambda: b"[x] Alpha" in plain(), "checked before cancel")
                        os.write(master, b"\x1b")
                        wait(lambda: b"CANCEL_OBSERVED" in plain(), "cancelled batch continuation")
                    else:
                        if mode == "multi-stop":
                            os.write(master, b" ")
                            wait(lambda: b"[x] Alpha" in plain(), "checked before stop")
                        os.write(master, b"\x03")
                        wait(lambda: b"Question closed" in plain(), "Ctrl-C stopped question Turn")
                    if mode not in ("stop", "multi-stop"):
                        wait(lambda: b"state=done:app=ion" in output or b"state=idle:app=ion" in output, "settled program status")
                        os.write(master, b"\x04")
                    else:
                        time.sleep(0.3)
                        os.write(master, b"\x04")
                    end = time.monotonic() + 6
                    while child.poll() is None:
                        assert time.monotonic() < end, (mode, "terminal did not exit", bytes(output[-2500:]))
                        pump()
                    drain_terminal(master, output)
                    assert child.returncode == 0, (mode, child.returncode, bytes(output[-2500:]))
                finally:
                    if child.poll() is None:
                        child.kill()
                    child.wait(timeout=5)
                    os.close(master)

                records = [json.loads(line) for line in trace.read_text().splitlines() if line.strip()]
                answered = mode in ("select", "custom", "multi", "multi-empty", "multi-custom")
                if answered:
                    assert len(records) == 3, (mode, len(records))
                    expected = {"select": "SELECTED:Beta", "multi": "MULTI:Alpha,Beta", "multi-empty": "MULTI:none"}.get(mode, "CUSTOM:café 🦀")
                    assert (workspace / "question-effect.txt").read_text() == expected
                elif mode in ("cancel", "multi-cancel"):
                    assert len(records) == 2, (mode, len(records))
                    assert not (workspace / "question-effect.txt").exists()
                else:
                    assert len(records) == 1, (mode, len(records))
                    assert not (workspace / "question-effect.txt").exists()
                inspected = json.loads(subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, check=True, capture_output=True).stdout)
                outcomes = []
                for entry in inspected["entries"]:
                    if entry["kind"] != "tool_result":
                        continue
                    result = entry["data"]["result"]
                    if result.get("name") != "ask_user_question":
                        continue
                    outcome = result["outcome"]
                    assert outcome["state"] == "observed", outcome
                    outcomes.append(outcome["output"]["value"])
                assert outcomes, (mode, inspected)
                if answered:
                    assert outcomes[-1]["status"] == "answered", outcomes[-1]
                else:
                    assert outcomes[-1]["status"] == "cancelled", outcomes[-1]
                status_sequences = re.findall(rb"\x1b\]7501;state=([^:\x1b]+)", bytes(output))
                assert status_sequences and status_sequences[-1] != b"blocked", (mode, status_sequences[-1:])
                assert b"Which route" not in b"".join(re.findall(rb"\x1b\]7501;.*?\x1b\\\\", bytes(output)))
            finally:
                server.terminate()
                server.wait(timeout=5)
                diagnostics = server.stderr.read()
                if diagnostics:
                    print(f"question provider {mode} stderr:\n{diagnostics.decode(errors='replace')}", file=sys.stderr)
    print("Ion typed questions: single/multi, explicit empty, custom Unicode, cancel, Ctrl-C stop, status and independent effects: OK")


exercise_questions()
