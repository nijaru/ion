"""Headless response settlement and stop signals under stdout/stderr pressure."""

import array
import fcntl
import json
import os
import select
import signal
import sqlite3
import subprocess
import tempfile
import termios
import threading
import time
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from itertools import product

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
refusal_requests = {}
refusal_text = "I cannot help with that 🦀."
optional_requests = []
optional_history = "h" * 36000


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        prompt = str(body["messages"][-1]["content"])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def event(delta, reason=None, usage=None):
            data = {"id": "headless", "choices": [{"index": 0, "delta": delta, "finish_reason": reason}]}
            if usage is not None:
                data["usage"] = usage
            self.wfile.write(b"data: " + json.dumps(data).encode() + b"\n\n")
            self.wfile.flush()

        try:
            group_prompt = next((str(message["content"]) for message in body["messages"] if message["role"] == "user" and "GROUP_TIMEOUT" in str(message["content"])), None)
            refusal_prompt = next((str(message["content"]) for message in reversed(body["messages"])
                                   if message["role"] == "user" and str(message["content"]).startswith("CHAT_REFUSAL_")), None)
            if body["model"] == "optional-model":
                optional_requests.append(body)
                if not body.get("tools"):
                    event({"content": "AUX_INCOMPLETE_NEVER_CONTEXT"}, "length",
                          {"prompt_tokens": 500, "completion_tokens": 100})
                elif prompt == "SEED_OPTIONAL":
                    event({"content": optional_history}, "stop")
                elif body["messages"][-1]["role"] == "tool":
                    event({"content": "CODING_COMPLETED"}, "stop")
                else:
                    event({"tool_calls": [{"index": 0, "id": "optional-effect", "type": "function", "function": {
                        "name": "exec", "arguments": json.dumps({"command": "printf 'ONCE\\n' >> effect.txt", "timeout_ms": 5000})}}]}, "tool_calls")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            elif refusal_prompt is not None:
                requests = refusal_requests.setdefault(refusal_prompt, [])
                requests.append(body)
                if "INTERRUPTED" in refusal_prompt and len(requests) == 1:
                    event({"refusal": "NO_RETRY 🦀"})
                    return  # EOF without finish_reason must not hide observed refusal output.
                if "CORRUPT" in refusal_prompt and body["messages"][-1]["role"] != "tool":
                    event({"tool_calls": [{"index": 0, "id": "refusal-call", "type": "function", "function": {
                        "name": "exec", "arguments": json.dumps({"command": "printf ONCE > refusal-effect", "timeout_ms": 5000})}}]})
                    event({"refusal": 42}, "tool_calls")
                elif "COMPLETE" in refusal_prompt:
                    event({"content": None, "refusal": "I cannot "})
                    event({"refusal": "help with that 🦀."}, "stop")
                    event({"content": None, "refusal": ""}, "stop")
                else:
                    event({"content": "FOLLOWUP_READY"}, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            elif group_prompt is not None:
                if body["messages"][-1]["role"] == "tool":
                    event({"content": "Group timeout observed."}, "stop")
                else:
                    direct_exits = "DIRECT_EXIT" in group_prompt
                    command = (
                        "printf %s $$ > native.pid; "
                        "sh -c 'trap \"\" TERM PIPE; printf %s $$ > descendant.pid; touch native.ready; while :; do printf x; done' & "
                        "while [ ! -e native.ready ]; do sleep 0.01; done; "
                        + ("exit 0" if direct_exits else "exec sleep 30")
                    )
                    event({"tool_calls": [{"index": 0, "id": "group", "type": "function", "function": {"name": "exec", "arguments": json.dumps({"command": command, "timeout_ms": 1000})}}]}, "tool_calls")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            elif "BURST" in prompt:
                deltas = [{"content": "x"}] * 5000 + [{}]
                records = [b"data: " + json.dumps({"id": "burst", "choices": [{"index": 0, "delta": delta, "finish_reason": "stop" if index == 5000 else None}]}).encode() + b"\n\n" for index, delta in enumerate(deltas)]
                self.wfile.write(b"".join(records) + b"data: [DONE]\n\n")
                self.wfile.flush()
            elif "TEXT_PRESSURE" in prompt:
                commands = [
                    "printf 'ONCE\\n' >> effects; python3 -c \"print('x' * 100000)\"",
                    "printf '%s\\n' $$ > native.pid; sh -c 'trap \"\" TERM; printf %s $$ > descendant.pid; touch native.ready; exec sleep 30' >/dev/null 2>&1 & exec sleep 30",
                    "touch unused",
                ]
                calls = [{"index": index, "id": f"call-{index}", "type": "function", "function": {"name": "exec", "arguments": json.dumps({"command": command, "timeout_ms": 120000})}} for index, command in enumerate(commands)]
                event({"tool_calls": calls}, "tool_calls")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            else:
                # Small valid model deltas create enough JSONL framing to fill
                # a pipe without relying on an oversized model answer.
                for _ in range(5000):
                    event({"content": "x"})
                    time.sleep(0.001)
                event({}, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_args):
        pass


def available(pipe):
    count = array.array("i", [0])
    fcntl.ioctl(pipe.fileno(), termios.FIONREAD, count, True)
    return count[0]


def entries(database):
    with closing(sqlite3.connect(f"file:{database}?mode=ro", uri=True)) as connection:
        return [json.loads(row[0]) for row in connection.execute("SELECT body FROM entries ORDER BY seq")]


def session_database(state, workspace):
    matches = []
    for path in state.rglob("*.sqlite"):
        with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as connection:
            header = json.loads(connection.execute("SELECT header FROM session WHERE id=1").fetchone()[0])
        if Path(header["cwd"]).resolve() == workspace.resolve():
            matches.append(path)
    assert len(matches) == 1, (workspace, matches)
    return matches[0]


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def running(pid):
    # Linux orphan zombies remain visible to kill(pid, 0), but cannot execute.
    result = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
    assert result.returncode in (0, 1) and not result.stderr, result
    state = result.stdout.strip()
    return bool(state) and not state.startswith("Z")


def wait_stopped(pid):
    # Group SIGKILL precedes root release, but grandchildren are not waitable.
    deadline = time.monotonic() + 2
    while running(pid):
        assert time.monotonic() < deadline, f"same-group descendant {pid} survived stop"
        time.sleep(0.01)


def cleanup(child, workspace):
    if child.poll() is None:
        child.kill()
        child.communicate(timeout=8)
    for name in ("native.pid", "descendant.pid"):
        pid_path = workspace / name
        if pid_path.exists():
            pid = int(pid_path.read_text())
            if running(pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


def exercise_refusals(work, env):
    # An interrupted readable refusal is observed output, not a pre-output
    # transport failure eligible for an automatic replacement request.
    workspace = work / "refusal-interrupted"
    workspace.mkdir()
    prompt = "CHAT_REFUSAL_INTERRUPTED"
    child = subprocess.run([binary, "--cwd", workspace, "--json", "run", prompt], env=env,
                           stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15)
    records = [json.loads(line) for line in child.stdout.splitlines()]
    assert child.returncode == 1 and len(refusal_requests[prompt]) == 1, (child.returncode, len(refusal_requests[prompt]), child.stdout, child.stderr)
    assert [record for record in records if record["type"] == "text_delta"] == [
        dict(type="text_delta", block=0, text="NO_RETRY 🦀")], records
    facts = entries(session_database(work / "ion", workspace))
    assert not any(fact["kind"] in ("assistant", "tool_result") for fact in facts), facts
    assert facts[-1]["kind"] == "turn_ended" and "failed" in facts[-1]["data"]["reason"], facts[-1]

    # Even an already complete provisional call cannot execute if a later
    # known semantic field is invalid in the same response.
    workspace = work / "refusal-corrupt"
    workspace.mkdir()
    prompt = "CHAT_REFUSAL_CORRUPT"
    child = subprocess.run([binary, "--cwd", workspace, "--json", "run", prompt], env=env,
                           stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15)
    assert child.returncode == 1 and len(refusal_requests[prompt]) == 1, (child.returncode, child.stdout, child.stderr)
    assert not (workspace / "refusal-effect").exists(), "invalid response admitted a provisional call"
    facts = entries(session_database(work / "ion", workspace))
    assert not any(fact["kind"] in ("assistant", "tool_result") for fact in facts), facts
    assert facts[-1]["kind"] == "turn_ended" and "failed" in facts[-1]["data"]["reason"], facts[-1]

    for json_output in (True, False):
        workspace = work / f"refusal-complete-{json_output}"
        workspace.mkdir()
        prompt = f"CHAT_REFUSAL_COMPLETE_{json_output}"
        command = [binary, "--cwd", workspace] + (["--json"] if json_output else [])
        child = subprocess.run(command + ["run", prompt], env=env, stdin=subprocess.DEVNULL,
                               capture_output=True, text=True, timeout=15)
        assert child.returncode == 0 and len(refusal_requests[prompt]) == 1, (child.returncode, child.stdout, child.stderr)
        if json_output:
            records = [json.loads(line) for line in child.stdout.splitlines()]
            assert [record for record in records if record["type"] == "text_delta"] == [
                dict(type="text_delta", block=0, text="I cannot "),
                dict(type="text_delta", block=0, text="help with that 🦀.")], records
            assert next(record["text"] for record in records if record["type"] == "final") == refusal_text, records
            assert records[-1] == dict(type="run_end", status="completed"), records[-1]
        else:
            assert child.stdout == refusal_text + "\n", (child.stdout, child.stderr)
        database = session_database(work / "ion", workspace)
        snapshot = entries(database)
        assert [fact["data"]["message"]["content"] for fact in snapshot if fact["kind"] == "assistant"] == [[{"Text": refusal_text}]], snapshot
        assert not any(fact["kind"] == "tool_result" for fact in snapshot), snapshot
        inspection = subprocess.run([binary, "--session", database, "inspect"], env=env,
                                    stdin=subprocess.DEVNULL, capture_output=True, check=True, text=True, timeout=15)
        assert json.loads(inspection.stdout)["entries"] == snapshot
        followup = f"CHAT_REFUSAL_REOPEN_{json_output}"
        reopened = subprocess.run([binary, "--cwd", workspace, "--session", database, "run", followup], env=env,
                                  stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15)
        assert reopened.returncode == 0 and reopened.stdout == "FOLLOWUP_READY\n", (reopened.stdout, reopened.stderr)
        assert len(refusal_requests[followup]) == 1, refusal_requests[followup]
        assistant = [message for message in refusal_requests[followup][0]["messages"] if message["role"] == "assistant"]
        assert len(assistant) == 1 and assistant[0]["content"] == refusal_text, assistant
        facts = entries(database)
        assert facts[:len(snapshot)] == snapshot and sum(fact["kind"] == "turn_started" for fact in facts) == 2, facts
        assert not any(fact["kind"] == "tool_result" for fact in facts), facts
    print("Ion headless refusal output, continuation, and response admission: OK")


def exercise_optional_compaction(work, endpoint):
    for json_output in (True, False):
        sandbox = work / f"optional-{json_output}"
        workspace = sandbox / "workspace"
        workspace.mkdir(parents=True)
        (workspace / ".git").mkdir()  # Bound instruction discovery to this fixture.
        env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
        env.update(HOME=str(sandbox / "home"), ION_HOME=str(sandbox / "ion"),
                   XDG_CONFIG_HOME=str(sandbox / "config"), XDG_STATE_HOME=str(sandbox / "state"))
        # The retained history fits with a positive, clamped output budget, but
        # consumes more than half the full coding application's available room.
        subprocess.run([binary, "use", "optional-fixture", "optional-model", "--endpoint", endpoint,
                        "--wire", "chat-completions", "--context-window", "28000", "--max-output-tokens", "16000"],
                       env=env, capture_output=True, check=True)
        optional_requests.clear()
        seed = subprocess.run([binary, "--cwd", workspace, "run", "SEED_OPTIONAL"], env=env,
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15)
        assert seed.returncode == 0 and seed.stdout == optional_history + "\n", (seed.returncode, seed.stderr)
        database = session_database(sandbox / "ion", workspace)
        before = entries(database)
        command = [binary, "--cwd", workspace, "--session", database] + (["--json"] if json_output else [])
        child = subprocess.run(command + ["run", "FINISH_OPTIONAL"], env=env,
                               stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15)
        assert child.returncode == 0, (child.returncode, child.stdout[-3000:], child.stderr)
        assert (workspace / "effect.txt").read_text() == "ONCE\n", "coding effect was omitted or replayed"
        if json_output:
            records = [json.loads(line) for line in child.stdout.splitlines()]
            skips = [record for record in records if record["type"] == "context_compaction_skipped"]
            assert skips == [dict(type="context_compaction_skipped", reason="invalid summary: model output limit reached")] * 2, skips
            assert records[-1] == dict(type="run_end", status="completed"), records[-1]
            assert next(record["text"] for record in records if record["type"] == "final") == "CODING_COMPLETED"
        else:
            assert child.stdout == "CODING_COMPLETED\n", child.stdout
            assert child.stderr.count("Auto-compaction skipped:") == 2, child.stderr
            assert child.stderr.count("existing context retained") == 2, child.stderr
        assert len(optional_requests) == 5, "one optional attempt per model/tool step; no hidden retries"
        for request in (optional_requests[1], optional_requests[3]):
            assert not request.get("tools"), "auxiliary request advertised effect authority"
        for request in (optional_requests[2], optional_requests[4]):
            assert request.get("tools") and 0 < request["max_completion_tokens"] < 16000, request.keys()
            assert any(message["role"] == "assistant" and message["content"] == optional_history for message in request["messages"])
            assert "AUX_INCOMPLETE_NEVER_CONTEXT" not in json.dumps(request["messages"])
        facts = entries(database)
        assert facts[:len(before)] == before, "optional failure rewrote existing history"
        assert not any(fact["kind"] == "compacted" for fact in facts), "rejected summary committed a cut"
        assert sum(fact["kind"] == "tool_result" for fact in facts) == 1, "auxiliary call gained execution authority"
        usage = [fact["data"]["usage"] for fact in facts if fact["kind"] == "uncommitted_usage"]
        assert len(usage) == 2 and sum(item["output_tokens"] for item in usage) == 200, usage
        assert facts[-1]["kind"] == "turn_ended" and facts[-1]["data"]["reason"] == "completed", facts[-1]
        reopened = subprocess.run([binary, "--session", database, "inspect"], env=env,
                                  stdin=subprocess.DEVNULL, capture_output=True, check=True, text=True, timeout=15)
        assert json.loads(reopened.stdout)["entries"] == facts and entries(database) == facts
        assert len(optional_requests) == 5, "passive reopening dispatched model work"
    print("Ion optional compaction failure, clamped coding effects, accounting and reopening: OK")


with tempfile.TemporaryDirectory(prefix="ion-headless-") as temporary:
    work = Path(temporary)
    env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
    env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "headless-smoke", "model", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        exercise_optional_compaction(work, endpoint)
        exercise_refusals(work, env)
        burst_workspace = work / "burst"
        burst_workspace.mkdir()
        burst = subprocess.run([binary, "--cwd", burst_workspace, "--json", "run", "BURST"], env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=15)
        assert burst.returncode == 0, burst.stderr
        burst_records = [json.loads(line) for line in burst.stdout.splitlines()]
        assert burst_records[-1] == {"type": "run_end", "status": "completed"}, burst_records[-3:]
        visible = "".join(record["text"] for record in burst_records if record["type"] == "text_delta")
        gaps = [record for record in burst_records if record["type"] == "observation_gap"]
        # Provisional progress is bounded and may overflow even with a draining
        # reader if scheduling lets a burst get ahead. Loss must be explicit;
        # the committed final below must still be complete and exact.
        assert len(visible) <= 5000 and visible == "x" * len(visible), (len(visible), gaps)
        if len(visible) < 5000:
            assert gaps and sum(record["skipped"] for record in gaps) > 0, (len(visible), gaps)
        assert next(record["text"] for record in burst_records if record["type"] == "final") == "x" * 5000
        for direct_exits in (False, True):
            workspace = work / f"group-timeout-{direct_exits}"
            workspace.mkdir()
            child = subprocess.Popen([binary, "--cwd", workspace, "--json", "run", "GROUP_TIMEOUT" + (" DIRECT_EXIT" if direct_exits else "")], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                stdout, stderr = child.communicate(timeout=10)
                assert child.returncode == 0, (stdout, stderr)
                database = session_database(work / "ion", workspace)
                facts = entries(database)
                results = [fact["data"]["result"]["outcome"] for fact in facts if fact["kind"] == "tool_result"]
                assert len(results) == 1 and results[0]["state"] == "observed", results
                output = results[0]["output"]["value"]
                assert output["timed_out"] and not output["cancelled"] and output["wait_error"] is None, output
                assert output["exit_code"] == (0 if direct_exits else None), output
                assert output["signal"] == (None if direct_exits else signal.SIGTERM), output
                assert facts[-1]["kind"] == "turn_ended" and facts[-1]["data"]["reason"] == "completed", facts[-1]
                descendant = int((workspace / "descendant.pid").read_text())
                wait_stopped(descendant)
                assert not alive(int((workspace / "native.pid").read_text())), "direct child was not reaped"
                # A fresh reader must recover exactly the same committed outcome.
                reopened = json.loads(subprocess.run([binary, "--cwd", workspace, "--session", database, "inspect"], env=env, capture_output=True, check=True).stdout)
                assert reopened["entries"] == facts, "read-only reopen changed the timeout outcome"
            finally:
                cleanup(child, workspace)
        for json_output, stop_signal in product((True, False), (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)):
            workspace = work / (("json" if json_output else "text") + f"-{stop_signal}")
            workspace.mkdir()
            command = [binary, "--cwd", workspace]
            if json_output:
                command.append("--json")
            child = subprocess.Popen(command + ["run", "JSON_PRESSURE" if json_output else "TEXT_PRESSURE"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            native_pid = None
            writer_probe = None
            try:
                pipe = child.stdout if json_output else child.stderr
                if hasattr(fcntl, "F_SETPIPE_SZ"):
                    # Change only this owned fixture pipe, not production limits.
                    fcntl.fcntl(pipe.fileno(), fcntl.F_SETPIPE_SZ, 4096)
                    # Linux pipe page/atomic-write rules can stall a writer well
                    # below byte capacity. Probe kernel writability, without
                    # writing data or consuming the recipient's buffered output.
                    descriptor = 1 if json_output else 2
                    writer_probe = os.open(f"/proc/{child.pid}/fd/{descriptor}", os.O_WRONLY | os.O_NONBLOCK | os.O_CLOEXEC)
                deadline = time.monotonic() + 8
                previous = -1
                unchanged_since = time.monotonic()
                while time.monotonic() < deadline:
                    assert child.poll() is None, "headless exited before output pressure"
                    count = available(pipe)
                    if count != previous:
                        previous = count
                        unchanged_since = time.monotonic()
                    blocked = writer_probe is None or not select.select([], [writer_probe], [], 0)[1]
                    # Plaintext stderr can fill with call-admission activity
                    # before either command starts. This case promises native
                    # cancellation, so establish the host effect and running
                    # second command before sending SIGINT, not just pipe load.
                    native_started = json_output or (workspace / "native.ready").exists()
                    if count > 0 and blocked and native_started and time.monotonic() - unchanged_since >= 0.2:
                        break
                    time.sleep(0.01)
                else:
                    raise AssertionError(f"fixture did not establish output pressure and required native work: json={json_output}, available={previous}, native_ready={(workspace / 'native.ready').exists()}")
                if not json_output:
                    # A prior cancelled Session must not become this test's
                    # oracle because a checkpoint/touch changed its file mtime.
                    previous = session_database(work / "ion", work / f"json-{stop_signal}")
                    stamp = time.time_ns() + 10_000_000_000
                    os.utime(previous, ns=(stamp, stamp))
                database = session_database(work / "ion", workspace)
                child.send_signal(stop_signal)
                deadline = time.monotonic() + 4
                facts = []
                while time.monotonic() < deadline:
                    facts = entries(database)
                    if facts and facts[-1]["kind"] == "turn_ended":
                        break
                    time.sleep(0.02)
                assert facts[-1]["kind"] == "turn_ended" and facts[-1]["data"]["reason"] == "cancelled", "output pressure prevented durable headless cancellation"
                if (workspace / "native.pid").exists():
                    native_pid = int((workspace / "native.pid").read_text())
                    assert not alive(native_pid), "headless settled before reaping owned native work"
                    descendant = int((workspace / "descendant.pid").read_text())
                    wait_stopped(descendant)
                if writer_probe is not None:
                    os.close(writer_probe)
                    writer_probe = None
                stdout, stderr = child.communicate(timeout=8)
                assert child.returncode == 1, (stdout, stderr)
                if json_output:
                    records = [json.loads(line) for line in stdout.splitlines()]
                    assert records[0]["type"] == "session", records
                    assert records[-1]["type"] == "run_end" and records[-1]["status"] == "cancelled", records[-3:]
                    assert sum(record["type"] == "turn_ended" for record in records) == 1
                else:
                    assert stdout == b"", "headless published an uncommitted final answer"
                    assert (workspace / "effects").read_text() == "ONCE\n"
                    assert not (workspace / "unused").exists()
                    outcomes = [fact["data"]["result"]["outcome"]["state"] for fact in facts if fact["kind"] == "tool_result"]
                    assert outcomes[0] == "observed" and outcomes[-1] == "not_dispatched", outcomes
            finally:
                if writer_probe is not None:
                    os.close(writer_probe)
                cleanup(child, workspace)
        # Plain passive inspection never acquired a startup terminal and must
        # retain default TERM handling, even with its output pipe left unread.
        workspace = work / f"text-{signal.SIGTERM}"
        database = session_database(work / "ion", workspace)
        snapshot = entries(database)
        child = subprocess.Popen([binary, "--session", database, "inspect"], env=env,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            if hasattr(fcntl, "F_SETPIPE_SZ"):
                fcntl.fcntl(child.stdout.fileno(), fcntl.F_SETPIPE_SZ, 4096)
            deadline = time.monotonic() + 5
            previous, unchanged_since = -1, time.monotonic()
            while time.monotonic() < deadline:
                assert child.poll() is None, "inspection exited before owned output pressure"
                count = available(child.stdout)
                if count != previous:
                    previous, unchanged_since = count, time.monotonic()
                if count >= 4096 and time.monotonic() - unchanged_since >= 0.2:
                    break
                time.sleep(0.01)
            else:
                raise AssertionError("passive inspection did not establish output pressure")
            child.send_signal(signal.SIGTERM)
            stdout, stderr = child.communicate(timeout=5)
            assert child.returncode == -signal.SIGTERM, (child.returncode, stdout, stderr)
            assert entries(database) == snapshot, "passive termination changed recorded work"
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)
        for json_output in (True, False):
            workspace = work / ("broken-json" if json_output else "broken-text")
            workspace.mkdir()
            command = [binary, "--cwd", workspace]
            if json_output:
                command.append("--json")
            child = subprocess.Popen(command + ["run", "TEXT_PRESSURE"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + 8
                while not (workspace / "native.ready").exists() and time.monotonic() < deadline:
                    assert child.poll() is None, "headless exited before native work started"
                    time.sleep(0.01)
                assert (workspace / "native.ready").exists(), "native fixture did not start"
                native_pid = int((workspace / "native.pid").read_text())
                if json_output:
                    child.stdout.close()
                    child.stdout = None
                else:
                    child.stderr.close()
                    child.stderr = None
                stdout, stderr = child.communicate(timeout=8)
                assert child.returncode == 1, (child.returncode, stdout, stderr)
                assert not alive(native_pid), "broken output dropped owned native work"
                database = session_database(work / "ion", workspace)
                facts = entries(database)
                assert facts[-1]["kind"] == "turn_ended" and facts[-1]["data"]["reason"] == "cancelled", facts[-1]
                outcomes = [fact["data"]["result"]["outcome"] for fact in facts if fact["kind"] == "tool_result"]
                assert [outcome["state"] for outcome in outcomes] == ["observed", "observed", "not_dispatched"], outcomes
                assert outcomes[1]["output"]["value"]["cancelled"] is True, outcomes[1]
                assert (workspace / "effects").read_text() == "ONCE\n" and not (workspace / "unused").exists()
            finally:
                cleanup(child, workspace)
    finally:
        server.shutdown()
        server.server_close()
print("Ion headless backpressure cancellation and durable settlement: OK")
