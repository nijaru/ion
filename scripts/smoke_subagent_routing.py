"""Offline coding-client oracle for child route/effort selection and preflight."""
import json
import os
import sqlite3
import subprocess
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


with tempfile.TemporaryDirectory(prefix="ion-subagent-routing-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    (workspace / "AGENTS.md").write_text("ROUTING_RESOURCE: retain this repository instruction.\n")
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
               TMPDIR=str(root / "tmp"), PARENT_TOKEN="parent-fixture", REVIEW_TOKEN="review-fixture")
    Path(env["HOME"]).mkdir()
    Path(env["TMPDIR"]).mkdir()
    cases = [
        ("route", {"model": "review/reviewer"}, "reviewer", "high"),
        ("inherit", {}, "parent", "high"),
        ("same", {"model": "smoke/parent"}, "parent", "high"),
        ("effort", {"reasoning": "off"}, "parent", "none"),
        ("route_low", {"model": "review/reviewer", "reasoning": "low"}, "reviewer", "low"),
        ("fork_background", {"model": "review/reviewer", "reasoning": "default",
                             "context": {"type": "fork"}, "mode": "background"}, "reviewer", None),
    ]
    refusals = [
        {"model": "review/missing"}, {"model": "../parent"},
        {"model": "parent"}, {"model": "/parent"}, {"model": "smoke/"},
        {"model": "anthropic/claude-opus-5-5", "reasoning": "default"}, {"reasoning": "budget:0"},
        {"reasoning": "budget:12"}, {"reasoning": "extreme"},
        {"model": "restricted/no-thinking"},
        {"model": "review/reviewer", "endpoint": "http://127.0.0.1:1"},
    ]
    current, requests, failures, retargeted = {}, [], [], set()

    def run(*arguments):
        return subprocess.run([BINARY, "--cwd", workspace, *arguments], env=env,
                              check=True, capture_output=True, text=True, timeout=20)

    def configure(provider, model, endpoint, token, wire="chat-completions", output=256):
        run("use", provider, model, "--endpoint", endpoint, "--wire", wire,
            "--api-key-env", token, "--max-output-tokens", str(output))

    def tool(name, arguments):
        return {"tool_calls": [{"index": 0, "id": "fixture-call", "type": "function",
                                "function": {"name": name, "arguments": json.dumps(arguments)}}]}

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append((self.path, body))
                user = next(message["content"] for message in reversed(body["messages"])
                            if message["role"] == "user")
                child = user.startswith("CHILD_")
                selected = current["model"] if child else "parent"
                assert body["model"] == selected, body
                assert self.path == f"/{selected}/v1/chat/completions", self.path
                expected_token = "review-fixture" if selected == "reviewer" else "parent-fixture"
                assert self.headers.get("Authorization") == "Bearer " + expected_token
                assert body.get("reasoning_effort") == (current["effort"] if child else "high"), body
                assert body["max_completion_tokens"] == (512 if selected == "reviewer" else 256)
                assert "ROUTING_RESOURCE" in json.dumps(body)
                fresh = body["messages"][-1]["role"] == "user"
                if user == "SEED":
                    delta = {"content": "SETTLED_FORK_SEED"}
                elif child:
                    assert user == "CHILD_" + current["name"]
                    names = {item["function"]["name"] for item in body.get("tools", [])}
                    assert not names.intersection({"subagent", "exec", "ask_user_question"}), names
                    assert ("SETTLED_FORK_SEED" in json.dumps(body)) == current["fork"]
                    assert "PARENT_" not in json.dumps(body), "unfinished initiating Turn leaked"
                    if fresh:
                        # Neither a live child nor its parent may re-resolve this
                        # changed store route on the next model/tool step.
                        provider, token = ("review", "REVIEW_TOKEN") if selected == "reviewer" else ("smoke", "PARENT_TOKEN")
                        configure(provider, selected, "http://127.0.0.1:1/v1", token)
                        retargeted.add("child")
                        delta = tool("write", {"path": current["name"] + ".txt", "content": "INDEPENDENT_CHILD_EFFECT"})
                    else:
                        assert not json.loads(body["messages"][-1]["content"])["is_error"]
                        delta = {"content": "OBSERVED_CHILD_ANSWER"}
                elif fresh:
                    # Inherited and explicitly same-identity selections must use
                    # the captured parent route, not this replacement.
                    configure("smoke", "parent", "http://127.0.0.1:1/v1", "PARENT_TOKEN")
                    retargeted.add("parent")
                    arguments = {"action": "start", "task": "CHILD_" + current["name"], **current["arguments"]}
                    if current["fork"]:
                        code = "const r=await tools.subagent(" + json.dumps(arguments) + "); const w=await tools.subagent({action:'wait',children:[r.value.child_session]}); return w.value.children[0];"
                        delta = tool("code_mode", {"code": code})
                    else:
                        delta = tool("subagent", arguments)
                else:
                    envelope = json.loads(body["messages"][-1]["content"])
                    result = envelope["result"] if current["fork"] else envelope
                    if current["refused"]:
                        assert result["is_error"] and "error" in result["result"], result
                        if current["arguments"].get("model") == "anthropic/claude-opus-5-5":
                            assert "credential" in result["result"]["error"], result
                        delta = {"content": "EXPECTED_REFUSAL"}
                    elif not current["fork"] and result["is_error"]:
                        # Keep an old build's refusal explicit instead of turning
                        # the provider into a retry/HTTP-error fixture.
                        delta = {"content": "UNEXPECTED_REFUSAL"}
                    else:
                        delta = {"content": "PARENT_OK"}
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
    endpoint = f"http://127.0.0.1:{server.server_port}"
    try:
        for name, arguments, model, effort in cases + [
            (f"refusal_{index}", arguments, "parent", "high") for index, arguments in enumerate(refusals)
        ]:
            refused = name.startswith("refusal_")
            current.update(name=name, arguments=arguments, model=model, effort=effort,
                           refused=refused, fork=name == "fork_background")
            retargeted.clear()
            configure("review", "reviewer", endpoint + "/reviewer/v1", "REVIEW_TOKEN", output=512)
            configure("restricted", "no-thinking", endpoint + "/no-thinking/v1", "PARENT_TOKEN", wire="llama-cpp-no-thinking")
            configure("smoke", "parent", endpoint + "/parent/v1", "PARENT_TOKEN")
            before = set(Path(env["ION_HOME"]).rglob("*.sqlite"))
            start = len(requests)
            options = ["--reasoning", "high", "--tools", "read,write,subagent,code_mode"]
            if current["fork"]:
                run(*options, "run", "SEED")
                parent = (set(Path(env["ION_HOME"]).rglob("*.sqlite")) - before).pop()
                options += ["--session", str(parent), "--code-mode"]
            records = [json.loads(line) for line in run(*options, "--json", "run", "PARENT_" + name).stdout.splitlines()]
            assert not failures, failures
            final = next(record for record in records if record["type"] == "final")
            assert final["text"] == ("EXPECTED_REFUSAL" if refused else "PARENT_OK"), final
            allocated = set(Path(env["ION_HOME"]).rglob("*.sqlite")) - before
            assert len(allocated) == (1 if refused else 2), (name, allocated)
            parent = next(path for path in allocated if path.parent.name != "children")
            admitted = [entry["data"]["record"] for entry in facts(parent) if entry["kind"] == "delegation_admitted"]
            if refused:
                assert not admitted and len(requests) - start == 2, (name, admitted, requests[start:])
                assert not (workspace / (name + ".txt")).exists()
                continue
            assert len(admitted) == 1 and retargeted == {"parent", "child"}, (admitted, retargeted)
            child_path = Path(admitted[0]["path"])
            identity = {"provider": "review" if model == "reviewer" else "smoke", "model": model}
            assert admitted[0]["spec"]["model"] == identity
            expected_reasoning = {None: "ProviderDefault", "none": "Off", "low": "Low", "high": "High"}[effort]
            assert admitted[0]["spec"]["reasoning"] == expected_reasoning
            link = next(entry["data"]["record"] for entry in facts(child_path) if entry["kind"] == "child_session_of")
            assert link == admitted[0]
            assert (workspace / (name + ".txt")).read_text() == "INDEPENDENT_CHILD_EFFECT"
            result = next(record["output"] for record in records if record["type"] == "tool_finished")
            result = result["result"] if current["fork"] else result
            assert result["model"] == identity
            assert result["reasoning"] == ("default" if effort is None else "off" if effort == "none" else effort)
            assert result["joined"] and result["outcome"]["answer"] == "OBSERVED_CHILD_ANSWER"
            if current["fork"]:
                owner = admitted[0]["ownership"]["background"]["owner"]
                transfer = next(entry["data"] for entry in facts(parent) if entry["kind"] == "delegation_transferred")
                assert transfer == {"child": admitted[0]["child"], "owner": owner}
                assert result["transferred"]
            else:
                assert admitted[0]["ownership"] == "foreground"
        print("coding child routing: inherited/cross/same routes, independent effort, credentials/limits, captured bindings, fork/background/Code Mode, effects and pre-admission refusals: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()
