"""Check actual inline publication in tmux, whose full-screen erase saves history."""

import json
import os
import shlex
import shutil
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
assert shutil.which("tmux"), "the native-scrollback smoke requires tmux"

with tempfile.TemporaryDirectory(prefix="ion-scrollback-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "data.txt").write_text("observed data\n")
    home = work / "home"
    home.mkdir()
    env = os.environ.copy()
    env.update(
        HOME=str(home),
        XDG_CONFIG_HOME=str(work / "config"),
        XDG_STATE_HOME=str(work / "state"),
        TERM="xterm-256color",
    )
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(body)
            if len(requests) in (1, 3):
                calls = [
                    ("read", {"path": "data.txt"}),
                    ("edit", {"path": "data.txt", "edits": [{"old_text": "observed data", "new_text": "UPDATED_SNAPSHOT"}]}),
                    ("exec", {"command": "printf 'FIRST_%s\\nmid-one\\nmid-two\\nCOMMAND_OUTPUT_%s\\n' HIDDEN ONCE"}),
                ] if len(requests) == 1 else [
                    ("exec", {"command": "printf 'FUTURE_%s\\nmid-one\\nmid-two\\nfuture-last\\n' FIRST"}),
                ]
                delta = {
                    "content": "NARRATIVE_ONCE" if len(requests) == 1 else "FUTURE_NARRATIVE",
                    "tool_calls": [
                        {"index": index, "id": f"call-{index}", "type": "function",
                         "function": {"name": name, "arguments": json.dumps(arguments)}}
                        for index, (name, arguments) in enumerate(calls)
                    ],
                }
                finish = "tool_calls"
            else:
                assert len(requests) in (2, 4), "unexpected additional model request"
                # Leave the completed calls visible in the mutable surface
                # before the final observation is committed and published.
                time.sleep(0.75)
                delta, finish = {"content": "FINAL_ONCE" if len(requests) == 2 else "FUTURE_DONE"}, "stop"
            payload = b"data: " + json.dumps({
                "id": "scrollback", "model": "smoke-model",
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            }).encode() + b"\n\ndata: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    serving = threading.Thread(target=server.serve_forever)
    serving.start()
    socket = str(work / "tmux.sock")

    def tmux(*args):
        return subprocess.check_output(["tmux", "-S", socket, *args], env=env).decode()

    try:
        subprocess.run(
            [binary, "use", "smoke", "smoke-model", "--endpoint",
             f"http://127.0.0.1:{server.server_port}/v1/chat/completions",
             "--wire", "chat-completions"], env=env, check=True, capture_output=True,
        )
        exit_path = work / "exit-status"
        launcher = work / "launch.sh"
        launcher.write_text(
            f"#!/bin/sh\n{shlex.quote(str(binary))} chat\nstatus=$?\n"
            f"printf '%s\\n' \"$status\" > {shlex.quote(str(exit_path))}\nexit \"$status\"\n"
        )
        launcher.chmod(0o700)
        tmux("new-session", "-d", "-s", "ion", "-x", "100", "-y", "30",
             "-c", str(workspace), str(launcher))
        deadline = time.monotonic() + 35
        while "›" not in tmux("capture-pane", "-p", "-t", "ion"):
            assert time.monotonic() < deadline, "inline composer did not start"
            time.sleep(0.02)
        tmux("send-keys", "-t", "ion", "-l", "INPUT_ONCE: read data.txt and check it")
        tmux("send-keys", "-t", "ion", "Enter")
        while "FINAL_ONCE" not in tmux("capture-pane", "-p", "-t", "ion"):
            assert time.monotonic() < deadline, "inline Turn did not finish"
            time.sleep(0.02)

        time.sleep(0.1)

        def check_history():
            history = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
            for marker in ("INPUT_ONCE", "NARRATIVE_ONCE", "COMMAND_OUTPUT_ONCE", "FINAL_ONCE", "-observed data", "+UPDATED_SNAPSHOT"):
                assert history.count(marker) == 1, f"{marker} was republished or lost:\n{history}"
            assert "Working" not in history, f"mutable operation chrome leaked into history:\n{history}"

        check_history()
        assert (workspace / "data.txt").read_text() == "UPDATED_SNAPSHOT\n"
        tmux("send-keys", "-t", "ion", "-l", "/tool 2")
        tmux("send-keys", "-t", "ion", "Enter")
        while "Recorded edit diff" not in tmux("capture-pane", "-p", "-t", "ion"):
            assert time.monotonic() < deadline, "recorded edit inspection did not open"
            time.sleep(0.02)
        detail = tmux("capture-pane", "-p", "-t", "ion")
        assert "-observed data" in detail and "+UPDATED_SNAPSHOT" in detail, detail
        tmux("send-keys", "-t", "ion", "Escape")
        time.sleep(0.1)
        check_history()

        def command(text, expected):
            tmux("send-keys", "-t", "ion", "-l", text)
            tmux("send-keys", "-t", "ion", "Enter")
            while expected not in tmux("capture-pane", "-p", "-t", "ion"):
                assert time.monotonic() < deadline, f"command did not complete: {text}"
                time.sleep(0.02)

        assert "FIRST_HIDDEN" not in tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
        command("/settings expanded", "Tool output: expanded")
        check_history()
        tmux("resize-window", "-t", "ion", "-x", "100", "-y", "60")
        command("/tui fullscreen", "FIRST_HIDDEN")
        assert '--- "data.txt"' in tmux("capture-pane", "-p", "-t", "ion"), "expanded diff header missing"
        command("/settings compact", "Tool output: compact")
        time.sleep(0.1)
        assert "FIRST_HIDDEN" not in tmux("capture-pane", "-p", "-t", "ion"), "compact view exposed omitted output"
        command("/tui inline", "native terminal scrollback")
        check_history()
        assert "FIRST_HIDDEN" not in tmux("capture-pane", "-p", "-t", "ion", "-S", "-"), "settings republished old native output"
        command("/settings expanded", "Tool output: expanded")
        command("NEXT_INPUT: run a follow-up command", "FUTURE_DONE")
        time.sleep(0.1)
        history = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
        assert history.count("FUTURE_FIRST") == 1, "expanded future publication omitted or duplicated recorded output"
        assert len(requests) == 4, "settings changed model requests"
        check_history()
        tmux("resize-window", "-t", "ion", "-x", "60", "-y", "20")
        time.sleep(0.15)
        check_history()
        tmux("send-keys", "-t", "ion", "C-c")
        while not exit_path.exists():
            assert time.monotonic() < deadline, "inline client did not exit"
            time.sleep(0.02)
        assert exit_path.read_text().strip() == "0", "inline client failed on exit"
    finally:
        subprocess.run(["tmux", "-S", socket, "kill-server"], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        server.shutdown()
        serving.join()
        server.server_close()

print("Ion inline native history: publish once, no mutable chrome, resize: OK")
