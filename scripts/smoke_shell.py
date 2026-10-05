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
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
def output_failure_settles_operation(workspace, env, mode, coding):
    """Keep input alive while failing only the actual terminal output device."""
    workspace = workspace / ("output-failure-turn" if coding else "output-failure-shell")
    workspace.mkdir()
    session = workspace / "session.sqlite"
    command = "printf '%s\\n' $$ > render.pid; printf 'CAPTURED_BEFORE_RENDER_FAILURE\\n'; touch render.ready; exec sleep 30"
    server = None
    thread = None
    requests = 0
    if coding:
        class Provider(BaseHTTPRequestHandler):
            def do_POST(self):
                nonlocal requests
                requests += 1
                self.rfile.read(int(self.headers["Content-Length"]))
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                delta = {"tool_calls": [{"index": 0, "id": "render-call", "type": "function", "function": {"name": "exec", "arguments": json.dumps({"command": command, "timeout_ms": 120000})}}]}
                body = {"id": "render", "choices": [{"index": 0, "delta": delta, "finish_reason": "tool_calls"}]}
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
        os.close(output_master)
        output_master = None
        # Force a changed live frame; an unchanged idle frame need not write.
        os.write(input_master, b"draft after output failure")
        assert child.wait(timeout=8) != 0, "output failure was reported as success"
        view = json.loads(subprocess.run([binary, "--cwd", workspace, "--session", session, "inspect"], env=env, capture_output=True, check=True).stdout)
        if coding:
            results = [entry["data"]["result"] for entry in view["entries"] if entry["kind"] == "tool_result"]
            assert len(results) == 1, ("coding tool outcome was lost on rendering failure", [entry["kind"] for entry in view["entries"]])
            result = results[0]["result"]
            assert view["unfinished_turn"] is None and view["entries"][-1]["data"]["reason"] == "cancelled", view["unfinished_turn"]
        else:
            shells = [entry["data"] for entry in view["entries"] if entry["kind"] == "user_shell"]
            assert len(shells) == 1, ("shell outcome was lost on rendering failure", [entry["kind"] for entry in view["entries"]])
            result = shells[-1]["output"]
        assert result["stdout"] == "CAPTURED_BEFORE_RENDER_FAILURE\n" and result["cancelled"], result
        assert result["signal"] is not None and result["wait_error"] is None, result
        assert requests == (1 if coding else 0), "output failure dispatched another model request"
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            pid = None
        else:
            raise AssertionError("direct shell command still exists after terminal exit")
        print(f"Ion {mode} terminal output failure preserves {'coding' if coding else 'shell'} settlement: OK")
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        if pid is not None:
            try:
                os.kill(pid, 9)
            except ProcessLookupError:
                pass
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
            for coding in (False, True):
                output_failure_settles_operation(workspace, env, mode, coding)
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
