"""Real CLI grammar and literal stdin boundaries; loopback only, isolated state."""

import json
import os
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))
requests = []


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        events = [
            {"choices": [{"index": 0, "delta": {"content": "CLI_OK"}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events)
        payload += b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-cli-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    (root / "tmp").mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
               TMPDIR=str(root / "tmp"), PYTHONSAFEPATH="1")
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()

    def run(*args, input=b"", check=True):
        result = subprocess.run([BINARY, *map(str, args)], input=input, env=env,
                                capture_output=True, timeout=15)
        if check:
            assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result

    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        for provider, model in (("first", "vendor/model"), ("second", "other")):
            run("use", provider, model, "--endpoint", endpoint, "--wire", "chat-completions")
        store = root / "ion" / "models.json"
        saved_default = store.read_bytes()

        # stdin-only modes preserve exact whitespace and command-looking text.
        for args, piped, expected in [
            (["--model", "first/vendor/model", "-p"], b"  stdin only\n\n", "  stdin only\n\n"),
            (["--model", "first/vendor/model", "run"], b"/not-a-template\n", "/not-a-template\n"),
            (["--json", "--model", "first/vendor/model"], b"bare pipeline", "bare pipeline"),
            (["--model", "first/vendor/model", "-p", "argument"], b"literal context\n", "literal context\n\nargument"),
            (["run", "argument", "--model", "first/vendor/model"], b"", "argument"),
            (["--model", "first/vendor/model", "  bare 🦀\n "], b"", "  bare 🦀\n "),
            (["-p", "--model", "first/vendor/model", "separate mode"], b"", "separate mode"),
            (["--model", "first/vendor/model", "-p", "--", "- dash-leading"], b"", "- dash-leading"),
            (["--provider", "first", "run", "--model", "vendor/model", "paired"], b"", "paired"),
        ]:
            result = run("--cwd", workspace, *args, input=piped)
            assert requests[-1]["model"] == "vendor/model", requests[-1]
            assert requests[-1]["messages"][-1]["content"] == expected, requests[-1]
            if "--json" in args:
                records = [json.loads(line) for line in result.stdout.splitlines()]
                assert records[-1] == {"type": "run_end", "status": "completed"}, records
            else:
                assert result.stdout.strip() == b"CLI_OK", result.stdout

        # Expansion applies to the argument, not to command-looking pipe input.
        prompts = workspace / ".ion" / "prompts"
        prompts.mkdir(parents=True)
        (prompts / "hello.md").write_text("EXPANDED_ARGUMENT\n")
        run("--cwd", workspace, "-p", "/hello", input=b"/hello")
        assert requests[-1]["messages"][-1]["content"] == "/hello\nEXPANDED_ARGUMENT\n"

        # An invocation override changes the bound Session, not the global default.
        session = root / "conversation.sqlite"
        run("--cwd", workspace, "--session", session, "run", "default")
        assert requests[-1]["model"] == "other"
        run("--session", session, "--model", "first/vendor/model", "run", "override")
        run("run", "restore", "--session", session)
        assert requests[-1]["model"] == "vendor/model"
        view = json.loads(run("inspect", "--session", session).stdout)
        assert view["last_model"] == {"provider": "first", "model": "vendor/model"}
        assert store.read_bytes() == saved_default

        # Failed input cannot create a Session, start MCP, or contact a model.
        marker = root / "mcp-started"
        sentinel = root / "sentinel.py"
        sentinel.write_text(f"from pathlib import Path\nPath({str(marker)!r}).touch()\n")
        run("mcp", "add", "sentinel", sys.executable, sentinel)
        before = set((root / "ion").rglob("*.sqlite"))
        calls = len(requests)
        for args, piped, error in [
            (["-p"], b"", b"needs text"),
            (["run"], b" \n", b"needs text"),
            (["run"], b"\xff", b"not UTF-8"),
            (["-p"], b"x" * (8 * 1024 * 1024 + 1), b"exceeds 8 MiB"),
            (["--model", "first/vendor/model", "auth"], b"", b"does not apply"),
            (["auth", "--model", "first/vendor/model"], b"", b"unexpected argument"),
            (["chat", "x" * (64 * 1024 + 1)], b"", b"exceeds 64 KiB"),
            (["unused prompt", "auth"], b"", b"put the prompt after"),
        ]:
            result = run("--cwd", workspace, *args, input=piped, check=False) if "auth" not in args else run(*args, input=piped, check=False)
            assert result.returncode != 0 and error in result.stderr, (args, result.stderr)
        assert set((root / "ion").rglob("*.sqlite")) == before
        assert not marker.exists()
        assert len(requests) == calls

        # Metadata never consumes stdin, and invalid option scopes fail before a
        # producer must close its pipe.
        assert run("auth", input=b"\xff").returncode == 0
        child = subprocess.Popen([BINARY, "--model", "first/vendor/model", "auth"],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, env=env)
        try:
            assert child.wait(timeout=5) == 2
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)
        for verb in ("auth", "models", "use", "login", "logout"):
            help_text = run(verb, "--help").stdout
            assert b"--model" not in help_text and b"--cwd" not in help_text
        assert b"openai-responses" in run("use", "--help").stdout
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()

print("CLI qualified models, stdin-only input, scoped help and pre-admission boundaries: OK")
