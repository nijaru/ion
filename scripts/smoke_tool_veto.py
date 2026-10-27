"""Offline real-client oracle for operator veto capture and child inheritance."""
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import threading
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))


def facts(path):
    with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as db:
        return [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]


def tool(index, name, arguments):
    return {"index": index, "id": f"call-{index}", "type": "function",
            "function": {"name": name, "arguments": json.dumps(arguments)}}


with tempfile.TemporaryDirectory(prefix="ion-tool-veto-client-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    child_workspace = workspace / "child"
    child_workspace.mkdir(parents=True)
    (workspace / "input").write_text("ROOT_READ")
    (child_workspace / "input").write_text("CHILD_READ")
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    for key, directory in (("HOME", "home"), ("ION_HOME", "ion"),
                           ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                           ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state"),
                           ("TMPDIR", "tmp")):
        path = root / directory
        path.mkdir()
        env[key] = str(path)
    env["SHELL"] = "/bin/sh"
    policy_log = root / "policy.jsonl"
    policy = root / "policy.py"
    policy.write_text('''import json, sys
assert sys.argv[1] == "fixed ' $ ;"
event = json.load(sys.stdin)
with open(sys.argv[2], 'a') as log: log.write(json.dumps(event) + '\\n')
# Exceed ordinary pipe capacity without exposing diagnostics to the model.
sys.stderr.write('PRIVATE_POLICY_DIAGNOSTIC' + 'x' * 131072)
print(json.dumps({'decision':'deny', 'reason':'operator refused write'} if event['tool_name'] == 'write' else {'decision':'allow'}))
''')
    settings = Path(env["ION_HOME"]) / "settings.json"
    settings.write_text(json.dumps({"tool_veto": {"program": sys.executable,
                        "args": ["-P", str(policy), "fixed ' $ ;", str(policy_log)]}}))
    requests, failures = [], []
    original = "literal ' $(touch INJECTED)\nUnicode 🦀"

    def run(*arguments, check=True):
        return subprocess.run([BINARY, "--cwd", workspace, *arguments], env=env,
                              capture_output=True, text=True, timeout=20, check=check)

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(body)
                assert "PRIVATE_POLICY_DIAGNOSTIC" not in json.dumps(body)
                index, prompt = next((index, message["content"])
                    for index, message in reversed(list(enumerate(body["messages"])))
                    if message["role"] == "user" and not message["content"].startswith(
                        "Automated managed-work readiness (not a user instruction)."))
                fresh = not any(message["role"] == "assistant" for message in body["messages"][index + 1:])
                if prompt == "ROOT_VETO" and fresh:
                    # A child prepared after this edit must use its parent's
                    # captured policy, not reinterpret ambient operator state.
                    settings.write_text(json.dumps({"tool_veto": {
                        "program": "PRIVATE_REPLACEMENT_PROGRAM", "timeout_ms": 0}}))
                    code = "const rs=await Promise.allSettled([tools.read({path:'input'}),tools.write({path:'composed',content:'NO'})]); return rs.map(r=>r.status);"
                    calls = [tool(0, "write", {"path": "direct", "content": original}),
                             tool(1, "code_mode", {"code": code}),
                             tool(2, "subagent", {"action": "start", "task": "CHILD_VETO",
                                  "cwd": str(child_workspace), "mode": "foreground"})]
                    delta = {"tool_calls": calls}
                elif prompt == "CHILD_VETO" and fresh:
                    delta = {"tool_calls": [tool(0, "read", {"path": "input"}),
                                           tool(1, "write", {"path": "managed", "content": "NO"})]}
                else:
                    assert prompt in ("ROOT_VETO", "CHILD_VETO"), prompt
                    delta = {"content": "CHILD_DONE" if prompt == "CHILD_VETO" else "ROOT_DONE"}
                reason = "tool_calls" if "tool_calls" in delta else "stop"
                events = [{"id": "veto", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                          {"id": "veto", "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}]
                payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except Exception as error:
                failures.append(repr(error))
                self.send_error(500, str(error))

        def log_message(self, *_arguments):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        subprocess.run([BINARY, "use", "smoke", "veto", "--wire", "chat-completions",
                        "--max-output-tokens", "256", "--endpoint",
                        f"http://127.0.0.1:{server.server_port}/v1"],
                       env=env, check=True, capture_output=True, text=True, timeout=20)
        before = set(Path(env["ION_HOME"]).rglob("*.sqlite"))
        run("doctor")
        assert not policy_log.exists() and not requests
        assert set(Path(env["ION_HOME"]).rglob("*.sqlite")) == before
        result = run("--tools", "read,write,code_mode,subagent", "--code-mode", "--json", "run", "ROOT_VETO")
        assert not failures, failures
        assert "PRIVATE_POLICY_DIAGNOSTIC" not in result.stdout + result.stderr
        assert len(requests) == 4, requests
        allocated = set(Path(env["ION_HOME"]).rglob("*.sqlite")) - before
        assert len(allocated) == 2, allocated
        parent = next(path for path in allocated if path.parent.name != "children")
        parent_facts = facts(parent)
        admission = next(entry["data"]["record"] for entry in parent_facts if entry["kind"] == "delegation_admitted")
        child_facts = facts(Path(admission["path"]))
        parent_results = {entry["data"]["result"]["name"]: entry["data"]["result"]["outcome"]
                          for entry in parent_facts if entry["kind"] == "tool_result"}
        assert parent_results["write"]["state"] == "not_dispatched", parent_results
        assert parent_results["code_mode"]["output"]["value"]["result"] == ["fulfilled", "rejected"], parent_results
        assert parent_results["subagent"]["output"]["value"]["joined"], parent_results
        child_results = {entry["data"]["result"]["name"]: entry["data"]["result"]["outcome"]
                         for entry in child_facts if entry["kind"] == "tool_result"}
        assert child_results["write"]["state"] == "not_dispatched", child_results
        assert child_results["read"]["output"]["value"]["content"] == "CHILD_READ", child_results
        composed = [entry["data"]["outcome"] for entry in parent_facts if entry["kind"] == "child_tool_result"]
        assert sorted(outcome["state"] for outcome in composed) == ["not_dispatched", "observed"], composed
        assert next(outcome for outcome in composed if outcome["state"] == "observed")["output"]["value"]["content"] == "ROOT_READ"
        assert not any((directory / name).exists() for directory, name in (
            (workspace, "direct"), (workspace, "composed"), (workspace, "INJECTED"), (child_workspace, "managed")))
        events = [json.loads(line) for line in policy_log.read_text().splitlines()]
        assert len(events) == 7, events
        assert next(event for event in events if event["tool_name"] == "write" and event["arguments"]["path"] == "direct")["arguments"]["content"] == original
        assert len({event["origin"]["session"] for event in events}) == 2
        assert sum(event["origin"].get("composed_child") is not None for event in events) == 2
        assert sum(event["cwd"] == str(child_workspace.resolve()) for event in events) == 2
        count = len(requests)
        refused = run("run", "NEW_BINDING", check=False)
        assert refused.returncode != 0 and "timeout_ms must be positive" in refused.stderr, refused
        diagnosed = run("doctor", check=False)
        assert diagnosed.returncode == 1 and "PRIVATE_REPLACEMENT_PROGRAM" not in diagnosed.stdout + diagnosed.stderr, diagnosed
        assert len(requests) == count and len(policy_log.read_text().splitlines()) == 7
        print("tool veto: inert diagnostics, operator capture, direct/Code Mode/managed-child denials, typed facts, original arguments, private stderr and fresh-binding validation: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
