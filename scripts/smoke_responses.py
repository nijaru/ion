"""Qualify stateless Responses replay and effect/retry fences through the CLI."""

import base64
import json
import os
import struct
import subprocess
import tempfile
import threading
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = {}
errors = []


def event(kind, **fields):
    return f"event: {kind}\ndata: {json.dumps({'type': kind, **fields})}\n\n".encode()


def call(arguments, *, status="completed", name="write"):
    return dict(type="function_call", id="fc_1", call_id="call_1", name=name,
                arguments=json.dumps(arguments) if not isinstance(arguments, str) else arguments,
                status=status)


def message(identity, text, *, phase="final_answer", refusal=False):
    part = dict(type="refusal", refusal=text) if refusal else dict(type="output_text", text=text, annotations=[])
    return dict(type="message", id=identity, role="assistant", status="completed", phase=phase, content=[part])


def finish(output, *, incomplete=False):
    response = dict(id="resp_1", model="mock", status="incomplete" if incomplete else "completed", output=output,
                    usage=dict(input_tokens=10, output_tokens=8, total_tokens=18,
                               input_tokens_details=dict(cached_tokens=6), output_tokens_details=dict(reasoning_tokens=3)))
    if incomplete:
        response["incomplete_details"] = dict(reason="max_output_tokens")
    return event("response.incomplete" if incomplete else "response.completed", response=response)


def failed(output):
    return event("response.failed", response=dict(id="resp_1", status="failed", output=output,
                 error=dict(code="server_error", message="fixture failure")))


def png():
    def chunk(kind, payload):
        return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00\xff")) + chunk(b"IEND", b"")


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert self.headers.get("Authorization") == "Bearer fixture-key"
            assert self.headers.get("x-api-key") is None
            assert body["store"] is False and body["stream"] is True
            assert body["include"] == ["reasoning.encrypted_content"]
            assert "previous_response_id" not in body and "conversation" not in body
            assert all(tool["type"] == "function" and tool["strict"] is False for tool in body["tools"])
            history = requests.setdefault(self.path, [])
            history.append(body)
            step = len(history)
            if self.path == "/v1/responses":
                payload = self.main_response(body, step)
            elif self.path == "/retry/v1/responses":
                payload = failed([]) if step == 1 else finish([message("msg_retry", "RETRIED")])
            elif self.path == "/post-output/v1/responses":
                # No earlier delta: the failed snapshot itself must fence retry.
                payload = failed([call(dict(path="forbidden.txt", content="NEVER"))])
            elif self.path == "/namespace/v1/responses":
                foreign = call(dict(path="namespace-forbidden.txt", content="NEVER"))
                foreign["namespace"] = "foreign"
                payload = finish([foreign]) if step == 1 else finish([message("msg_namespace", "WRONG_LOCAL_TOOL_EXECUTED")])
            elif self.path == "/truncated/v1/responses":
                if step == 1:
                    payload = finish([call('{"path":"forbidden.txt"', status="incomplete")], incomplete=True)
                else:
                    retained = next(item for item in body["input"] if item["type"] == "function_call")
                    assert retained["arguments"] == '{"path":"forbidden.txt"'
                    result = next(item for item in body["input"] if item["type"] == "function_call_output")
                    assert json.loads(result["output"])["is_error"] is True
                    payload = finish([message("msg_truncated", "TRUNCATED_NOT_EXECUTED")])
            else:
                raise AssertionError(f"unexpected endpoint: {self.path}")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except Exception as error:
            errors.append(repr(error))
            self.send_error(500, "fixture assertion failed")

    def main_response(self, body, step):
        inputs = body["input"]
        if step == 1:
            user = next(item for item in inputs if item.get("role") == "user")
            assert user["content"][1]["type"] == "input_image"
            assert base64.b64decode(user["content"][1]["image_url"].split(",", 1)[1]) == png()
            reasoning = dict(id="rs_1", type="reasoning", summary=[])
            write = call(dict(path="created.txt", content="INDEPENDENT_EFFECT\n"))
            # Ciphertext is deliberately absent from the done item.
            return (event("response.output_item.added", output_index=0, item=reasoning)
                    + event("response.output_item.done", output_index=0, item=reasoning)
                    + finish([{**reasoning, "encrypted_content": "TERMINAL_CIPHER"}, write]))
        reasoning = next(item for item in inputs if item["type"] == "reasoning")
        assert reasoning == dict(id="rs_1", type="reasoning", summary=[], encrypted_content="TERMINAL_CIPHER")
        if step == 2:
            result = next(item for item in inputs if item["type"] == "function_call_output")
            assert result["call_id"] == "call_1" and json.loads(result["output"])["is_error"] is False
            image_read = call(dict(path="red.png"), name="read")
            image_read.update(id="fc_2", call_id="call_2")
            return finish([image_read])
        if step == 3:
            result = next(item for item in inputs if item.get("call_id") == "call_2" and item["type"] == "function_call_output")
            assert json.loads(result["output"][0]["text"])["is_error"] is False
            assert result["output"][1]["type"] == "input_image"
            assert base64.b64decode(result["output"][1]["image_url"].split(",", 1)[1]) == png()
            return finish([message("msg_comment", "Checked. ", phase="commentary"), message("msg_final", "Done.", refusal=True)])
        assert step == 4
        assert next(item for item in inputs if item.get("id") == "msg_comment") == message("msg_comment", "Checked. ", phase="commentary")
        assert next(item for item in inputs if item.get("id") == "msg_final") == message("msg_final", "Done.", refusal=True)
        return finish([message("msg_reopen", "REOPENED")])

    def log_message(self, format, *args):
        pass


def run(env, *args, success=True):
    result = subprocess.run([binary, *map(str, args)], env=env, capture_output=True, text=True, timeout=30)
    assert not errors, errors
    assert (result.returncode == 0) == success, (result.args, result.stdout, result.stderr)
    return [json.loads(line) for line in result.stdout.splitlines()] if "--json" in args else []


with tempfile.TemporaryDirectory(prefix="ion-responses-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "red.png").write_bytes(png())
    (workspace / "AGENTS.md").write_text("RESPONSES_PROJECT\n")
    for directory in ("home", "tmp"):
        (work / directory).mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), TMPDIR=str(work / "tmp"),
               XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"), XDG_DATA_HOME=str(work / "data"), PYTHONSAFEPATH="1", RESPONSES_FIXTURE_KEY="fixture-key")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base = f"http://127.0.0.1:{server.server_port}"

    def select(prefix=""):
        run(env, "use", "responses-fixture", "mock", "--endpoint", base + prefix + "/v1", "--wire", "openai-responses", "--images", "--api-key-env", "RESPONSES_FIXTURE_KEY")

    def submit(name, prompt, *args, success=True):
        return run(env, "--json", "--cwd", workspace, "--session", work / f"{name}.sqlite", *args, "run", prompt, success=success)

    try:
        select()
        first = submit("main", "Create created.txt and read this image.", "--image", "red.png")
        assert first[-1] == dict(type="run_end", status="completed"), first
        assert (workspace / "created.txt").read_text() == "INDEPENDENT_EFFECT\n"
        reopened = submit("main", "Continue")
        assert reopened[-1] == dict(type="run_end", status="completed"), reopened
        assert len(requests["/v1/responses"]) == 4
        inspected = subprocess.run([binary, "--cwd", workspace, "--session", work / "main.sqlite", "inspect"], env=env, capture_output=True, text=True, check=True, timeout=20)
        entries = json.loads(inspected.stdout)["entries"]
        replay = next(entry["data"]["message"]["provider_replay"] for entry in entries
                      if entry["kind"] == "assistant" and entry["data"]["message"]["provider_replay"])
        assert replay["data"]["output"][0]["encrypted_content"] == "TERMINAL_CIPHER"
        assert not any("TERMINAL_CIPHER" in item.get("text", "") for item in first), "ciphertext entered human progress"

        select("/retry")
        retried = submit("retry", "Retry before output")
        assert sum(item["type"] == "provider_retry" for item in retried) == 1, retried
        assert requests["/retry/v1/responses"][0] == requests["/retry/v1/responses"][1]

        select("/post-output")
        failed_run = submit("failed", "Do not execute incomplete output", success=False)
        assert not any(item["type"] in ("provider_retry", "tool_started", "tool_finished") for item in failed_run), failed_run
        assert len(requests["/post-output/v1/responses"]) == 1
        assert not (workspace / "forbidden.txt").exists()

        select("/namespace")
        namespace = submit("namespace", "Reject foreign function identities", success=False)
        assert not any(item["type"] in ("tool_started", "tool_finished") for item in namespace), namespace
        assert len(requests["/namespace/v1/responses"]) == 1
        assert not (workspace / "namespace-forbidden.txt").exists()

        select("/truncated")
        truncated = submit("truncated", "Handle a truncated call safely")
        assert truncated[-1] == dict(type="run_end", status="completed"), truncated
        assert len(requests["/truncated/v1/responses"]) == 2
        assert not (workspace / "forbidden.txt").exists()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        assert not thread.is_alive()

print("Responses stateless replay, native images, real write, reopen and failed/truncated effect fences: ok")
