"""Offline built-client oracle: a join expiry transfers the same native child."""
import json
import os
import shlex
import signal
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


def dead(pid):
    try:
        os.kill(pid, 0)
        return False
    except ProcessLookupError:
        return True


with tempfile.TemporaryDirectory(prefix="ion-subagent-join-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"), TMPDIR=str(root / "tmp"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
    Path(env["HOME"]).mkdir()
    Path(env["TMPDIR"]).mkdir()
    program = workspace / "owned_child.py"
    program.write_text('''import json, os, secrets, time
from pathlib import Path
record = dict(pid=os.getpid(), cwd=os.getcwd(), nonce=secrets.token_hex(12))
with Path('native.starts').open('a') as output:
    output.write(json.dumps(record)+'\\n')
Path('native.before').write_text(json.dumps(record))
while not Path('native.release').exists():
    time.sleep(0.01)
Path('native.after').write_text(json.dumps(record))
print(json.dumps(record))
''')
    code = """
const start = args => tools.subagent({action:'start',...args});
const invalid = [];
for (const join_timeout_ms of [0,600001,null])
  invalid.push(await start({task:'INVALID',join_timeout_ms}));
invalid.push(await start({task:'INVALID',mode:'background',join_timeout_ms:10}));
const fast = await start({task:'FAST'});
const empty = await tools.subagent({action:'list'});
const held = await start({task:'HELD',join_timeout_ms:50});
if (held.is_error) return {unsupported:held};
try {
  const ready = await tools.exec({command:'while [ ! -f native.before ]; do sleep 0.01; done',timeout_ms:10000});
  const live = await tools.subagent({action:'status',id:held.value.id});
  const brief = await tools.subagent({action:'wait',ids:[held.value.id],timeout_ms:10});
  const guided = await tools.subagent({action:'steer',id:held.value.id,message:'FOLLOW_LITERAL'});
  await tools.write({path:'native.release',content:'release'});
  const joined = await tools.subagent({action:'wait',ids:[held.value.id],timeout_ms:10000});
  const same = await tools.subagent({action:'status',id:held.value.id});
  const ack = await tools.subagent({action:'acknowledge',id:held.value.id});
  const absent = await tools.subagent({action:'status',id:held.value.id});
  return {invalid,fast,empty,held,ready,live,brief,guided,joined,same,ack,absent};
} finally {
  await tools.write({path:'native.release',content:'release'});
}
"""
    failures, requests, schemas = [], [], []

    def tool(name, arguments):
        return {"tool_calls": [{"index": 0, "id": "fixture", "type": "function",
                                "function": {"name": name, "arguments": json.dumps(arguments)}}]}

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                task = next(message["content"] for message in body["messages"] if message["role"] == "user")
                requests.append(task)
                count = requests.count(task)
                if task == "PARENT":
                    if count == 1:
                        schemas.append(next(item["function"]["parameters"] for item in body["tools"]
                                            if item["function"]["name"] == "subagent"))
                        delta = tool("code_mode", {"code": code})
                    else:
                        delta = {"content": "PARENT_JOIN_DONE"}
                else:
                    assert "subagent" not in {item["function"]["name"] for item in body.get("tools", [])}
                    if task == "FAST":
                        assert count == 1
                        delta = {"content": "FAST_ANSWER"}
                    else:
                        assert task == "HELD", task
                        if count == 1:
                            delta = tool("exec", {"command": "exec " + shlex.join([sys.executable, str(program)]),
                                                  "timeout_ms": 120000})
                        else:
                            assert count == 2 and "FOLLOW_LITERAL" in json.dumps(body), body
                            actual = json.loads((workspace / "native.after").read_text())
                            assert actual["nonce"] in json.dumps(body), "native result/history lost during transfer"
                            delta = {"content": "HELD_ANSWER_" + actual["nonce"]}
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
                                  text=True, check=True, timeout=30)

        run("use", "smoke", "join", "--wire", "chat-completions", "--max-output-tokens", "256",
            "--endpoint", f"http://127.0.0.1:{server.server_port}/v1")
        process = run("--tools", "read,write,exec,subagent,code_mode", "--code-mode", "--json", "run", "PARENT")
        records = [json.loads(line) for line in process.stdout.splitlines()]
        assert not failures, failures
        result = next(record["output"]["result"] for record in records if record["type"] == "tool_finished")
        assert "held" in result, result
        assert all(item["is_error"] for item in result["invalid"]), result["invalid"]
        fast, held = result["fast"]["value"], result["held"]["value"]
        assert fast["join_timeout_ms"] == 60000 and not fast["join_expired"]
        assert fast["joined"] and not fast["transferred"] and fast["outcome"]["answer"] == "FAST_ANSWER"
        assert result["empty"]["value"]["jobs"] == [] and result["empty"]["value"]["summary"] == {"running": 0, "joined": 0, "reserved": 0}, result["empty"]
        assert held["join_timeout_ms"] == 50 and held["join_expired"]
        assert held["transferred"] and not held["joined"] and held["outcome"] is None, held
        assert fast["id"] != held["id"] and fast["child_session"] != held["child_session"]
        assert not result["ready"]["is_error"]
        assert result["live"]["value"]["id"] == held["id"] and not result["live"]["value"]["joined"]
        assert not result["brief"]["is_error"] and result["brief"]["value"]["wait_expired"]
        assert not result["guided"]["is_error"]
        assert not result["joined"]["is_error"] and not result["joined"]["value"]["wait_expired"]
        [joined] = result["joined"]["value"]["runs"]
        assert joined["id"] == held["id"] and joined["child_session"] == held["child_session"]
        actual = json.loads((workspace / "native.before").read_text())
        assert actual == json.loads((workspace / "native.after").read_text())
        assert actual["cwd"] == str(workspace.resolve()) and dead(actual["pid"]), actual
        assert (workspace / "native.starts").read_text().splitlines() == [json.dumps(actual)]
        assert joined["joined"] and joined["outcome"]["answer"] == "HELD_ANSWER_" + actual["nonce"]
        assert not joined["outcome"]["execution_timed_out"]
        assert result["same"]["value"] == result["ack"]["value"] == joined
        assert result["absent"]["is_error"]
        assert requests.count("HELD") == requests.count("PARENT") == 2 and requests.count("FAST") == 1, requests
        start_schema = next(choice for choice in schemas[0]["oneOf"] if choice["properties"]["action"]["const"] == "start")
        budget = start_schema["properties"]["join_timeout_ms"]
        assert (budget["minimum"], budget["maximum"], budget["default"]) == (1, 600000, 60000)
        parent = next(path for path in Path(env["ION_HOME"]).rglob("*.sqlite") if path.parent.name != "children")
        with closing(sqlite3.connect(f"file:{parent}?mode=ro", uri=True)) as db:
            entries = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
        assert json.loads(run("--session", str(parent), "inspect").stdout)["entries"] == entries
        admissions = [entry["data"]["record"] for entry in entries if entry["kind"] == "delegation_admitted"]
        transferred = [entry["data"]["run"] for entry in entries if entry["kind"] == "delegation_transferred"]
        settled = {entry["data"]["run"]: entry["data"]["outcome"] for entry in entries if entry["kind"] == "delegation_settled"}
        assert len(admissions) == 2 and transferred == [held["id"]]
        assert settled == {fast["id"]: fast["outcome"], held["id"]: joined["outcome"]}
        for output in (fast, held):
            with closing(sqlite3.connect(f"file:{output['path']}?mode=ro", uri=True)) as db:
                header = json.loads(db.execute("SELECT header FROM session").fetchone()[0])
                child_entries = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
            assert header["id"] == output["child_session"]
            assert len([entry for entry in child_entries if entry["kind"] == "child_session_of"]) == 1
            assert len([entry for entry in child_entries if entry["kind"] == "turn_started"]) == 1
        print("coding join: fast completion releases reservation; expiry transfers same native run/control, no replay; retained reports and HTTP/effects/SQLite/inspection verified: OK")
    finally:
        (workspace / "native.release").touch()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
        before = workspace / "native.before"
        if before.exists():
            pid = json.loads(before.read_text())["pid"]
            if not dead(pid):
                command = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True, check=False).stdout
                if str(program) in command:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
