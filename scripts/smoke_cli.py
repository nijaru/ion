"""Real CLI grammar and literal stdin boundaries; loopback only, isolated state."""

import base64
import json
import os
import signal
import sqlite3
import subprocess
from contextlib import closing
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))
requests = []
batch_waiting = threading.Event()
batch_release = threading.Event()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        events = [
            {"choices": [{"index": 0, "delta": {"content": "CLI_OK"}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        prompt = requests[-1]["messages"][-1]["content"]
        if prompt == "BATCH_FAIL":
            payload = json.dumps({"error": {"message": "BATCH_FAILURE", "type": "invalid_request_error"}}).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        if prompt == "BATCH_WAIT":
            batch_waiting.set()
            assert batch_release.wait(5), "batch cancellation fixture was not released"
        if prompt == "!touch UNEXPECTED_BATCH_EFFECT":
            events[0]["choices"][0]["delta"]["content"] = "BATCH_LAST_OK"
        if prompt in ("default", "restore"):
            events[-1]["model"] = "served-alias"
            events[-1]["usage"] = {
                "prompt_tokens": 1017 if prompt == "default" else 0,
                "completion_tokens": 17 if prompt == "default" else 0,
                "prompt_tokens_details": {"cached_tokens": 555 if prompt == "default" else 0},
            }
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events)
        payload += b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            assert prompt == "BATCH_WAIT", "unexpected fixture disconnect"

    def log_message(self, *_args):
        pass


def exercise_doctor(root, base_env, endpoint):
    sandbox = root / "doctor"
    workspace = sandbox / "workspace"
    workspace.mkdir(parents=True)
    home = sandbox / "ion"
    home.mkdir()
    env = dict(base_env, HOME=str(sandbox / "home"), ION_HOME=str(home))

    def call(*args, expected=0):
        result = subprocess.run([BINARY, *map(str, args)], env=env, cwd=workspace,
                                capture_output=True, timeout=15)
        assert result.returncode == expected, (args, result.stdout, result.stderr)
        return result

    call("use", "fixture", "doctor-model", "--endpoint", endpoint, "--wire", "chat-completions")
    marker = sandbox / "UNEXPECTED_EXECUTION"
    program = [sys.executable, "-c", f"from pathlib import Path; Path({str(marker)!r}).touch()"]
    (home / "mcp.json").write_text(json.dumps({"servers": {
        "sentinel": {"command": program[0], "args": program[1:]}}}))
    (home / "commands.json").write_text(json.dumps({"commands": {
        "sentinel": {"description": "Sentinel", "program": program[0], "args": program[1:],
                     "share_with_model": False}}}))
    secret = "DOCTOR_PRIVATE_VALUE"
    (home / "settings.json").write_text(json.dumps({"shell": {
        "path": "/bin/sh", "setup": f"# {secret}\nprintf executed > '{marker}'"}}))

    def doctor(expected=0, terms=()):
        before = {p.relative_to(sandbox): p.read_bytes() for p in sandbox.rglob("*") if p.is_file()}
        calls = len(requests)
        result = call("--cwd", workspace, "doctor", expected=expected)
        text = (result.stdout + result.stderr).decode()
        assert all(term in text for term in terms), text
        assert secret not in text and endpoint not in text and "\x1b" not in text, text
        assert not marker.exists() and len(requests) == calls, text
        after = {p.relative_to(sandbox): p.read_bytes() for p in sandbox.rglob("*") if p.is_file()}
        assert before == after, "doctor changed local files or allocated a Session"
        return text

    doctor(terms=("ok\tmodel", "ok\tcredentials", "ok\tsettings", "ok\tresources",
                  "ok\tcommands", "ok\tmcp", "ok\tkeybindings", "Offline"))
    model = (home / "models.json").read_bytes()
    (home / "models.json").write_text(json.dumps({"custom": secret}))
    (home / "keybindings.json").write_text(json.dumps({"tui.select.down": "ctrl+n"}))
    doctor(1, ("fail\tmodel", "fail\tkeybindings", "ok\tsettings", "ok\tmcp", "line 1"))
    (home / "models.json").write_bytes(model)
    (home / "keybindings.json").unlink()
    (home / "settings.json").write_text(json.dumps({"shell": {"setup": [secret]}}))
    doctor(1, ("fail\tsettings", "ok\tmodel", "ok\tresources", "line 1"))
    (home / "settings.json").write_text(json.dumps({"search": {"fallbacks": [
        {"provider": "brave", "endpoint": "file:///" + secret}]}}))
    doctor(1, ("fail\tsettings", "ok\tmodel"))
    for fallbacks in (
        [{"provider": "brave"}, {"provider": "brave"}],
        [{"provider": "brave", "endpoint": "http://127.0.0.1/" + "x" * 8193}],
    ):
        (home / "settings.json").write_text(json.dumps({"search": {"fallbacks": fallbacks}}))
        doctor(1, ("fail\tsettings", "ok\tmodel"))
    themes = home / "themes"
    themes.mkdir()
    (home / "settings.json").write_text(json.dumps({"theme": "fixture"}))
    (themes / "fixture.json").write_text(json.dumps({"name": "fixture", "colors": {"code": "#9ad"}}))
    doctor(terms=("ok\ttheme", "ok\tsettings"))
    (themes / "fixture.json").write_text(json.dumps({"name": "fixture", "colors": {"code": secret}}))
    doctor(1, ("fail\ttheme", "ok\tsettings"))
    (home / "settings.json").write_text(json.dumps({"theme": {"bad": secret}}))
    doctor(1, ("fail\ttheme", "ok\tsettings", "line 1"))
    (home / "settings.json").unlink()
    os.mkfifo(home / "keybindings.json")
    doctor(1, ("fail\tkeybindings", "ok\tmodel", "ok\tmcp"))
    (home / "keybindings.json").unlink()
    project = workspace / ".ion"
    project.mkdir()
    (project / "mcp.json").write_text((home / "mcp.json").read_text())
    (project / "commands.json").write_text((home / "commands.json").read_text())
    skills = home / "skills" / "broken\x1b[2J"
    skills.mkdir(parents=True)
    (skills / "SKILL.md").write_text(f"---\nname: [{secret}]\n---\n")
    doctor(terms=("warn\tresources", "warn\tcommands", "warn\tmcp", "SKILL.md"))
    # Inert inspection must reject pipe/device config sources rather than block.
    for path in (home / "mcp.json", project / "mcp.json", home / "mcp-trust.json"):
        original = path.read_bytes() if path.exists() else None
        path.unlink(missing_ok=True)
        os.mkfifo(path)
        doctor(terms=("warn\tmcp", "ok\tmodel"))
        path.unlink()
        if original is not None:
            path.write_bytes(original)
    call("use", "fixture", "doctor-model", "--endpoint", endpoint, "--wire", "chat-completions",
         "--api-key-env", "DOCTOR_TEST_KEY")
    doctor(terms=("ok\tcredentials", "requires no API key"))
    # Use the actual bundled catalog, not an invented required-key custom route.
    route = next(line.split("\t")[0] for line in call("models", "openai").stdout.decode().splitlines()
                 if line.startswith("openai/"))
    call("use", *route.split("/", 1))
    doctor(1, ("ok\tmodel", "fail\tcredentials", "ion login"))
    env["OPENAI_API_KEY"] = secret
    doctor(terms=("ok\tcredentials", "environment credential present"))
    del env["OPENAI_API_KEY"]
    credentials = home / "credentials"
    credentials.mkdir(mode=0o700)
    key = credentials / "openai.key"
    key.write_text(secret)
    key.chmod(0o600)
    doctor(terms=("ok\tcredentials", "saved credential present"))
    key.chmod(0o644)
    doctor(1, ("fail\tcredentials", "permissions"))

    # A fresh, unconfigured operator home stays absent, even on failure.
    import shutil
    shutil.rmtree(home)
    doctor(1, ("fail\tmodel", "ok\tsettings", "ok\tkeybindings"))
    assert not home.exists()
    for args in [("doctor", "--json"), ("--model", "fixture/doctor-model", "doctor"),
                 ("doctor", "--session", "ignored.sqlite")]:
        call(*args, expected=2)
    print("Offline doctor: independent failures, safe diagnostics, trust and zero execution/writes: OK")


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

    def ended(database):
        with closing(sqlite3.connect(f"file:{database}?mode=ro", uri=True)) as connection:
            return [json.loads(row[0])["data"]["reason"] for row in connection.execute(
                "SELECT body FROM entries WHERE json_extract(body, '$.kind') = 'turn_ended' ORDER BY seq")]

    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        exercise_doctor(root, env, endpoint)
        run("use", "pictures", "model", "--endpoint", endpoint, "--wire", "chat-completions", "--images")
        for provider, model in (("first", "vendor/model"), ("second", "other")):
            run("use", provider, model, "--endpoint", endpoint, "--wire", "chat-completions")
        store = root / "ion" / "models.json"
        for provider, model in (("matches", "shorthand-20260101"),
                                ("matches", "shorthand-20261001"),
                                ("matches", "shorthand-latest"),
                                ("matches", "first/namespaced:high"),
                                ("matches", "twin"), ("second", "twin")):
            run("use", provider, model, "--endpoint", endpoint, "--wire", "chat-completions")
        run("use", "second", "other")
        saved_default = store.read_bytes()

        # Malformed but unused presentation data cannot block headless work or
        # leak into model context. Doctor validates it separately above.
        presentation = root / "ion" / "settings.json"
        presentation.write_text(json.dumps({"theme": {"bad": "CLI_PRIVATE_THEME_VALUE"}}))
        # Human patterns resolve once to canonical requests and Session facts.
        # They neither change the global default nor interpret colon IDs as effort.
        for args, expected in [
            (["--model", "shorthand"], "shorthand-latest"),
            (["--model", "MATCHES/SHORTHAND-2026"], "shorthand-20261001"),
            (["--model", "matches/shorthand-20260101"], "shorthand-20260101"),
            (["--provider", "matches", "--model", "first/namespaced:high"], "first/namespaced:high"),
        ]:
            database = root / "pattern.sqlite"
            run("--cwd", workspace, "--session", database, *args, "run", "MODEL_SHORTHAND_PROBE")
            assert requests[-1]["model"] == expected, requests[-1]
            assert "reasoning_effort" not in requests[-1], requests[-1]
            view = json.loads(run("inspect", "--session", database).stdout)
            assert view["last_model"] == {"provider": "matches", "model": expected}, view
            assert store.read_bytes() == saved_default
            assert "CLI_PRIVATE_THEME_VALUE" not in json.dumps(requests[-1])
        presentation.unlink()

        run("use", "matches", "shorthand")
        assert json.loads(store.read_bytes())["default"] == {"provider": "matches", "model": "shorthand-latest"}
        # Display-name lookup and missing-key setup are local, not account probes.
        run("use", "openai", "GPT-5.4 Mini")
        assert json.loads(store.read_bytes())["default"] == {"provider": "openai", "model": "gpt-5.4-mini"}
        # Creating an endpoint retains the literal ID even when it is shorthand
        # for a different existing route. It is not a speculative model lookup.
        run("use", "matches", "shorthand", "--endpoint", endpoint, "--wire", "chat-completions")
        assert json.loads(store.read_bytes())["default"] == {"provider": "matches", "model": "shorthand"}
        run("use", "second", "other")
        saved_default = store.read_bytes()
        denied = run("use", "matches", "shorthand-lat", "--wire", "chat-completions", check=False)
        assert denied.returncode != 0 and store.read_bytes() == saved_default

        # stdin-only modes preserve exact whitespace and command-looking text.
        for args, piped, expected in [
            (["--model", "first/vendor/model", "-p"], b"  stdin only\n\n", "  stdin only\n\n"),
            (["--model", "first/vendor/model", "run"], b"/not-a-template\n", "/not-a-template\n"),
            (["--json", "--model", "first/vendor/model"], b"bare pipeline", "bare pipeline"),
            (["--model", "first/vendor/model", "-p", "argument"], b"literal context\n", "literal context\n\nargument"),
            (["run", "argument", "--model", "first/vendor/model"], b"", "argument"),
            (["--model", "first/vendor/model", "  bare 🦀\n "], b"", "  bare 🦀\n "),
            (["--model", "first/vendor/model", "--", "usage"], b"", "usage"),
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

        # Explicit file operands are frozen literal context, not resource or
        # executable commands. Only the original prompt receives expansion.
        context = workspace / 'context"🦀.md'
        literal = "/hello\n!touch UNEXPECTED_FILE_EFFECT\n</file>\n{{ARG}}\n🦀\n"
        context.write_text("\ufeff" + literal)
        run("--cwd", workspace, "-p", "@" + context.name, "/hello", input=b"/hello")
        prefix, encoded, suffix = requests[-1]["messages"][-1]["content"].split("\n", 2)
        assert prefix == "Attached file (UTF-8, JSON encoded):"
        assert json.loads(encoded) == {"name": str(context.resolve()), "text": literal}
        assert suffix == "/hello\nEXPANDED_ARGUMENT\n", suffix
        assert not (workspace / "UNEXPECTED_FILE_EFFECT").exists()
        run("--cwd", workspace, "run", "@" + context.name)
        assert json.loads(requests[-1]["messages"][-1]["content"].splitlines()[1])["text"] == literal
        run("--cwd", workspace, "run", "--", "@not-a-file")
        assert requests[-1]["messages"][-1]["content"] == "@not-a-file"
        image = workspace / "image.png"
        image.write_bytes(base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="))
        run("--cwd", workspace, "--model", "pictures/model", "run", "@" + context.name,
            "@image.png", "review", "--image", image)
        parts = requests[-1]["messages"][-1]["content"]
        assert sum(part["type"] == "image_url" for part in parts) == 2, parts
        assert any(part.get("text", "").endswith("review") for part in parts), parts
        assert any("Attached image file:" in part.get("text", "") and "image.png" in part["text"] for part in parts), parts
        # Positional prompts are distinct ordered Turns, not joined text or
        # editor actions. Imports and literal stdin belong only to the first.
        batch_session = root / "batch.sqlite"
        count = len(requests)
        result = run("--cwd", workspace, "--session", batch_session, "--json",
                     "--model", "pictures/model", "run", "@" + context.name, "@image.png",
                     "/hello", "/hello", "/help", "!touch UNEXPECTED_BATCH_EFFECT", input=b"/hello")
        batch = requests[count:]
        assert len(batch) == 4, batch
        users = [message["content"] for message in batch[-1]["messages"] if message["role"] == "user"]
        first_text = next(part["text"] for part in users[0] if part["type"] == "text")
        assert json.loads(first_text.splitlines()[1])["text"] == literal
        assert first_text.endswith("/hello\nEXPANDED_ARGUMENT\n"), first_text
        assert sum(part["type"] == "image_url" for part in users[0]) == 1
        assert users[1:] == ["EXPANDED_ARGUMENT\n", "/help", "!touch UNEXPECTED_BATCH_EFFECT"], users[1:]
        assert not (workspace / "UNEXPECTED_BATCH_EFFECT").exists()
        records = [json.loads(line) for line in result.stdout.splitlines()]
        assert [record for record in records if record["type"] == "final"] == [{"type": "final", "text": "BATCH_LAST_OK"}], records
        assert records[-1] == {"type": "run_end", "status": "completed"}
        assert ended(batch_session) == ["completed"] * 4
        count = len(requests)
        run("--cwd", workspace, "run", "--", "@literal-first", "usage", "@literal-last")
        assert [request["messages"][-1]["content"] for request in requests[count:]] == ["@literal-first", "usage", "@literal-last"]

        # Failure after partial success remains failure; Stop/failure discard
        # the unstarted tail rather than retrying it or hiding the first fault.
        for prefix in ([], ["successful first"]):
            database = root / f"failed-{len(prefix)}.sqlite"
            count = len(requests)
            result = run("--cwd", workspace, "--session", database, "--json", "run",
                         *prefix, "BATCH_FAIL", "NEVER_RUN", check=False)
            assert result.returncode != 0
            assert len(requests) == count + len(prefix) + 1
            records = [json.loads(line) for line in result.stdout.splitlines()]
            assert records[-1]["status"] == "failed" and "BATCH_FAILURE" in records[-1]["error"], records
            assert not any(record["type"] == "final" for record in records)
            reasons = ended(database)
            assert reasons[:-1] == ["completed"] * len(prefix) and "failed" in reasons[-1], reasons
        database = root / "cancelled-batch.sqlite"
        count = len(requests)
        child = subprocess.Popen([BINARY, "--cwd", workspace, "--session", database, "--json",
                                  "run", "BATCH_WAIT", "NEVER_RUN"], env=env,
                                 stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            assert batch_waiting.wait(5), "first batch Turn did not reach fixture"
            child.send_signal(signal.SIGINT)
            output, errors = child.communicate(timeout=5)
            assert child.returncode != 0, errors
            assert json.loads(output.splitlines()[-1])["status"] == "cancelled", output
            assert len(requests) == count + 1 and ended(database) == ["cancelled"]
        finally:
            batch_release.set()
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=5)

        file_session = root / "files.sqlite"
        run("--cwd", workspace, "--session", file_session, "run", "@" + context.name, "first")
        run("--session", file_session, "run", "@" + context.name, "second")
        assert json.loads(requests[-1]["messages"][-1]["content"].splitlines()[1])["name"] == str(context.resolve())

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
        invalid = []
        for name, data in (("bad-utf8", b"\xff"), ("binary", b"a\0b"),
                           ("large", b"x" * (8 * 1024 * 1024 + 1))):
            path = workspace / name
            path.write_bytes(data)
            invalid.append(path)
        pipe = workspace / "fifo"
        os.mkfifo(pipe)
        invalid.extend([pipe, workspace, workspace / "missing", image,
                        workspace / "missing\x1b]52;c;QUJD\x07"])
        before = set((root / "ion").rglob("*.sqlite"))
        calls = len(requests)
        for args, diagnostic in [
            (["--model", "no-known-pattern"], b"no known model matches"),
            (["--model", "twin"], b"ambiguous model pattern"),
            (["--model", "first/namespaced:high"], b"within provider"),
            (["--provider", "first", "--model", "other"], b"within provider"),
        ]:
            database = root / "denied-pattern.sqlite"
            denied = run("--cwd", workspace, "--session", database, *args, "run", "not admitted", check=False)
            assert denied.returncode != 0 and diagnostic in denied.stderr, denied.stderr
            assert not database.exists() and not marker.exists() and len(requests) == calls
            assert store.read_bytes() == saved_default
            if "twin" in args:
                assert b"matches/twin" in denied.stderr and b"second/twin" in denied.stderr
        near_limit = workspace / "near-limit"
        near_limit.write_bytes(b"x" * (7 * 1024 * 1024))
        aggregate_session = root / "aggregate.sqlite"
        denied = run("--cwd", workspace, "--session", aggregate_session, "run", "@" + str(near_limit),
                     input=b"y" * (2 * 1024 * 1024), check=False)
        assert denied.returncode != 0, denied
        assert not aggregate_session.exists() and not marker.exists(), "aggregate input preflight admitted setup effects"
        for tail in ("/hello '", ""):
            database = root / "invalid-later.sqlite"
            denied = run("--cwd", workspace, "--session", database, "run", "valid first", tail, check=False)
            assert denied.returncode != 0 and not database.exists()
            assert not marker.exists() and len(requests) == calls, "later prompt preflight admitted effects"
        (prompts / "bulk.md").write_text("b" * (1024 * 1024))
        for label, operands in (("capacity", ["small"] * 65), ("retained-bytes", ["/bulk"] * 11)):
            database = root / f"batch-{label}.sqlite"
            denied = run("--cwd", workspace, "--session", database, "run", *operands, check=False)
            assert denied.returncode != 0 and not database.exists() and not marker.exists(), (label, "batch bounds admitted setup effects", denied.stderr)
            assert len(requests) == calls
        for path in invalid:
            result = run("--cwd", workspace, "run", "@" + str(path), check=False)
            assert result.returncode != 0, path
            assert b"\x1b" not in result.stderr and b"\x07" not in result.stderr, result.stderr
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

        # Usage is passive saved-history inspection, even with no configured
        # model and an invalid unused keymap. It cannot start MCP or read stdin.
        store.rename(store.with_suffix(".saved"))
        (root / "ion/keybindings.json").write_text("invalid unused keymap")
        report = run("usage", "--session", session, input=b"\xff").stdout.decode()
        assert "Input tokens: 1017 reported; 1 observation(s) unreported" in report, report
        assert "Output tokens: 17 reported; 1 observation(s) unreported" in report, report
        assert "Total tokens: 1034 reported (incomplete)" in report, report
        assert "Cache read tokens: 555 reported; 1 observation(s) unreported" in report, report
        assert "Cache write tokens: unknown" in report, report
        assert "first/vendor/model" in report and "second/other" in report, report
        assert "provider reports served-alias" in report and "Cost: unknown" in report, report
        assert json.loads(run("inspect", "--session", session).stdout) == view
        latest = run("--cwd", workspace, "--continue", "usage").stdout.decode()
        assert "Total tokens: unknown" in latest and "Cost: unknown" in latest, latest
        assert len(requests) == calls and not marker.exists()
        absent = root / "absent.sqlite"
        for args in (("usage",), ("usage", "--session", absent),
                     ("--model", "first/vendor/model", "usage", "--session", session)):
            assert run(*args, check=False).returncode != 0, args
        assert not absent.exists() and len(requests) == calls and not marker.exists()

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
        batch_release.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        assert not thread.is_alive()

print("CLI model patterns/canonical authority, ordered Turns, first-only context, failure/Stop and pre-admission boundaries: OK")
