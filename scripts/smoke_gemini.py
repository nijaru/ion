"""Qualify the built Gemini adapter against a loopback-only SSE endpoint."""

import json
import os
import queue
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))
MODEL = "gemini-4-hydrogen"
ENDPOINT = f"/v1beta/models/{MODEL}:streamGenerateContent?alt=sse"
SIGNATURE = "c2lnbmVkLWNhbGw="
SIGNED = [
    {"text": "A signed thought", "thought": True, "thoughtSignature": "c2lnbmVkLXRob3VnaHQ="},
    {"functionCall": {"name": "write", "args": {"path": "one.txt", "content": "one\n"}, "id": "native_one"}, "thoughtSignature": SIGNATURE},
    {"functionCall": {"name": "write", "args": {"path": "two.txt", "content": "two\n"}}},
    {"text": "Waiting for both results."},
]
RELEASE = threading.Event()
requests = {}
errors = []


def event(*parts, finish=None, usage=None):
    candidate = {"index": 0}
    if parts:
        candidate["content"] = {"role": "model", "parts": parts}
    if finish:
        candidate["finishReason"] = finish
    payload = {"candidates": [candidate], "modelVersion": MODEL}
    if usage is not None:
        payload["usageMetadata"] = usage
    return f"data: {json.dumps(payload)}\n\n".encode()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            assert self.path.endswith(ENDPOINT), self.path
            route = self.path[:-len(ENDPOINT)]
            assert self.headers["x-goog-api-key"] == "fixture-key"
            assert "Authorization" not in self.headers and "key=" not in self.path
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert body["store"] is False
            assert "model" not in body and "previous_interaction_id" not in body
            assert body["generationConfig"]["candidateCount"] == 1
            assert body["toolConfig"]["functionCallingConfig"] == {"mode": "AUTO"}
            assert [tool["name"] for tool in body["tools"][0]["functionDeclarations"]] == ["write"]
            history = requests.setdefault(route, [])
            history.append(body)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            if route == "/ok" and len(history) == 1:
                assert body["generationConfig"]["thinkingConfig"] == {"thinkingLevel": "XHIGH", "includeThoughts": True}
                self.wfile.write(event(*SIGNED[:3]))
                self.wfile.write(event(SIGNED[3]))
                self.wfile.flush()
                assert RELEASE.wait(10), "terminal transport release not received"
                self.wfile.write(event(finish="STOP", usage={
                    "promptTokenCount": 10, "cachedContentTokenCount": 4,
                    "candidatesTokenCount": 3, "thoughtsTokenCount": 7,
                }))
            elif route == "/ok":
                contents = body["contents"]
                assert contents[1] == {"role": "model", "parts": SIGNED}, contents
                observed = contents[2]
                assert observed["role"] == "user"
                first, second = [part["functionResponse"] for part in observed["parts"]]
                assert first["name"] == second["name"] == "write"
                assert first["id"] == "native_one" and "id" not in second
                assert all("output" in result["response"] for result in (first, second))
                assert body["generationConfig"]["thinkingConfig"]["thinkingLevel"] == "XHIGH"
                text = "GEMINI_DONE" if len(history) == 2 else "GEMINI_REOPENED"
                self.wfile.write(event({"text": text}, finish="STOP"))
            elif route in ("/unsigned", "/truncated"):
                call = {"functionCall": {"name": "write", "args": {"path": "forbidden.txt", "content": "never"}}}
                if route == "/truncated":
                    call["thoughtSignature"] = SIGNATURE
                self.wfile.write(event(call, finish="STOP" if route == "/unsigned" else None))
            else:
                raise AssertionError(route)
        except Exception as error:
            errors.append(repr(error))

    def log_message(self, *args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-gemini-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    env = {name: os.environ[name] for name in ("PATH", "SHELL") if name in os.environ}
    env.update(HOME=str(work / "home"), ION_HOME=str(work / "ion"), GEMINI_FIXTURE_KEY="fixture-key")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base = f"http://127.0.0.1:{server.server_port}"

    def use(route):
        subprocess.run([BINARY, "use", "gemini-fixture", MODEL, "--wire", "gemini-generate-content",
                        "--endpoint", base + route, "--api-key-env", "GEMINI_FIXTURE_KEY"],
                       env=env, check=True, capture_output=True, timeout=15)

    def command(db, *args):
        return [BINARY, "--json", "--cwd", workspace, "--session", work / f"{db}.sqlite",
                "--tools", "write", *args]

    try:
        use("/ok")
        child = subprocess.Popen(command("ok", "--reasoning", "xhigh", "run", "Write both files"),
                                 env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        lines = queue.Queue()
        published = []

        def read_stdout():
            for line in child.stdout:
                published.append(line)
                lines.put(json.loads(line))
            lines.put(None)

        reader = threading.Thread(target=read_stdout, daemon=True)
        reader.start()
        try:
            while True:
                record = lines.get(timeout=10)
                assert record is not None, child.poll()
                if record.get("type") == "text_delta" and record["text"] == "Waiting for both results.":
                    break
            assert not (workspace / "one.txt").exists() and not (workspace / "two.txt").exists()
            RELEASE.set()
            assert child.wait(timeout=20) == 0, child.stderr.read()
            reader.join(timeout=5)
            assert not reader.is_alive()
            assert all(SIGNATURE not in line for line in published)
        finally:
            RELEASE.set()
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
        assert (workspace / "one.txt").read_text() == "one\n"
        assert (workspace / "two.txt").read_text() == "two\n"
        assert len(requests["/ok"]) == 2, errors
        (workspace / "one.txt").write_text("edited after the first run\n")
        reopened = subprocess.run(command("ok", "run", "Continue"), env=env, text=True,
                                  capture_output=True, check=True, timeout=20)
        assert "GEMINI_REOPENED" in reopened.stdout and SIGNATURE not in reopened.stdout
        assert len(requests["/ok"]) == 3
        assert (workspace / "one.txt").read_text() == "edited after the first run\n"

        for route in ("/unsigned", "/truncated"):
            use(route)
            result = subprocess.run(command(route[1:], "run", "Do not execute an unsettled call"),
                                    env=env, text=True, capture_output=True, timeout=20)
            assert result.returncode != 0, (route, result.stdout, result.stderr)
            assert len(requests.get(route, [])) == 1 and not (workspace / "forbidden.txt").exists()
        use("/off")
        refused = subprocess.run(command("off", "--reasoning", "off", "run", "Do not send"),
                                 env=env, text=True, capture_output=True, timeout=20)
        assert refused.returncode != 0 and "does not support this reasoning" in refused.stderr, refused
        assert "/off" not in requests
        assert not errors, errors
    finally:
        RELEASE.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)

print("Gemini: SSE terminal custody, signed/ID-less parallel tools, exact reopen, credentials, and refusal: OK", flush=True)
