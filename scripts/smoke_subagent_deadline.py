"""Offline built-client oracle for child execution deadlines and joined effects."""
import json
import os
import shlex
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
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


with tempfile.TemporaryDirectory(prefix="ion-subagent-deadline-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"), TMPDIR=str(root / "tmp"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"))
    Path(env["HOME"]).mkdir()
    Path(env["TMPDIR"]).mkdir()
    program = workspace / "owned_process.py"
    program.write_text('''import os, subprocess, sys, time
from pathlib import Path
if sys.argv[1] == 'descendant':
    time.sleep(60)
elif sys.argv[1] == 'check':
    for suffix in ('.pid', '.descendant'):
        pid = int(Path('EXPIRE'+suffix).read_text())
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError('deadline report preceded native joining')
        os.kill(int(Path('SURVIVE'+suffix).read_text()), 0)
    Path('JOIN_OBSERVED').touch()
else:
    name = sys.argv[1]
    Path(name+'.pid').write_text(str(os.getpid()))
    child = subprocess.Popen([sys.executable, __file__, 'descendant', name])
    Path(name+'.descendant').write_text(str(child.pid))
    Path(name+'.before').write_text('BEFORE_STOP')
    Path(name+'.ready').touch()
    while not Path(name+'.release').exists():
        time.sleep(0.01)
    child.terminate()
    child.wait(timeout=5)
    Path(name+'.after').write_text('AFTER_RELEASE')
''')
    code = """
const start = (task, options={}) => tools.subagent({action:'start',task,...options});
const release = task => tools.write({path:task+'.release',content:'release'});
try {
  const invalid = [];
  for (const execution_timeout_ms of [0,7200001,null])
    invalid.push(await start('INVALID',{execution_timeout_ms}));
  const [expired, survivor] = await Promise.all([
    start('EXPIRE',{mode:'background',execution_timeout_ms:3000}),
    start('SURVIVE',{mode:'background'})
  ]);
  if (expired.is_error || survivor.is_error) return {unsupported:[expired,survivor]};
  const ids = [expired.value.id,survivor.value.id];
  await tools.exec({command:'while [ ! -f EXPIRE.ready ] || [ ! -f SURVIVE.ready ]; do sleep 0.01; done',timeout_ms:10000});
  const brief = await tools.subagent({action:'wait',ids:ids,timeout_ms:10});
  const first = await tools.subagent({action:'wait',ids:ids,timeout_ms:10000});
  const retained = await tools.subagent({action:'status',id:ids[0]});
  const checked = await tools.exec({command:CHECK_COMMAND,timeout_ms:10000});
  await release('SURVIVE');
  const survived = await tools.subagent({action:'wait',ids:[ids[1]],timeout_ms:10000});
  const cancelled = await start('CANCEL',{mode:'background',execution_timeout_ms:7200000});
  await tools.exec({command:'while [ ! -f CANCEL.ready ]; do sleep 0.01; done',timeout_ms:10000});
  await tools.subagent({action:'cancel',id:cancelled.value.id});
  const stopped = await tools.subagent({action:'wait',ids:[cancelled.value.id],timeout_ms:10000});
  const foreground = await start('FOREGROUND',{execution_timeout_ms:3000});
  const stream = await start('STREAM',{mode:'background',execution_timeout_ms:3000});
  let live;
  for (let attempt=0; attempt<12; attempt++) {
    const view = await tools.subagent({action:'status',id:stream.value.id});
    if (view.value.progress?.provisional_output?.truncated) { live=view.value; break; }
    await tools.subagent({action:'wait',ids:[stream.value.id],timeout_ms:100});
  }
  if (!live) return {missing_live_progress:true};
  const [silent] = (await tools.subagent({action:'wait',ids:[stream.value.id],timeout_ms:30})).value.runs;
  // Check the entire actual preview without multiplying large copies in this
  // parent's own returned test context.
  const preview_exact = live.progress.provisional_output.text === '🚀'.repeat(12800);
  const compact = value => ({...value,progress:{...value.progress,
    provisional_output:{...value.progress.provisional_output,text:'(checked separately)'}}});
  const streaming = await tools.subagent({action:'wait',ids:[stream.value.id],timeout_ms:10000});
  return {invalid,expired,survivor,brief,first,retained,checked,survived,stopped,foreground,
    streaming:{is_error:streaming.is_error,value:streaming.value.runs[0]},
    live:compact(live),silent:compact(silent),preview_exact};
} finally {
  await Promise.all(['EXPIRE','SURVIVE','CANCEL','FOREGROUND'].map(release));
}
"""
    code = code.replace('CHECK_COMMAND', json.dumps(shlex.join([sys.executable, str(program), 'check'])))
    failures, requests, schemas = [], [], []
    stream_release = threading.Event()

    def tool(name, arguments):
        return {"tool_calls": [{"index": 0, "id": "fixture", "type": "function",
                                "function": {"name": name, "arguments": json.dumps(arguments)}}]}

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                # Automated readiness does not start a new human task.
                task_index, user = next(
                    (index, message["content"])
                    for index, message in reversed(list(enumerate(body["messages"])))
                    if message["role"] == "user"
                    and not message["content"].startswith("Automated managed-work readiness (not a user instruction).")
                )
                fresh = not any(message["role"] == "assistant" for message in body["messages"][task_index + 1:])
                requests.append(user)
                if user == "PARENT":
                    if fresh:
                        schemas.append(next(item["function"]["parameters"] for item in body["tools"]
                                            if item["function"]["name"] == "subagent"))
                    delta = tool("code_mode", {"code": code}) if fresh else {"content": "PARENT_DEADLINE_DONE"}
                else:
                    assert user in ("EXPIRE", "SURVIVE", "CANCEL", "FOREGROUND", "STREAM"), user
                    if user == 'STREAM':
                        assert fresh
                        self.send_response(200)
                        self.send_header('Content-Type', 'text/event-stream')
                        self.send_header('Connection', 'close')
                        self.end_headers()
                        partial = {'choices':[{'index':0,'delta':{'content':'🚀'*13000},'finish_reason':None}]}
                        self.wfile.write(b'data: '+json.dumps(partial).encode()+b'\n\n')
                        self.wfile.flush()
                        stream_release.wait(20)
                        return
                    assert "subagent" not in {item["function"]["name"] for item in body.get("tools", [])}
                    if fresh:
                        delta = tool("exec", {"command": "exec " + shlex.join([sys.executable, str(program), user]),
                                              "timeout_ms": 120000})
                        delta['content'] = 'VISIBLE_'+user
                    else:
                        assert user == "SURVIVE", f"stopped child dispatched more model work: {user}"
                        delta = {"content": "SURVIVOR_ANSWER"}
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
                                  text=True, check=True, timeout=35)

        subprocess.run([BINARY, "use", "smoke", "deadline", "--wire", "chat-completions",
                        "--max-output-tokens", "256", "--endpoint", f"http://127.0.0.1:{server.server_port}/v1"],
                       env=env, capture_output=True, text=True, check=True, timeout=35)
        process = run("--tools", "read,write,exec,subagent,code_mode", "--code-mode", "--json", "run", "PARENT")
        records = [json.loads(line) for line in process.stdout.splitlines()]
        assert not failures, failures
        result = next(record["output"]["result"] for record in records if record["type"] == "tool_finished")
        assert "first" in result, result
        assert all(item["is_error"] for item in result["invalid"]), result["invalid"]
        assert result["expired"]["value"]["execution_timeout_ms"] == 3000
        assert result["survivor"]["value"]["execution_timeout_ms"] == 1800000
        assert result["brief"]["value"]["wait_expired"] and not result["brief"]["is_error"]
        assert result["first"]["is_error"] and not result["first"]["value"]["wait_expired"]
        timed_out, running = result["first"]["value"]["runs"]
        assert timed_out["joined"] and not running["joined"], result["first"]
        assert result["retained"]["value"] == timed_out and result["retained"]["is_error"]
        assert not result["checked"]["is_error"] and (workspace / 'JOIN_OBSERVED').exists(), result["checked"]
        [survived] = result["survived"]["value"]["runs"]
        assert not result["survived"]["is_error"] and survived["outcome"]["answer"] == "SURVIVOR_ANSWER"
        [cancelled] = result["stopped"]["value"]["runs"]
        foreground = result["foreground"]["value"]
        for receipt, expired in [(timed_out, True), (cancelled, False), (foreground, True)]:
            assert receipt["joined"] and receipt["outcome"]["kind"] == "joined", receipt
            outcome = receipt["outcome"]
            assert outcome["execution_timed_out"] == expired and outcome["answer"] is None, receipt
            assert outcome["receipt"]["end"] == "cancelled", receipt
        assert result["stopped"]["is_error"] and result["foreground"]["is_error"]
        assert not survived["outcome"]["execution_timed_out"]
        assert survived['outcome']['partial_output'] is None
        assert isinstance(survived['outcome']['elapsed_ms'], int)
        for report, name in [(timed_out, 'EXPIRE'), (cancelled, 'CANCEL'), (foreground, 'FOREGROUND')]:
            observation = report['outcome']['partial_output']
            assert observation == {'text':'VISIBLE_'+name,'truncated':False,'observation_gap':False}, report
            elapsed = report['outcome']['elapsed_ms']
            assert isinstance(elapsed, int) and elapsed >= 0, report
            if report['outcome']['execution_timed_out']:
                assert elapsed >= 3000, report
                assert f'after {elapsed} ms (budget 3000 ms)' in report['outcome']['error'], report
        streaming = result['streaming']['value']
        assert result['streaming']['is_error'] and streaming['joined']
        streamed = streaming['outcome']
        assert streamed['execution_timed_out'] and streamed['answer'] is None, {key:value for key,value in streamed.items() if key != 'partial_output'}
        assert streamed['elapsed_ms'] >= 3000
        assert streamed['partial_output'] == {'text':'🚀'*12800,'truncated':True,'observation_gap':False}
        assert requests.count('STREAM') == 1, requests
        live, silent = result['live'], result['silent']
        assert not live['joined'] and live['outcome'] is None
        observed = live['progress']
        assert observed['child_session'] == live['child_session']
        assert observed['request'] == {'scope':'ion.delegation','id':live['id']}
        assert observed['activity'] == 'text' and observed['current_tool'] is None
        assert observed['last_activity_at_ms'] > 0 and not observed['stopping']
        assert result['preview_exact'] and observed['provisional_output']['truncated']
        assert not silent['joined'] and silent['outcome'] is None
        assert silent['progress']['last_activity_at_ms'] == observed['last_activity_at_ms']
        assert silent['progress']['elapsed_ms'] > observed['elapsed_ms']
        assert streaming['progress']['stopping']
        assert streaming['progress']['elapsed_ms'] == streamed['elapsed_ms']
        assert streaming['progress']['provisional_output'] == streamed['partial_output']
        # Waiting for the first timeout never turns the sibling tool's activity
        # into a heartbeat, answer, or completed effect.
        assert running['progress']['activity'] == 'tool'
        assert running['progress']['current_tool'] == 'exec'
        assert running['progress']['provisional_output']['text'] == 'VISIBLE_SURVIVE'
        assert requests.count("SURVIVE") == 2 and requests.count("PARENT") == 2, requests
        for name in ("EXPIRE", "CANCEL", "FOREGROUND"):
            assert requests.count(name) == 1, requests
        for name in ("EXPIRE", "SURVIVE", "CANCEL", "FOREGROUND"):
            assert (workspace / (name + ".before")).read_text() == "BEFORE_STOP"
            for suffix in (".pid", ".descendant"):
                assert dead(int((workspace / (name + suffix)).read_text())), f"{name}{suffix} survived joining"
            assert (workspace / (name + ".after")).exists() == (name == "SURVIVE")
        start_schema = next(choice for choice in schemas[0]["oneOf"] if choice["properties"]["action"]["const"] == "start")
        budget = start_schema["properties"]["execution_timeout_ms"]
        assert (budget["minimum"], budget["maximum"], budget["default"]) == (1, 7200000, 1800000)
        parent = next(path for path in Path(env["ION_HOME"]).rglob("*.sqlite") if path.parent.name != "children")
        with closing(sqlite3.connect(f"file:{parent}?mode=ro", uri=True)) as db:
            entries = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
        inspected = json.loads(run('--session', str(parent), 'inspect').stdout)
        assert inspected['entries'] == entries, 'CLI inspection dropped deadline facts'
        admissions = [entry["data"]["record"] for entry in entries if entry["kind"] == "delegation_admitted"]
        outcomes = {entry["data"]["run"]: entry["data"]["outcome"] for entry in entries if entry["kind"] == "delegation_settled"}
        assert len(admissions) == len(outcomes) == 5, (admissions, outcomes)
        for receipt in (timed_out, survived, cancelled, foreground, streaming):
            assert outcomes[receipt["id"]] == receipt["outcome"]
        print("coding deadlines: foreground/background native joining, independent expiry/cancellation; bounded live progress/quiet timestamps and committed/provisional reports, elapsed runtime and typed facts retained: OK")
    finally:
        stream_release.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
        # Failed probes must not leave owned native effects behind. Guard PID
        # cleanup against reuse by verifying the unique fixture program path.
        for pattern in ("*.pid", "*.descendant"):
            for file in workspace.glob(pattern):
                pid = int(file.read_text())
                if not dead(pid):
                    command = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True, check=False).stdout
                    if str(program) in command:
                        try:
                            os.kill(pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
