"""Native refusal reasons, resource-only reload and explicit policy rebinding."""

import errno
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

from terminal_style import painted_text, program_ready

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion")).resolve()
DIRECT = "Operator blocked direct — café 🦀"
NESTED = "Operator blocked nested — café 🦀"
PRIVATE = "PRIVATE_POLICY_DIAGNOSTIC"
POLICY = '''import json, sys
mode, log = sys.argv[1:]
event = json.load(sys.stdin)
with open(log, 'a') as output:
    output.write(json.dumps({'mode':mode, **event}) + '\\n')
sys.stderr.write('PRIVATE_POLICY_DIAGNOSTIC')
if mode == 'broken':
    print('PRIVATE_POLICY_DIAGNOSTIC')
    sys.exit(7)
if mode == 'deny' and event['tool_name'] == 'write':
    reason = 'Operator blocked ' + event['arguments']['path'] + ' — café 🦀'
    print(json.dumps({'decision':'deny', 'reason':reason}))
else:
    print(json.dumps({'decision':'allow'}))
'''


class Case:
    def __init__(self, root):
        self.root = root
        self.workspace = root / "workspace"
        (self.workspace / ".git").mkdir(parents=True)
        (self.workspace / "input").write_text("READ_ONLY_CONTENT")
        self.env = {key: os.environ[key] for key in ("PATH",) if key in os.environ}
        for key, name in (("HOME", "home"), ("ION_HOME", "ion"),
                          ("XDG_CONFIG_HOME", "config"), ("XDG_STATE_HOME", "state"), ("TMPDIR", "tmp")):
            directory = root / name
            directory.mkdir()
            self.env[key] = str(directory)
        self.env.update(TERM="xterm-256color", LANG="C.UTF-8")
        self.policy = root / "policy.py"
        self.policy.write_text(POLICY)
        self.log = root / "policy.jsonl"
        self.settings = Path(self.env["ION_HOME"]) / "settings.json"
        self.configure("deny")
        self.requests, self.errors = [], []

    def configure(self, mode):
        self.settings.write_text(json.dumps({"tool_veto": {"program": sys.executable,
            "args": [str(self.policy), mode, str(self.log)]}}))

    def run(self, *args):
        return subprocess.run([str(BINARY), *map(str, args)], cwd=self.workspace,
                              env=self.env, check=True, capture_output=True, timeout=15)

    def events(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def handler(self):
        case = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    step = len(case.requests)
                    case.requests.append(body)
                    assert PRIVATE not in json.dumps(body), "policy diagnostics leaked to the model"
                    assert {"read", "write", "code_mode"}.issubset(
                        tool["function"]["name"] for tool in body["tools"])
                    if step == 0:
                        # Settings change while the current binding remains live.
                        case.configure("broken")
                        code = "try { await tools.write({path:'nested',content:'NO'}); } catch (_) { return 'CHILD_REFUSED'; }"
                        calls = [("read", "read", {"path": "input"}),
                                 ("compose", "code_mode", {"code": code}),
                                 ("direct", "write", {"path": "direct", "content": "NO"})]
                    elif step == 2:
                        calls = [("retained", "write", {"path": "retained", "content": "NO"})]
                    elif step == 4:
                        case.configure("allow")
                        calls = [("broken", "write", {"path": "broken", "content": "NO"})]
                    elif step == 6:
                        calls = [("allowed", "write", {"path": "allowed", "content": "ONE_ALLOWED_EFFECT"})]
                    else:
                        assert step in (1, 3, 5, 7), step
                        calls = []
                    if calls:
                        delta = {"tool_calls": [{"index": index, "id": call_id, "type": "function",
                            "function": {"name": name, "arguments": json.dumps(args)}}
                            for index, (call_id, name, args) in enumerate(calls)]}
                    else:
                        delta = {"content": {1: "FIRST_DONE", 3: "RETAINED_DONE", 5: "BROKEN_DONE", 7: "ALLOWED_DONE"}[step]}
                    events = [
                        {"id": "veto", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                        {"id": "veto", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if calls else "stop"}]},
                    ]
                    payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except Exception as error:
                    case.errors.append(repr(error))
                    self.send_error(500, str(error))

        return Handler

    def check_finished(self):
        assert not self.errors, self.errors
        assert len(self.requests) == 8, len(self.requests)
        assert (self.workspace / "allowed").read_text() == "ONE_ALLOWED_EFFECT"
        assert all(not (self.workspace / name).exists() for name in ("direct", "nested", "retained", "broken"))
        events = self.events()
        assert [(event["mode"], event["tool_name"]) for event in events] == [
            ("deny", "read"), ("deny", "code_mode"), ("deny", "write"), ("deny", "write"),
            ("deny", "write"), ("broken", "write"), ("allow", "write")], events
        assert events[2]["origin"]["composed_child"] == 0
        assert len({event["origin"]["session"] for event in events}) == 1
        assert all(event["cwd"] == str(self.workspace.resolve()) for event in events)
        sessions = list((self.root / "ion").rglob("*.sqlite"))
        assert len(sessions) == 1, sessions
        view = json.loads(self.run("--session", sessions[0], "inspect").stdout)
        results = {entry["data"]["result"]["call_id"]: entry["data"]["result"]["outcome"]
                   for entry in view["entries"] if entry["kind"] == "tool_result"}
        assert {key: value["state"] for key, value in results.items()} == {
            "read": "observed", "compose": "observed", "direct": "not_dispatched",
            "retained": "not_dispatched", "broken": "not_dispatched", "allowed": "observed"}, results
        assert DIRECT in results["direct"]["reason"], results
        assert "exited unsuccessfully" in results["broken"]["reason"], results
        children = [entry["data"]["outcome"] for entry in view["entries"] if entry["kind"] == "child_tool_result"]
        assert len(children) == 1 and children[0]["state"] == "not_dispatched" and NESTED in children[0]["reason"], children
        assert PRIVATE not in json.dumps(view), "policy diagnostics entered the journal"
        ends = [entry["data"] for entry in view["entries"] if entry["kind"] == "turn_ended"]
        assert len(ends) == 4 and all(entry["reason"] == "completed" for entry in ends), ends
        assert view["unfinished_turn"] is None
        return view


@contextmanager
def veto_case():
    with tempfile.TemporaryDirectory(prefix="ion-terminal-veto-") as temporary:
        case = Case(Path(temporary))
        server = HTTPServer(("127.0.0.1", 0), case.handler())
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            case.run("use", "smoke", "veto-model", "--endpoint",
                     f"http://127.0.0.1:{server.server_port}/v1", "--wire", "chat-completions")
            case.run("doctor")
            assert not case.log.exists() and not case.requests
            yield case
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            assert not thread.is_alive()


def qualify(mode):
    with veto_case() as case:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 120, 0, 0))
        before = termios.tcgetattr(slave)

        def attach():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen([str(BINARY), "--cwd", str(case.workspace), "--tools", "read,write,code_mode",
                                  "--code-mode", "--tui-mode", mode, "chat"], env=case.env,
                                 stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach)
        output, pending = bytearray(), b""

        def collect():
            nonlocal pending
            if not select.select([master], [], [], 0.05)[0]:
                return
            try:
                data = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                return
            output.extend(data)
            pending += data
            while b"\x1b[6n" in pending:
                pending = pending.split(b"\x1b[6n", 1)[1]
                os.write(master, b"\x1b[2;1R")
            pending = pending[-8:]

        def wait(predicate):
            deadline = time.monotonic() + 15
            while not predicate():
                assert child.poll() is None and time.monotonic() < deadline, (mode, case.errors, output[-4000:])
                collect()

        def message(text, marker):
            output.clear()
            os.write(master, text.encode() + b"\r")
            wait(lambda: marker in output and program_ready(output)
                 and "prompts blocked" not in painted_text(output))
            assert PRIVATE.encode() not in output

        def reload(expected_events, expected_requests):
            output.clear()
            os.write(master, b"/reload\r")
            wait(lambda: "Reloaded" in painted_text(output) and "› " in painted_text(output))
            assert len(case.events()) == expected_events and len(case.requests) == expected_requests

        def rebind(expected_events, expected_requests):
            output.clear()
            os.write(master, b"/model smoke/veto-model\r")
            wait(lambda: "Selected smoke/veto-model" in painted_text(output)
                 and "› " in painted_text(output))
            assert len(case.events()) == expected_events and len(case.requests) == expected_requests

        try:
            wait(lambda: program_ready(output))
            assert not case.log.exists(), "startup executed the policy"
            message("Read and try direct and composed writes.", b"FIRST_DONE")
            visible = painted_text(output)
            assert DIRECT in visible and NESTED in visible, (mode, visible)
            assert "Skipped" in visible, visible
            reload(4, 2)
            message("Try a write after resource reload.", b"RETAINED_DONE")
            assert "Operator blocked retained" in painted_text(output)
            rebind(5, 4)
            message("Try a write while the policy is broken.", b"BROKEN_DONE")
            visible = painted_text(output)
            assert "tool veto program exited unsuccessfully" in visible, (mode, visible)
            rebind(6, 6)
            message("Try a write under the rebound policy.", b"ALLOWED_DONE")
            os.write(master, b"/exit\r")
            deadline = time.monotonic() + 10
            while child.poll() is None:
                assert time.monotonic() < deadline, output[-4000:]
                collect()
            assert child.returncode == 0, output[-4000:]
            after = termios.tcgetattr(master)
            if os.uname().sysname == "Darwin":
                for attrs in (before, after):
                    attrs[3] &= ~0x20000000
            assert after == before, (before, after)
            case.check_finished()
            print(f"Ion {mode} native veto: direct/nested reasons, resource-only reload, failed policy, rebind and one allowed effect: OK")
        finally:
            if child.poll() is None:
                child.kill()
            deadline = time.monotonic() + 10
            while child.poll() is None:
                assert time.monotonic() < deadline, "native client did not exit after cleanup"
                time.sleep(0.02)
            os.close(slave)
            os.close(master)


if __name__ == "__main__":
    for mode in ("inline", "fullscreen"):
        qualify(mode)
