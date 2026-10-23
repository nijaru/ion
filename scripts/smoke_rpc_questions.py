"""Exercise same-Turn RPC questions, reply pressure and foreground owner loss."""

import fcntl
import json
import os
import select
import subprocess
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
questions = [
    {"question": "Which route?", "options": [{"label": "Alpha"}, {"label": "Beta", "description": "second"}]},
    {"question": "Which detail?", "options": [{"label": "One"}, {"label": "Two"}]},
    {"question": "Which features?", "multi_select": True, "options": [{"label": "Read"}, {"label": "Write"}, {"label": "Search"}]},
    {"question": "Anything else?", "multi_select": True, "options": [{"label": "One"}, {"label": "Two"}]},
]
answers = [{"kind": "option", "index": 1}, {"kind": "text", "text": "  café 🦀\n  "}, {"kind": "options", "indices": [2, 0]}, {"kind": "options", "indices": []}]
expected_answers = [
    {"kind": "option", "question": 1, "option": 1, "label": "Beta"},
    {"kind": "text", "question": 2, "text": "  café 🦀\n  "},
    {"kind": "options", "question": 3, "options": [{"option": 0, "label": "Read"}, {"option": 2, "label": "Search"}]},
    {"kind": "options", "question": 4, "options": []},
]
requests, errors = [], []


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(body)
            user = max(index for index, message in enumerate(body["messages"]) if message["role"] == "user")
            prompt = str(body["messages"][user]["content"])
            names = {tool["function"]["name"] for tool in body.get("tools", [])}
            if prompt == "NO_QUESTIONS":
                assert "ask_user_question" not in names, names
                delta, finish = {"content": "NO_QUESTIONS_OK"}, "stop"
            else:
                assert "ask_user_question" in names, names
                tail = body["messages"][user + 1:]
                if not tail:
                    # The second call must not dispatch before the question resolves.
                    calls = [
                        ("question-1", "ask_user_question", {"questions": questions}),
                        ("dependent-1", "exec", {"command": "printf 'dependent\\n' > question-effect.txt"}),
                    ]
                    delta = {"tool_calls": [
                        {"index": index, "id": call, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}
                        for index, (call, name, arguments) in enumerate(calls)
                    ]}
                    finish = "tool_calls"
                else:
                    observed = {}
                    for message in tail:
                        if message["role"] == "tool":
                            envelope = json.loads(message["content"])
                            observed[message["tool_call_id"]] = envelope.get("result", envelope)
                    decision = observed["question-1"]
                    if decision["status"] == "answered":
                        assert decision["answers"] == expected_answers, decision
                    else:
                        assert decision == {"status": "cancelled", "reason": "user_cancelled"}, decision
                    assert observed["dependent-1"]["exit_code"] == 0, observed
                    delta, finish = {"content": "QUESTION_FLOW_OK"}, "stop"
            payload = {"id": "questions", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            encoded = b"data: " + json.dumps(payload).encode() + b"\n\ndata: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)
        except Exception as error:
            errors.append(repr(error))
            self.send_error(400, "question fixture rejected request")

    def log_message(self, *_args):
        pass


class Client:
    def __init__(self, cwd, env, extra=()):
        self.cwd, self.env = cwd, env
        self.process = subprocess.Popen([binary, "--cwd", cwd, *extra, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            # This pipe belongs to the test; keep the pressure proof deterministic on Linux.
            if hasattr(fcntl, "F_SETPIPE_SZ"):
                fcntl.fcntl(self.process.stdout, fcntl.F_SETPIPE_SZ, 4096)
            ready = self.read()
            assert ready["type"] == "ready", ready
            self.session = ready["session"]
        except BaseException:
            self.cleanup()
            raise

    def send(self, command):
        self.process.stdin.write(json.dumps(command).encode() + b"\n")
        self.process.stdin.flush()

    def read(self):
        assert select.select([self.process.stdout], [], [], 8)[0], "question RPC output timed out"
        line = self.process.stdout.readline()
        assert line, ("question RPC ended unexpectedly", self.process.poll())
        return json.loads(line)

    def through(self, predicate):
        records = []
        while True:
            record = self.read()
            records.append(record)
            if predicate(record):
                return records

    def command(self, kind, **fields):
        identity = str(uuid.uuid4())
        self.send({"type": kind, "id": identity, **fields})
        return self.through(lambda record: record.get("id") == identity)[-1]

    def question(self, padding=""):
        self.send({"type": "prompt", "id": "prompt", "message": "ASK_QUESTIONS" + padding})
        while True:
            records = self.through(lambda record: record["type"] in ("question_changed", "turn_end"))
            assert records[-1]["type"] == "question_changed", ("Turn ended without a pending question", records)
            reply = self.command("get_question")
            assert reply["success"], reply
            pending = reply["data"]["question"]
            if pending is not None:
                break
        for actual, expected in zip(pending["questions"], questions, strict=True):
            assert actual["question"] == expected["question"], pending
            assert actual["multi_select"] == expected.get("multi_select", False), pending
            assert actual["options"] == [dict(label=option["label"], description=option.get("description")) for option in expected["options"]], pending
        assert pending["origin"]["session"] == self.session, pending
        assert not (self.cwd / "question-effect.txt").exists(), "dependent call ran before the answer"
        return pending["request"]

    def close(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        records = []
        if not self.process.stdout.closed:
            while True:
                assert select.select([self.process.stdout], [], [], 8)[0], "question RPC shutdown timed out"
                line = self.process.stdout.readline()
                if not line:
                    break
                records.append(json.loads(line))
        assert self.process.wait(timeout=8) == 0, self.process.stderr.read()
        return records

    def cleanup(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=8)
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            if pipe and not pipe.closed:
                pipe.close()


def inspect(client):
    result = subprocess.run([binary, "--cwd", client.cwd, "--session", client.session, "inspect"], env=client.env, capture_output=True, check=True, timeout=8)
    return json.loads(result.stdout)


with tempfile.TemporaryDirectory(prefix="ion-rpc-questions-") as temporary:
    work = Path(temporary)
    env = {name: os.environ[name] for name in ("PATH", "SHELL", "TMPDIR") if name in os.environ}
    env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = False
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    clients = []
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "questions", "fixture", "--endpoint", endpoint, "--wire", "chat-completions", "--context-window", "1000000", "--max-output-tokens", "512"], env=env, check=True, capture_output=True)

        def start(name, extra=()):
            cwd = work / name
            cwd.mkdir(exist_ok=True)
            client = Client(cwd, env, extra)
            clients.append(client)
            return client

        client = start("answered")
        empty = client.command("get_question")
        assert empty["success"] and empty["data"]["question"] is None, empty
        nonce = client.question(" x" * 131072)
        valid = {"type": "answer_question", "request": nonce, "answers": answers}
        invalid = [
            {**valid, "request": str(uuid.uuid4())},
            {**valid, "request": None},
            {**valid, "extra": True},
            {**valid, "answers": None},
            {**valid, "answers": answers[:1]},
            {**valid, "answers": answers * 3},
        ]
        for answer in [
            {"kind": "option", "index": 9}, {"kind": "option", "index": -1},
            {"kind": "option", "index": 1.0}, {"kind": "option", "index": True},
            {"kind": "option", "index": 1, "label": "forged"},
            {"kind": "text", "text": "  "}, {"kind": "text", "text": "é" * 2049},
            {"kind": "text", "text": False}, {"kind": "unknown", "text": "ignored"},
        ]:
            invalid.append({**valid, "answers": [answers[0], answer, *answers[2:]]})
        for index, answer in [
            (0, {"kind": "options", "indices": [0]}),  # wrong mode
            (2, {"kind": "option", "index": 0}),
            (2, {"kind": "options", "indices": [0, 0]}),
            (2, {"kind": "options", "indices": [3]}),
            (2, {"kind": "options", "indices": [0, 1, 2, 0]}),
            (2, {"kind": "options", "indices": [True]}),
            (2, {"kind": "options", "indices": [1.0]}),
            (2, {"kind": "options", "indices": [-1]}),
            (2, {"kind": "options", "indices": None}),
            (2, {"kind": "options", "indices": [0], "labels": ["forged"]}),
        ]:
            invalid.append({**valid, "answers": [*answers[:index], answer, *answers[index + 1:]]})
        for command in invalid:
            reply = client.command(command.pop("type"), **command)
            assert not reply["success"], reply
        assert client.command("get_question")["data"]["question"]["request"] == nonce
        assert not (client.cwd / "question-effect.txt").exists()

        # A large inspection fills physical stdout. All ordinary reply slots then
        # remain retained, but an answer in the reserved control lane must advance
        # the existing Turn before this test resumes reading output.
        client.send({"type": "inspect", "id": "blocked-inspection"})
        for index in range(40):
            client.send({"type": "get_state", "id": f"bulk-{index}"})
        client.send({"type": "answer_question", "id": "answer", "request": nonce, "answers": answers})
        deadline = time.monotonic() + 8
        effect = client.cwd / "question-effect.txt"
        while not effect.exists() or effect.read_bytes() != b"dependent\n":
            assert time.monotonic() < deadline, "bulk publication blocked the completed dependent effect"
            time.sleep(0.01)
        assert (client.cwd / "question-effect.txt").read_text() == "dependent\n"
        settled = client.through(lambda record: record["type"] == "turn_end")
        assert settled[-1]["status"] == "completed" and settled[-1]["turn"] == 1, settled[-1]
        assert any(record.get("id") == "answer" and record.get("success") for record in settled), settled
        assert sum(record.get("commands_not_admitted", 0) for record in settled) >= 8, settled
        assert client.command("get_question")["data"]["question"] is None
        assert not client.command("answer_question", request=nonce, answers=answers)["success"]
        assert not client.command("cancel_question", request=nonce)["success"]
        view = client.command("inspect")["data"]
        observed = [entry["data"]["result"]["outcome"] for entry in view["entries"] if entry["kind"] == "tool_result" and entry["data"]["result"]["name"] == "ask_user_question"]
        assert len(observed) == 1 and observed[0]["state"] == "observed", observed
        assert observed[0]["output"]["value"] == {"status": "answered", "answers": expected_answers}, observed
        assert len([entry for entry in view["entries"] if entry["kind"] == "turn_started"]) == 1
        client.close()

        # A fresh process reattaches only to live control, not old question history.
        reopened = start("answered", ("--session", client.session))
        assert reopened.command("get_question")["data"]["question"] is None
        assert not reopened.command("answer_question", request=nonce, answers=answers)["success"]
        replaced = reopened.command("new_session")
        assert replaced["success"], replaced
        reopened.session = replaced["data"]["session"]
        (reopened.cwd / "question-effect.txt").unlink()
        fresh = reopened.question()
        assert fresh != nonce
        assert not reopened.command("answer_question", request=nonce, answers=answers)["success"]
        assert not reopened.command("cancel_question", request=fresh, extra=True)["success"]
        assert reopened.command("cancel_question", request=fresh)["success"]
        assert reopened.through(lambda record: record["type"] == "turn_end")[-1]["status"] == "completed"
        assert (reopened.cwd / "question-effect.txt").read_text() == "dependent\n", "batch cancellation incorrectly stopped the Turn"
        reopened.close()

        for mode in ("abort", "eof", "broken-output"):
            client = start(mode)
            before = len(requests)
            request = client.question()
            if mode == "abort":
                client.send({"type": "abort", "id": "stop"})
                assert client.through(lambda record: record["type"] == "turn_end")[-1]["status"] == "cancelled"
                assert client.command("get_question")["data"]["question"] is None
                assert not client.command("answer_question", request=request, answers=answers)["success"]
                client.close()
            elif mode == "eof":
                closed = client.close()
                assert any(record["type"] == "turn_end" and record["status"] == "cancelled" for record in closed), closed
            else:
                client.process.stdout.close()
                client.send({"type": "get_question", "id": "broken"})
                assert client.process.wait(timeout=8) == 1
            view = inspect(client)
            assert view["unfinished_turn"] is None, view
            outcomes = {entry["data"]["result"]["call_id"]: entry["data"]["result"]["outcome"] for entry in view["entries"] if entry["kind"] == "tool_result"}
            assert outcomes["question-1"]["state"] == "observed", outcomes
            assert outcomes["question-1"]["output"]["value"]["status"] == "cancelled", outcomes
            assert outcomes["dependent-1"]["state"] == "not_dispatched", outcomes
            assert not (client.cwd / "question-effect.txt").exists(), "owner loss dispatched a dependent call"
            assert len(requests) == before + 1, "stop dispatched another generation"

        # One-shot headless has no answer channel. An unattended RPC client can
        # use existing tool selection rather than a new transport-specific flag.
        standalone = subprocess.run([binary, "--cwd", work, "run", "NO_QUESTIONS"], env=env, capture_output=True, timeout=8, check=True)
        assert b"NO_QUESTIONS_OK" in standalone.stdout
        unattended = start("unattended", ("--exclude-tools", "ask_user_question"))
        unattended.send({"type": "prompt", "message": "NO_QUESTIONS"})
        assert unattended.through(lambda record: record["type"] == "turn_end")[-1]["status"] == "completed"
        unattended.close()
    finally:
        for client in clients:
            client.cleanup()
        server.shutdown()
        server.server_close()
        thread.join(timeout=8)
        assert not thread.is_alive(), "question fixture server did not join"
    assert not errors, errors

print("Ion RPC questions: typed answers, pressure, cancellation, binding/reopen and joined owner loss: OK")
