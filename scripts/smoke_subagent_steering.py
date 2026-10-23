"""Offline built-client steering oracle: real input custody, HTTP and native effects."""
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
GUIDANCE = '/not-a-command "literal" 🦀'

with tempfile.TemporaryDirectory(prefix="ion-subagent-steering-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"), TMPDIR=str(root / "tmp"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
    Path(env["HOME"]).mkdir()
    Path(env["TMPDIR"]).mkdir()
    program = workspace / "owned_process.py"
    program.write_text("""import os,sys,time
from pathlib import Path
root=Path(__file__).parent
name=sys.argv[1]
(root/(name+'.pid')).write_text(str(os.getpid()))
(root/(name+'.before')).write_text('started')
(root/(name+'.ready')).touch()
deadline=time.monotonic()+20
while not (root/(name+'.release')).exists():
    if time.monotonic()>deadline: raise RuntimeError('fixture not released')
    time.sleep(0.01)
(root/(name+'.after')).write_text('completed')
""")
    failures, requests = [], []
    code = """
const ids = [];
const start = async (task, extra={}) => { const r=await tools.subagent({action:'start',task,mode:'background',...extra}); ids.push(r.value.id); return r.value.id; };
const steer = (child,message) => tools.subagent({action:'steer',id:child,message});
const wait = child => tools.subagent({action:'wait',ids:[child],timeout_ms:10000});
const ready = name => tools.exec({command:'while [ ! -f '+name+'.ready ]; do sleep 0.01; done',timeout_ms:10000});
try {
  const worker=await start('WORKER'), cancel=await start('CANCEL');
  await Promise.all([ready('WORKER'),ready('CANCEL')]);
  const first=await steer(worker,GUIDANCE);
  if (first.is_error) return {unsupported:first};
  const second=await steer(worker,GUIDANCE);
  const queued=await tools.subagent({action:'status',id:worker});
  const invalid=[];
  for (const message of ['   ','é'.repeat(40000)]) invalid.push(await steer(worker,message));
  invalid.push(await steer('00000000-0000-0000-0000-000000000000','no grant'));
  invalid.push(await tools.subagent({action:'steer',id:worker,message:'no route grant',model:'smoke/other'}));
  const cancelledGuidance=await steer(cancel,'return cancelled guidance');
  await tools.subagent({action:'cancel',id:cancel});
  const cancelled=await wait(cancel);
  const cancelledAck=await tools.subagent({action:'acknowledge',id:cancel});
  await tools.write({path:'WORKER.release',content:'release'});
  const joined=await wait(worker);
  const settled=await steer(worker,'must not start another Turn');
  await tools.subagent({action:'acknowledge',id:worker});
  const absent=await steer(worker,'acknowledged is not authority');
  const deadline=await start('DEADLINE',{execution_timeout_ms:3000});
  await ready('DEADLINE');
  const deadlineGuidance=await steer(deadline,'return expired guidance');
  const expired=await wait(deadline);
  return {worker,cancel,deadline,first,second,queued,invalid,cancelledGuidance,cancelled,cancelledAck,joined,settled,absent,deadlineGuidance,expired};
} finally {
  for (const child of ids) await tools.subagent({action:'cancel',id:child});
  for (const name of ['WORKER','CANCEL','DEADLINE']) await tools.write({path:name+'.release',content:'cleanup'});
}
""".replace("GUIDANCE", json.dumps(GUIDANCE))

    def tool(name, arguments):
        return {"tool_calls": [{"index": 0, "id": "fixture", "type": "function",
                                "function": {"name": name, "arguments": json.dumps(arguments)}}]}

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                assert body['model'] == 'steering', body['model']
                users = [m["content"] for m in body["messages"] if m["role"] == "user"]
                task = users[0]
                requests.append((task, body))
                count = sum(name == task for name, _ in requests)
                if task == "PARENT":
                    delta = tool("code_mode", {"code": code}) if count == 1 else {"content": "STEERING_DONE"}
                elif count == 1:
                    assert task in ("WORKER", "CANCEL", "DEADLINE"), task
                    assert "subagent" not in {t["function"]["name"] for t in body.get("tools", [])}
                    delta = tool("exec", {"command": "exec " + shlex.join([sys.executable, str(program), task]), "timeout_ms": 15000})
                elif task == "WORKER" and count == 2:
                    assert users == ["WORKER", GUIDANCE, GUIDANCE], users
                    roles = [m["role"] for m in body["messages"]]
                    assert roles[-3:] == ["tool", "user", "user"], roles
                    delta = tool("write", {"path": "FOLLOWED", "content": "\n".join(users[1:])})
                else:
                    assert task == "WORKER" and count == 3, (task, count)
                    assert not json.loads(body["messages"][-1]["content"])["is_error"]
                    delta = {"content": "GUIDED_ANSWER"}
                reason = "tool_calls" if "tool_calls" in delta else "stop"
                events = [{"id": "fixture", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                          {"id": "fixture", "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}]
                payload = b"".join(b"data: " + json.dumps(e).encode() + b"\n\n" for e in events) + b"data: [DONE]\n\n"
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
                                  text=True, check=True, timeout=50)

        run("use", "smoke", "steering", "--wire", "chat-completions", "--max-output-tokens", "256",
            "--endpoint", f"http://127.0.0.1:{server.server_port}/v1")
        process = run("--tools", "read,write,exec,subagent,code_mode", "--code-mode", "--json", "run", "PARENT")
        records = [json.loads(line) for line in process.stdout.splitlines()]
        result = next(r["output"]["result"] for r in records if r["type"] == "tool_finished")
        assert "first" in result, result
        assert not failures, failures
        for name in ("first", "second", "cancelledGuidance", "deadlineGuidance"):
            assert not result[name]["is_error"], result[name]
            assert result[name]["value"]["steering"]["status"]["state"] == "queued"
        assert result["first"]["value"]["steering"]["id"] != result["second"]["value"]["steering"]["id"]
        assert all(r["status"]["state"] == "queued" for r in result["queued"]["value"]["steering"])
        assert all(r["is_error"] for r in result["invalid"])
        assert result["settled"]["is_error"] and result["absent"]["is_error"]
        [joined] = result["joined"]["value"]["runs"]
        assert joined["joined"] and joined["outcome"]["answer"] == "GUIDED_ANSWER", joined
        assert len(joined["steering"]) == 2
        assert all(r["status"]["state"] == "committed" and r["status"]["acceptance"]["turn"] == 1 for r in joined["steering"])
        assert result['cancelledAck']['value'] == result['cancelled']['value']['runs'][0], result['cancelledAck']
        for name, message, timed_out in (("cancelled", "return cancelled guidance", False), ("expired", "return expired guidance", True)):
            [child] = result[name]["value"]["runs"]
            assert child["joined"] and child["outcome"]["execution_timed_out"] == timed_out, child
            assert child["outcome"]["answer"] is None and child["outcome"]["receipt"]["end"] == "cancelled"
            [guidance] = child["steering"]
            assert guidance["status"]["state"] == "returned" and guidance["status"]["reason"] == "Stopped", guidance
            assert guidance["status"]["input"]["Message"]["content"] == [{"Text": message}], guidance
        assert (workspace / "FOLLOWED").read_text() == GUIDANCE + "\n" + GUIDANCE
        assert (workspace / "WORKER.after").exists()
        assert all(not (workspace / (n + ".after")).exists() for n in ("CANCEL", "DEADLINE"))
        assert [sum(task == n for task, _ in requests) for n in ("PARENT", "WORKER", "CANCEL", "DEADLINE")] == [2, 3, 1, 1]
        parent = next(p for p in Path(env["ION_HOME"]).rglob("*.sqlite") if p.parent.name != "children")
        with closing(sqlite3.connect(f"file:{parent}?mode=ro", uri=True)) as db:
            entries = [json.loads(r[0]) for r in db.execute("SELECT body FROM entries ORDER BY seq")]
        admissions = [e["data"]["record"] for e in entries if e["kind"] == "delegation_admitted"]
        assert len(admissions) == len([e for e in entries if e["kind"] == "delegation_settled"]) == 3
        for record in admissions:
            with closing(sqlite3.connect(f"file:{record['path']}?mode=ro", uri=True)) as db:
                facts = [json.loads(r[0]) for r in db.execute("SELECT body FROM entries ORDER BY seq")]
            steering = [e["data"]["input"] for e in facts if e["kind"] == "steering"]
            assert steering == ([{"role": "User", "content": [{"Text": GUIDANCE}], "provider_replay": None}] * 2 if record["run"] == result["worker"] else [])
            assert len([e for e in facts if e["kind"] == "turn_started"]) == 1, "steering must never start another Turn"
        for path in workspace.glob("*.pid"):
            pid = int(path.read_text())
            assert subprocess.run(["ps", "-p", str(pid)], capture_output=True, check=False).returncode != 0, f"owned process alive: {pid}"
        print("coding steering: literal FIFO guidance committed, native effect and HTTP/facts verified; cancellation/deadline return originals, settled/absent grants rejected: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
        for path in workspace.glob("*.pid"):
            pid = int(path.read_text())
            command = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True, check=False).stdout
            if str(program) in command:
                os.kill(pid, signal.SIGKILL)
