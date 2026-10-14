"""Qualify the built SDK example's effects, reopen, stop-and-join and output custody."""

import json
import os
import shlex
import signal
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
cli = Path(os.environ.get("ION_SMOKE_BIN", repo / "target/debug/ion"))
example = Path(os.environ.get("ION_SMOKE_EMBEDDED_BIN", repo / "target/debug/examples/embedded"))
requests = []


class Peer(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert "Authorization" not in self.headers
        assert body["model"] == "sdk-fixture"
        requests.append(body)
        messages = body["messages"]
        task = [message["content"] for message in messages if message["role"] == "user"][-1]
        if task == "CREATE_α🦀" and messages[-1]["role"] != "tool":
            name, arguments, call_id = "write", {"path": "effect.txt", "content": "preserved α🦀\n"}, "create-file"
        elif task == "CANCEL":
            code = "from pathlib import Path; import time; Path('started.txt').write_text('started once'); time.sleep(30)"
            name, arguments, call_id = "exec", {"command": f"python3 -c {shlex.quote(code)}"}, "cancel-exec"
        else:
            if task == "CREATE_α🦀":
                assert json.loads(messages[-1]["content"])["is_error"] is False
                assert (workspace / "effect.txt").read_text() == "preserved α🦀\n"
            if task == "REOPEN":
                assert any(message.get("tool_call_id") == "create-file" for message in messages)
                assert any(message.get("content") == "COMMITTED_α🦀" for message in messages)
            answer = {"CREATE_α🦀": "COMMITTED_α🦀", "REOPEN": "REOPEN_OK", "RECOVER": "RECOVERED", "OUTPUT_FAILURE": "COMMITTED_BUT_UNDELIVERED"}[task]
            delta, reason = {"content": answer}, "stop"
        if task == "CANCEL" or (task == "CREATE_α🦀" and messages[-1]["role"] != "tool"):
            delta = {"tool_calls": [{"index": 0, "id": call_id, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
            reason = "tool_calls"
        events = [{"choices": [{"index": 0, "delta": delta, "finish_reason": None}]}, {"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}]
        wire = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(wire)))
        self.end_headers()
        self.wfile.write(wire)

    def log_message(self, *args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-embedded-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    other = root / "other"
    other.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
    env.update(HOME=str(root / "home"), XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Peer)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    def command(session, prompt, cwd=workspace):
        return [example, "--cwd", cwd, "--session", root / session, "--provider", "sdk-fixture", "--model", "sdk-fixture", prompt]

    def inspect(session):
        result = subprocess.run([cli, "--cwd", workspace, "--session", root / session, "inspect"], env=env, capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    def reasons(view):
        assert view["unfinished_turn"] is None, view
        return [entry["data"]["reason"] for entry in view["entries"] if entry["kind"] == "turn_ended"]

    def run(session, prompt, answer):
        result = subprocess.run(command(session, prompt), env=env, capture_output=True, text=True, timeout=15)
        assert result.returncode == 0 and result.stdout == answer + "\n", result

    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([cli, "use", "sdk-fixture", "sdk-fixture", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        run("coding.sqlite", "CREATE_α🦀", "COMMITTED_α🦀")
        run("coding.sqlite", "REOPEN", "REOPEN_OK")
        saved = inspect("coding.sqlite")
        assert reasons(saved) == ["completed", "completed"]
        count = len(requests)
        mismatch = subprocess.run(command("coding.sqlite", "REOPEN", other), env=env, capture_output=True, timeout=10)
        assert mismatch.returncode != 0 and len(requests) == count and inspect("coding.sqlite") == saved

        process = subprocess.Popen(command("cancel.sqlite", "CANCEL"), env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 10
            while not (workspace / "started.txt").exists():
                assert process.poll() is None, "example exited before the native effect"
                assert time.monotonic() < deadline, "native effect did not start"
                time.sleep(0.025)
            process.send_signal(signal.SIGINT)
            output, errors = process.communicate(timeout=10)
            assert process.returncode != 0 and output == b"", (process.returncode, output, errors)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        cancelled = inspect("cancel.sqlite")
        assert reasons(cancelled) == ["cancelled"]
        outcomes = [entry["data"]["result"] for entry in cancelled["entries"] if entry["kind"] == "tool_result"]
        assert len(outcomes) == 1 and outcomes[0]["call_id"] == "cancel-exec" and outcomes[0]["outcome"]["state"] == "observed", outcomes
        run("cancel.sqlite", "RECOVER", "RECOVERED")
        recovered = inspect("cancel.sqlite")
        assert reasons(recovered) == ["cancelled", "completed"]
        assert sum(entry["kind"] == "tool_result" for entry in recovered["entries"]) == 1
        assert (workspace / "started.txt").read_text() == "started once"

        process = subprocess.Popen(command("delivery.sqlite", "OUTPUT_FAILURE"), env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            process.stdout.close()
            process.stdout = None
            _, errors = process.communicate(timeout=10)
            assert process.returncode != 0, errors
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()
        delivery = inspect("delivery.sqlite")
        assert reasons(delivery) == ["completed"]
        assert any({"Text": "COMMITTED_BUT_UNDELIVERED"} in entry["data"]["message"]["content"] for entry in delivery["entries"] if entry["kind"] == "assistant")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)

print("Embedded SDK smoke passed")
