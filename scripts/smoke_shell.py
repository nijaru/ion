"""Exercise visible shell outcomes, context choice and inspection in both TUIs."""

import fcntl
import json
import os
import pty
import select
import signal
import sqlite3
import struct
import subprocess
import tempfile
import termios
import time
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))


def observed_shells(view):
    admissions = {index: entry["data"] for index, entry in enumerate(view["entries"], 1) if entry["kind"] == "user_shell_admitted"}
    settlements = [entry["data"] for entry in view["entries"] if entry["kind"] == "user_shell_settled"]
    assert len(admissions) == len(settlements), view
    assert all(entry["outcome"]["kind"] == "observed" for entry in settlements), settlements
    return [(admissions[entry["admission_entry"]], entry["outcome"]) for entry in settlements]


def terminal_failure_settles_operation(workspace, env, mode, coding, fault):
    """A queued prompt cannot start after a terminal device disconnects."""
    workspace = workspace / f"{fault}-failure-{'turn' if coding else 'shell'}"
    workspace.mkdir()
    session = workspace / "session.sqlite"
    command = "printf '%s\\n' $$ > render.pid; printf 'CAPTURED_BEFORE_RENDER_FAILURE\\n'; touch render.ready; exec sleep 30"
    server = None
    thread = None
    requests = 0
    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            nonlocal requests
            requests += 1
            self.rfile.read(int(self.headers["Content-Length"]))
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            if coding and requests == 1:
                delta = {"tool_calls": [{"index": 0, "id": "render-call", "type": "function", "function": {"name": "exec", "arguments": json.dumps({"command": command, "timeout_ms": 120000})}}]}
                finish = "tool_calls"
            else:
                delta = {"content": "Done without tools."}
                finish = "stop"
            body = {"id": "render", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            self.wfile.write(b"data: " + json.dumps(body).encode() + b"\n\ndata: [DONE]\n\n")
            self.wfile.flush()

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    subprocess.run([binary, "use", "smoke", "shell-model", "--endpoint", f"http://127.0.0.1:{server.server_port}/v1/chat/completions", "--wire", "chat-completions"], env=env, capture_output=True, check=True)
    input_master, input_slave = pty.openpty()
    output_master, output_slave = pty.openpty()
    for slave in (input_slave, output_slave):
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

    def attach_input_terminal():
        os.setsid()
        fcntl.ioctl(input_slave, termios.TIOCSCTTY, 0)
        if fault == "input":
            # Exercise Ion's input lifecycle rather than a kernel SIGHUP exit.
            signal.signal(signal.SIGHUP, signal.SIG_IGN)

    child = subprocess.Popen([binary, "--cwd", workspace, "--session", session, "--tui-mode", mode, "chat"], env=env, stdin=input_slave, stdout=output_slave, stderr=subprocess.PIPE, preexec_fn=attach_input_terminal)
    os.close(input_slave)
    os.close(output_slave)
    pid = None
    try:
        output = bytearray()
        deadline = time.monotonic() + 8
        while "› ".encode() not in output:
            assert time.monotonic() < deadline, "terminal did not show its composer"
            if select.select([output_master], [], [], 0.05)[0]:
                data = os.read(output_master, 65536)
                output.extend(data)
                if b"\x1b[6n" in data:
                    os.write(input_master, b"\x1b[2;1R")
        os.write(input_master, ("Run the verification command.\r" if coding else f"!{command}\r").encode())
        while not (workspace / "render.ready").exists():
            assert time.monotonic() < deadline, "shell did not become ready"
            if select.select([output_master], [], [], 0.05)[0]:
                os.read(output_master, 65536)
        pid = int((workspace / "render.pid").read_text())
        os.write(input_master, b"Do not run another command." + (b"\x1b[13;3u" if coding else b"\r"))
        queued = bytearray()
        while b"1 follow-up(s) queued" not in queued:
            assert time.monotonic() < deadline, "follow-up was not admitted to the client queue"
            if select.select([output_master], [], [], 0.05)[0]:
                queued.extend(os.read(output_master, 65536))
        saved_entry = None
        if fault == "output-corrupt-view":
            # The live writer retains its state, but passive history inspection
            # now fails. That error must not mask the terminal's fatal fault.
            with sqlite3.connect(session) as connection:
                saved_entry = connection.execute("SELECT seq, body FROM entries ORDER BY seq LIMIT 1").fetchone()
                connection.execute("UPDATE entries SET body = ? WHERE seq = ?", (b"invalid entry", saved_entry[0]))
        if fault.startswith("output"):
            os.close(output_master)
            output_master = None
            # Force a changed live frame; an unchanged idle frame need not write.
            os.write(input_master, b"draft after output failure")
        else:
            os.write(input_master, b"\x0f")
            modal = bytearray()
            while b"Conversation" not in modal or b"\x1b[?25l" not in modal:
                assert time.monotonic() < deadline, "details did not hide the terminal cursor"
                if select.select([output_master], [], [], 0.05)[0]:
                    modal.extend(os.read(output_master, 65536))
            os.close(input_master)
            input_master = None
        teardown = bytearray()
        deadline = time.monotonic() + 8
        while child.poll() is None:
            assert time.monotonic() < deadline, "terminal failure did not settle and exit"
            if output_master is not None and select.select([output_master], [], [], 0.05)[0]:
                try:
                    teardown.extend(os.read(output_master, 65536))
                except OSError:
                    pass
            else:
                time.sleep(0.01)
        assert child.returncode != 0, "terminal failure was reported as success"
        if output_master is not None:
            while select.select([output_master], [], [], 0)[0]:
                try:
                    chunk = os.read(output_master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                teardown.extend(chunk)
        if saved_entry is not None:
            with sqlite3.connect(session) as connection:
                connection.execute("UPDATE entries SET body = ? WHERE seq = ?", (saved_entry[1], saved_entry[0]))
        view = json.loads(subprocess.run([binary, "--cwd", workspace, "--session", session, "inspect"], env=env, capture_output=True, check=True).stdout)
        if coding:
            results = [entry["data"]["result"] for entry in view["entries"] if entry["kind"] == "tool_result"]
            assert len(results) == 1, ("coding tool outcome was lost on rendering failure", [entry["kind"] for entry in view["entries"]])
            assert results[0]["outcome"]["state"] == "observed", results[0]
            result = results[0]["outcome"]["output"]["value"]
            assert view["unfinished_turn"] is None and view["entries"][-1]["data"]["reason"] == "cancelled", view["unfinished_turn"]
        else:
            shells = observed_shells(view)
            assert len(shells) == 1, ("shell outcome was lost on rendering failure", [entry["kind"] for entry in view["entries"]])
            result = shells[-1][1]["output"]
        assert result["stdout"] == "CAPTURED_BEFORE_RENDER_FAILURE\n" and result["cancelled"], result
        assert result["signal"] is not None and result["wait_error"] is None, result
        turns = [entry for entry in view["entries"] if entry["kind"] == "turn_started"]
        assert len(turns) == (1 if coding else 0), "terminal failure admitted the queued prompt"
        assert requests == (1 if coding else 0), "terminal failure dispatched another model request"
        if output_master is not None:
            left_alt = teardown.rfind(b"\x1b[?1049l")
            assert left_alt >= 0, "error teardown left the details surface active"
            restored = teardown[left_alt + len(b"\x1b[?1049l"):]
            assert b"\x1b[?25h" in restored, "error teardown left the terminal cursor hidden"
            assert b"\x1b[0m" in restored, "error teardown left terminal styling active"
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            pid = None
        else:
            raise AssertionError("direct shell command still exists after terminal exit")
        print(f"Ion {mode} terminal {fault} failure settles {'coding' if coding else 'shell'} without queued admission: OK")
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        if pid is not None:
            try:
                os.kill(pid, 9)
            except ProcessLookupError:
                pass
        if input_master is not None:
            os.close(input_master)
        if output_master is not None:
            os.close(output_master)
        if server is not None:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


for mode in ("inline", "fullscreen"):
    with tempfile.TemporaryDirectory(prefix=f"ion-shell-{mode}-") as temporary:
        work = Path(temporary)
        workspace = work / "workspace"
        workspace.mkdir()
        editor = work / "editor.sh"
        editor.write_text(f"#!/bin/sh\nif test -f '{workspace / 'editor.fail'}'; then exit 7; fi\nprintf edited-from-editor > \"$1\"\n")
        editor.chmod(0o700)
        env = {
            **os.environ,
            "XDG_CONFIG_HOME": str(work / "config"),
            "XDG_STATE_HOME": str(work / "state"),
            "TERM": "xterm-256color",
            "VISUAL": str(editor),
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
                    (workspace / "editor.fail").touch()
                    os.write(master, b"\x07")
                    step = "editor_failure"
                if step == "editor_failure" and b"Editor failed" in segment and b"original draft retained" in segment and b"edited-from-editor" in segment:
                    (workspace / "editor.fail").unlink()
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
            entries = observed_shells(json.loads(inspect))
            assert len(entries) == 3, entries
            assert entries[0][1]["output"]["stdout"] == "VISIBLE_OUTPUT\n" and not entries[0][0]["exclude_from_context"]
            assert entries[1][1]["output"]["stderr"] == "FAILED_STDERR\n" and entries[1][1]["output"]["exit_code"] == 7 and entries[1][1]["is_error"]
            assert entries[2][1]["output"]["stdout"] == "PRIVATE_OUTPUT\n" and entries[2][0]["exclude_from_context"]
            transcript = (workspace / "session.txt").read_text()
            assert "VISIBLE_OUTPUT" in transcript and "PRIVATE_OUTPUT" in transcript and "not shared with model" in transcript
            assert (workspace / "session.txt").stat().st_mode & 0o777 == 0o600
            resumed = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, capture_output=True, text=True, check=True)
            assert len(observed_shells(json.loads(resumed.stdout))) == 3
            exported = subprocess.run([binary, "--cwd", workspace, "--continue", "export"], env=env, capture_output=True, text=True, check=True)
            assert "User shell" in exported.stdout and "PRIVATE_OUTPUT" in exported.stdout
            refused = subprocess.run([binary, "--cwd", workspace, "--continue", "export", "session.txt"], env=env, capture_output=True, text=True)
            assert refused.returncode != 0 and "cannot save transcript" in refused.stderr
            print(f"Ion {mode} shell output, context choice and inspection: OK")
            for fault in ("output", "input"):
                for coding in (False, True):
                    terminal_failure_settles_operation(workspace, env, mode, coding, fault)
            terminal_failure_settles_operation(workspace, env, mode, False, "output-corrupt-view")
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
