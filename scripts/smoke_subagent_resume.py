"""Offline built-client resume: one conversation, independent runs and fresh authority."""
import json
import os
import shlex
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import uuid
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))
GUIDE = '/literal follow-up "guidance" 🦀'


def facts(path):
    with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as db:
        header = json.loads(db.execute("SELECT header FROM session").fetchone()[0])
        entries = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
    return header, entries


with tempfile.TemporaryDirectory(prefix="ion-subagent-resume-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    child_cwd = workspace / "child"
    child_cwd.mkdir(parents=True)
    env = {k: os.environ[k] for k in ("PATH", "SHELL") if k in os.environ}
    env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"), TMPDIR=str(root / "tmp"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
    Path(env["HOME"]).mkdir()
    Path(env["TMPDIR"]).mkdir()
    nonce = str(uuid.uuid4())
    marker = child_cwd / "AGENTS.md"
    marker.write_text("CURRENT_CHILD_RESOURCES_V1\n")
    program = workspace / "owned_process.py"
    program.write_text("""import os,sys,time
from pathlib import Path
phase=sys.argv[1]
Path(phase+'.pid').write_text(str(os.getpid()))
Path(phase+'.cwd').write_text(str(Path.cwd()))
if phase=='BASE': Path('nonce').write_text(sys.argv[2])
else: assert Path('nonce').read_text()==sys.argv[2]
Path(phase+'.ready').touch()
if phase=='FOLLOW2':
    deadline=time.monotonic()+20
    while not Path(__file__).with_name('FOLLOW2.release').exists():
        if time.monotonic()>deadline: raise RuntimeError('fixture not released')
        time.sleep(0.01)
Path(phase+'.effect').write_text(Path('nonce').read_text())
print('OBSERVED_NONCE='+Path('nonce').read_text())
""")
    failures, requests = [], []
    resumed_path = None

    def parent_for(prompt):
        for path in Path(env["ION_HOME"]).rglob("*.sqlite"):
            if path.parent.name == "children":
                continue
            if any(e["kind"] == "turn_started" and e["data"]["input"]["content"] == [{"Text": prompt}]
                   for e in facts(path)[1]):
                return path
        raise AssertionError("active parent not found")

    def first_code(parent):
        return """
const ids=[];
const start=async(task,extra={})=>{const r=await tools.subagent({action:'start',task,mode:'background',...extra}); if(!r.is_error)ids.push(r.value.id); return r;};
const wait=id=>tools.subagent({action:'wait',ids:[id],timeout_ms:10000});
try {
  const missing=await start('NOFILE',{resume:MISSING});
  if(!missing.is_error || missing.value.error.includes('unknown field'))return {unsupported:missing};
  const base=await start('BASE',{cwd:'child',model:'saved/saved'});
  if(base.is_error)return {unsupported:base};
  const [first]=(await wait(base.value.id)).value.runs;
  const invalid=[];
  invalid.push(await start('CONFLICT',{resume:first.path,context:{type:'fresh'},cwd:'child'}));
  invalid.push(await start('WRONG_CWD',{resume:first.path}));
  invalid.push(await start('SELF',{resume:PARENT}));
  const next=await start('FOLLOW2',{resume:first.path,cwd:'child'});
  if(next.is_error)return {unsupported:next};
  const ready=await tools.exec({command:READY,timeout_ms:10000});
  if(ready.is_error)return {unsupported:ready};
  const busy=await start('BUSY',{resume:first.path,cwd:'child'});
  const retained=await tools.subagent({action:'status',id:first.id});
  const oldWait=await wait(first.id);
  const oldCancel=await tools.subagent({action:'cancel',id:first.id});
  const stale=await tools.subagent({action:'steer',id:first.id,message:'must not reach new run'});
  const current=await tools.subagent({action:'status',id:next.value.id});
  const both=await tools.subagent({action:'list'});
  const ack=await tools.subagent({action:'acknowledge',id:first.id});
  const afterAck=await tools.subagent({action:'list'});
  const guide=await tools.subagent({action:'steer',id:next.value.id,message:GUIDE});
  const release=await tools.write({path:'FOLLOW2.release',content:'release'});
  const second=await wait(next.value.id);
  return {missing,first,invalid,busy,retained,oldWait,oldCancel,stale,current,both,ack,afterAck,guide,release,second};
} finally {
  for(const id of ids)await tools.subagent({action:'cancel',id});
  await tools.write({path:'FOLLOW2.release',content:'cleanup'});
}
""".replace("MISSING", json.dumps(str(root / "missing.sqlite"))).replace("PARENT", json.dumps(str(parent))).replace(
            "READY", json.dumps("while [ ! -f child/FOLLOW2.ready ]; do sleep 0.01; done")).replace("GUIDE", json.dumps(GUIDE))

    def tool(name, arguments):
        return {"tool_calls": [{"index": 0, "id": "fixture", "type": "function",
                                "function": {"name": name, "arguments": json.dumps(arguments)}}]}

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                users = [m["content"] for m in body["messages"] if m["role"] == "user"]
                task = users[0]
                phase = next((x for x in reversed(users) if x in ("BASE", "FOLLOW2", "FOLLOW3")), task)
                requests.append((phase, body))
                count = sum(p == phase for p, _ in requests)
                if task == "PARENT_ONE":
                    delta = tool("code_mode", {"code": first_code(parent_for(task))}) if count == 1 else {"content": "FIRST_PARENT_DONE"}
                elif task == "PARENT_TWO":
                    delta = tool("subagent", {"action": "start", "task": "FOLLOW3", "resume": str(resumed_path), "cwd": "child"}) if count == 1 else {"content": "SECOND_PARENT_DONE"}
                else:
                    assert task == "BASE", users
                    assert body["model"] == {"BASE": "saved", "FOLLOW2": "parent", "FOLLOW3": "fresh"}[phase], body["model"]
                    names = {t["function"]["name"] for t in body.get("tools", [])}
                    assert "subagent" not in names
                    systems = json.dumps([m for m in body["messages"] if m["role"] == "system"])
                    version = "V2" if phase == "FOLLOW3" else "V1"
                    assert "CURRENT_CHILD_RESOURCES_" + version in systems, systems
                    if phase != "BASE":
                        assert any(m["role"] == "assistant" and m.get("content") == "SAVED:" + nonce for m in body["messages"]), body["messages"]
                        assert nonce in json.dumps([m for m in body["messages"] if m["role"] == "tool"])
                    if phase == "FOLLOW3":
                        assert "write" not in names, "old conversation cannot restore an excluded executor"
                    if count == 1:
                        delta = tool("exec", {"command": "exec " + shlex.join([sys.executable, str(program), phase, nonce]), "timeout_ms": 20000})
                    elif phase == "FOLLOW2" and count == 2:
                        assert users == ["BASE", "FOLLOW2", GUIDE], users
                        delta = tool("write", {"path": "GUIDED", "content": nonce + "\n" + users[-1]})
                    else:
                        assert count == (3 if phase == "FOLLOW2" else 2), (phase, count)
                        delta = {"content": "SAVED:" + nonce if phase == "BASE" else phase + "_DONE"}
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
            return subprocess.run([BINARY, "--cwd", workspace, *arguments], env=env,
                                  capture_output=True, text=True, check=True, timeout=50)

        def configure(provider, model):
            subprocess.run([BINARY, "use", provider, model, "--wire", "chat-completions",
                            "--max-output-tokens", "256", "--endpoint", f"http://127.0.0.1:{server.server_port}/v1"],
                           env=env, capture_output=True, text=True, check=True, timeout=50)

        configure("saved", "saved")
        configure("smoke", "parent")
        output = run("--tools", "read,write,exec,subagent,code_mode", "--code-mode", "--json", "run", "PARENT_ONE")
        records = [json.loads(line) for line in output.stdout.splitlines()]
        completed = next(r for r in records if r["type"] == "tool_finished")
        assert "result" in completed["output"], completed
        result = completed["output"]["result"]
        assert "unsupported" not in result, result
        assert not failures, failures
        first = result["first"]
        [second] = result["second"]["value"]["runs"]
        assert first["id"] != second["id"] and first["child_session"] == second["child_session"]
        assert os.path.samefile(first["path"], second["path"])
        assert result["retained"]["value"] == result["oldWait"]["value"]["runs"][0] == result["ack"]["value"] == first
        assert result["oldCancel"]["value"]["stop_requested"] and result["stale"]["is_error"]
        assert not result["current"]["value"]["joined"]
        assert result["both"]["value"]["summary"] == {"running": 1, "joined": 1, "reserved": 0}
        assert result["afterAck"]["value"]["summary"] == {"running": 1, "joined": 0, "reserved": 0}
        assert all(r["is_error"] for r in result["invalid"]) and result["busy"]["is_error"]
        assert result["missing"]["is_error"] and not (root / "missing.sqlite").exists()
        assert result["guide"]["value"]["steering"]["status"]["state"] == "queued"
        assert not result["release"]["is_error"], result["release"]
        assert second["joined"], {"second":second,"release":result["release"],"requests":[p for p,_ in requests],"files":[str(p.relative_to(workspace)) for p in workspace.rglob('*')]}
        assert second["outcome"]["answer"] == "FOLLOW2_DONE" and second["outcome"]["receipt"]["turn"] == 2
        assert second["steering"][0]["status"]["state"] == "committed"
        assert (child_cwd / "GUIDED").read_text() == nonce + "\n" + GUIDE
        resumed_path = Path(second["path"])
        before_header, before_entries = facts(resumed_path)
        marker.write_text("CURRENT_CHILD_RESOURCES_V2\n")
        configure("fresh", "fresh")
        output = run("--tools", "read,exec,subagent", "--json", "run", "PARENT_TWO")
        records = [json.loads(line) for line in output.stdout.splitlines()]
        third = next(r["output"] for r in records if r["type"] == "tool_finished")
        assert third["child_session"] == first["child_session"] and len({first["id"], second["id"], third["id"]}) == 3
        assert third["outcome"]["answer"] == "FOLLOW3_DONE" and third["outcome"]["receipt"]["turn"] == 3
        assert not failures, failures
        header, entries = facts(resumed_path)
        assert header == before_header and entries[:len(before_entries)] == before_entries, "resume rewrote the conversation"
        links = [e["data"]["record"] for e in entries if e["kind"] == "child_session_of"]
        assert [r["run"] for r in links] == [first["id"], second["id"], third["id"]]
        assert links[0]["spec"]["authority"] != links[2]["spec"]["authority"], "old ancestry is not the new live authority"
        assert [r["spec"]["model"]["model"] for r in links] == ["saved", "parent", "fresh"]
        assert len([e for e in entries if e["kind"] == "turn_started"]) == 3
        admissions, outcomes = {}, {}
        for path in Path(env["ION_HOME"]).rglob("*.sqlite"):
            if path.parent.name == "children":
                continue
            parent_entries = facts(path)[1]
            inspected = json.loads(run("--session", str(path), "inspect").stdout)
            assert inspected["entries"] == parent_entries, "built inspection lost run/target facts"
            for entry in parent_entries:
                if entry["kind"] == "delegation_admitted":
                    admissions[entry["data"]["record"]["run"]] = entry["data"]["record"]
                elif entry["kind"] == "delegation_settled":
                    outcomes[entry["data"]["run"]] = entry["data"]["outcome"]
        assert set(admissions) == set(outcomes) == {first["id"], second["id"], third["id"]}
        for report in (first, second, third):
            assert outcomes[report["id"]] == report["outcome"]
        assert [sum(p == phase for p, _ in requests) for phase in ("PARENT_ONE", "BASE", "FOLLOW2", "PARENT_TWO", "FOLLOW3")] == [2, 2, 3, 2, 2]
        for phase in ("BASE", "FOLLOW2", "FOLLOW3"):
            assert (child_cwd / (phase + ".effect")).read_text() == nonce
            assert Path((child_cwd / (phase + ".cwd")).read_text()).samefile(child_cwd)
            pid = int((child_cwd / (phase + ".pid")).read_text())
            assert subprocess.run(["ps", "-p", str(pid)], capture_output=True, check=False).returncode != 0
        print("coding resume: same durable conversation, independent retained runs and stale controls, exclusive pre-admission refusal, fresh routes/resources/grants across restart; HTTP/native/facts verified: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
        for path in child_cwd.glob("*.pid"):
            pid = int(path.read_text())
            command = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True, check=False).stdout
            if str(program) in command:
                os.kill(pid, signal.SIGKILL)
