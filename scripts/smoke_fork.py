"""Exercise selected-point Session forks through the built CLI and terminal."""

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
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))


requests = []


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        events = [
            {"id": "fork", "choices": [{"index": 0, "delta": {"content": "FORK_OK"}, "finish_reason": None}]},
            {"id": "fork", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-fork-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "fork-model", "--endpoint", endpoint, "--wire", "openrouter-chat"], env=env, check=True, capture_output=True)

        def ion(*args, check=True):
            return subprocess.run([binary, "--cwd", workspace, *args], env=env, capture_output=True, text=True, check=check)

        first = ion("run", "first request")
        assert first.stdout.strip() == "FORK_OK"
        source_id = first.stderr.split("[session: ")[1].split("]")[0]
        ion("--session", source_id, "run", "second request")
        source_provider_id = requests[0]["session_id"]
        assert requests[1]["session_id"] == source_provider_id
        assert len(source_provider_id) == 36
        listing = ion("--session", source_id, "turns").stdout
        assert "1\tended\tfirst request" in listing and "2\tended\tsecond request" in listing
        fork_id = ion("--session", source_id, "fork", "2").stdout.split()[-1]
        fork_view = json.loads(ion("--session", fork_id, "inspect").stdout)
        starts = [entry["data"]["turn"] for entry in fork_view["entries"] if entry["kind"] == "turn_started"]
        assert starts == [1], starts
        assert fork_view["last_model"] == {"provider": "smoke", "model": "fork-model"}
        ion("--session", fork_id, "run", "alternative second request")
        fork_provider_id = requests[-1]["session_id"]
        assert fork_provider_id != source_provider_id
        source_view = json.loads(ion("--session", source_id, "inspect").stdout)
        assert [entry["data"]["turn"] for entry in source_view["entries"] if entry["kind"] == "turn_started"] == [1, 2]
        after_id = ion("--session", source_id, "fork", "1", "--after").stdout.split()[-1]
        after_view = json.loads(ion("--session", after_id, "inspect").stdout)
        assert [entry["data"]["turn"] for entry in after_view["entries"] if entry["kind"] == "turn_started"] == [1]
        ion("--session", after_id, "run", "continue after first request")
        after_provider_id = requests[-1]["session_id"]
        clone_id = ion("--session", source_id, "clone").stdout.split()[-1]
        ion("--session", clone_id, "run", "continue cloned conversation")
        clone_provider_id = requests[-1]["session_id"]
        assert len({source_provider_id, fork_provider_id, after_provider_id, clone_provider_id}) == 4
        before_compact = len(requests)
        ion("--session", source_id, "compact")
        assert len(requests) > before_compact, "compaction did not call the provider"
        assert all(request["session_id"] == source_provider_id for request in requests[before_compact:])
        assert all(request["session_id"] not in json.dumps(request["messages"]) for request in requests)
        missing = ion("--session", source_id, "fork", "99", check=False)
        assert missing.returncode != 0 and "boundary" in missing.stderr

        rpc = subprocess.Popen([binary, "--cwd", workspace, "--session", source_id, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            assert json.loads(rpc.stdout.readline())["type"] == "ready"
            rpc.stdin.write(b'{"id":"turns","type":"list_turns"}\n')
            rpc.stdin.flush()
            listed = json.loads(rpc.stdout.readline())
            assert [turn["turn"] for turn in listed["data"]] == [1, 2]
            rpc.stdin.write(b'{"id":"fork","type":"fork","turn":1,"after":true}\n')
            rpc.stdin.flush()
            forked = json.loads(rpc.stdout.readline())
            assert forked["success"] and forked["data"]["session"] != source_id
            rpc.stdin.close()
            assert rpc.wait(timeout=8) == 0
        finally:
            if rpc.poll() is None:
                rpc.kill()
                rpc.wait()

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def attach_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen([binary, "--cwd", workspace, "--session", source_id, "chat"], env={**env, "TERM": "xterm-256color"}, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
        os.close(slave)
        output = bytearray()
        sent = quit_sent = False
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
                if b"\xe2\x80\xba " in output and not sent:
                    assert b"\x1b[?1049h" not in output, "fork smoke entered alternate screen at inline startup"
                    os.write(master, b"/fork 2\r")
                    sent = True
                if sent and b"Forked as" in output and not quit_sent:
                    os.write(master, b"\x03\x03")
                    quit_sent = True
                if child.poll() is not None:
                    break
            child.wait(timeout=5)
            assert child.returncode == 0 and b"Forked as" in output, output[-1200:]
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
        print("Ion fork/clone, provider affinity across reopen/compaction and terminal control: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
