"""Offline real-client oracle for bounded, non-consuming managed-child wait-any."""
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
import time
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))

with tempfile.TemporaryDirectory(prefix="ion-subagent-wait-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"), TMPDIR=str(root / "tmp"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
    Path(env["HOME"]).mkdir()
    Path(env["TMPDIR"]).mkdir()
    failures, child_requests = [], []
    closing_server = threading.Event()
    code = """
const start = task => tools.subagent({action:'start',task,mode:'background'});
const release = child => tools.write({path:child+'.release',content:'release'});
const [slow, fast] = await Promise.all([start('SLOW'),start('FAST')]);
const ids = [slow.value.id,fast.value.id];
try {
  const expired = await tools.subagent({action:'wait',ids:ids,timeout_ms:10});
  if (expired.is_error) return {unsupported:expired.value};
  await release('FAST');
  const first = await tools.subagent({action:'wait',ids:ids,timeout_ms:5000});
  const repeated = await tools.subagent({action:'wait',ids:[ids[1],ids[0]],timeout_ms:1});
  const invalid = [];
  for (const args of [
    {ids:[ids[1],'00000000-0000-0000-0000-000000000000']},
    {ids:[ids[1],ids[1]]},{ids:[]},
    {ids:[ids[0]],timeout_ms:0},{ids:[ids[0]],timeout_ms:600001}
  ]) invalid.push(await tools.subagent({action:'wait',...args}));
  const ack = await tools.subagent({action:'acknowledge',id:ids[1]});
  const absent = await tools.subagent({action:'wait',ids:[ids[1]],timeout_ms:1});
  await release('SLOW');
  const last = await tools.subagent({action:'wait',ids:[ids[0]]});
  const retained = await tools.subagent({action:'status',id:ids[0]});
  return {ids,expired,first,repeated,invalid,ack,absent,last,retained};
} finally {
  await Promise.all([release('FAST'),release('SLOW')]);
}
"""

    def tool(name, arguments):
        return {"tool_calls": [{"index": 0, "id": "fixture", "type": "function",
                                "function": {"name": name, "arguments": json.dumps(arguments)}}]}

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                user = next(message["content"] for message in reversed(body["messages"])
                            if message["role"] == "user")
                fresh = body["messages"][-1]["role"] == "user"
                if user == "PARENT":
                    delta = tool("code_mode", {"code": code}) if fresh else {"content": "PARENT_WAIT_DONE"}
                else:
                    assert user in ("FAST", "SLOW"), user
                    child_requests.append(user)
                    assert "subagent" not in {item["function"]["name"] for item in body.get("tools", [])}
                    if fresh:
                        deadline = time.monotonic() + 15
                        while not (workspace / (user + ".release")).exists():
                            if closing_server.wait(0.01):
                                return
                            assert time.monotonic() < deadline, f"{user} never released"
                        delta = tool("write", {"path": user + ".effect", "content": "OBSERVED_" + user})
                    else:
                        assert not json.loads(body["messages"][-1]["content"])["is_error"]
                        delta = {"content": "ANSWER_" + user}
                reason = "tool_calls" if "tool_calls" in delta else "stop"
                events = [{"id": "fixture", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                          {"id": "fixture", "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}]
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
        def run(*arguments):
            return subprocess.run([BINARY, "--cwd", workspace, *arguments], env=env, capture_output=True,
                                  text=True, check=True, timeout=25)

        run("use", "smoke", "wait", "--wire", "chat-completions", "--max-output-tokens", "256",
            "--endpoint", f"http://127.0.0.1:{server.server_port}/v1")
        process = run("--tools", "read,write,subagent,code_mode", "--code-mode", "--json", "run", "PARENT")
        records = [json.loads(line) for line in process.stdout.splitlines()]
        assert not failures, failures
        result = next(record["output"]["result"] for record in records if record["type"] == "tool_finished")
        assert "expired" in result, result
        slow, fast = result["ids"]
        expired = result["expired"]
        assert not expired["is_error"] and expired["value"]["wait_expired"], expired
        assert [child["id"] for child in expired["value"]["runs"]] == [slow, fast]
        assert all(not child["joined"] for child in expired["value"]["runs"])
        first = result["first"]
        assert not first["is_error"] and not first["value"]["wait_expired"], first
        held, ready = first["value"]["runs"]
        assert held["id"] == slow and not held["joined"]
        assert ready["id"] == fast and ready["joined"]
        assert ready["outcome"]["answer"] == "ANSWER_FAST", ready
        assert result["repeated"]["value"] == {"wait_expired": False, "runs": [ready, held]}
        assert all(item["is_error"] and "error" in item["value"] for item in result["invalid"])
        assert result["ack"]["value"] == ready and not result["ack"]["is_error"]
        assert result["absent"]["is_error"], result["absent"]
        last = result["last"]
        assert not last["is_error"] and not last["value"]["wait_expired"], last
        [joined] = last["value"]["runs"]
        assert joined["id"] == slow and joined["joined"]
        assert joined["outcome"]["answer"] == "ANSWER_SLOW"
        assert result["retained"]["value"] == joined
        assert child_requests.count("FAST") == child_requests.count("SLOW") == 2, child_requests
        for name in ("FAST", "SLOW"):
            assert (workspace / (name + ".effect")).read_text() == "OBSERVED_" + name
        parent = next(path for path in Path(env["ION_HOME"]).rglob("*.sqlite") if path.parent.name != "children")
        with closing(sqlite3.connect(f"file:{parent}?mode=ro", uri=True)) as db:
            entries = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
        admissions = [entry["data"]["record"]["run"] for entry in entries if entry["kind"] == "delegation_admitted"]
        outcomes = [entry["data"] for entry in entries if entry["kind"] == "delegation_settled"]
        assert set(admissions) == {slow, fast} and len(outcomes) == 2, (admissions, outcomes)
        print("coding wait-any: expiry preserves work, first completion wins, reports retained, selection validated, default wait joins; HTTP/effects/facts verified: OK")
    finally:
        closing_server.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
