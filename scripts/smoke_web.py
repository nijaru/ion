#!/usr/bin/env python3
"""Credential-free built coding web retrieval, composition and source oracle."""
import hashlib
import json
import os
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BINARY = Path(os.environ.get("ION_SMOKE_BIN", ROOT / "target/debug/ion"))
DOCUMENT = b"<h1>WEB_FETCH_DOCUMENT</h1><p>Reference <a href='/reference'>link</a>.</p><script>UNEXPECTED_SCRIPT</script>"
DIGEST = hashlib.sha256(DOCUMENT).hexdigest()
web_calls = []
failures = []
steps = {}
search_calls = []
SEARCH_QUERY = "ion current documentation"


class Web(BaseHTTPRequestHandler):
    def do_GET(self):
        web_calls.append((self.path, dict(self.headers)))
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", f"http://localhost:{self.server.server_port}/document")
            self.send_header("Set-Cookie", "private=must-not-carry")
            body = b""
        else:
            self.send_response(200)
            body = DOCUMENT if self.path == "/document" else b"PDF not decoded"
            self.send_header("Content-Type", "text/html; charset=utf-8" if self.path == "/document" else "application/pdf")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path in {"/proxy/v1/responses", "/search/exa", "/search/deepseek"}:
                self.search_response(body)
                return
            prompt = next(m["content"] for m in reversed(body["messages"]) if m["role"] == "user")
            if prompt.startswith("SEARCH_"):
                self.search_step(body, prompt)
                return
            step = steps.get(prompt, 0)
            steps[prompt] = step + 1
            url = f"http://127.0.0.1:{web.server_port}"
            args = {"url": url + "/redirect", "max_bytes": 16}
            if step == 0:
                names = {t["function"]["name"] for t in body["tools"]}
                assert ("web_fetch" in names) == (prompt != "WEB_EXCLUDED"), names
                if prompt == "WEB_READONLY":
                    assert "exec" not in names and "write" not in names, names
                if prompt in {"WEB_CODE", "WEB_EXCLUDED"}:
                    code = "const r=await tools.web_fetch(" + json.dumps(args) + ");"
                    if prompt == "WEB_CODE":
                        code += (
                            "if(r.is_error||!r.value.untrusted||!r.value.truncated)throw new Error('bad retrieval');"
                            "const p=await tools.web_fetch(" + json.dumps({"url": url + "/pdf"}) + ");"
                            "const i=await tools.web_fetch({url:'file:///etc/passwd'});"
                            "const full=await tools.read({path:r.value.full_path});"
                            "text({source:r.value.source,content:full.value.content,pdf:p.value.error.kind,invalid:i.value.error.kind});"
                        )
                    self.calls([("code", "code_mode", {"code": code})])
                else:
                    self.calls([("fetch", "web_fetch", args), ("pdf", "web_fetch", {"url": url + "/pdf"}), ("scheme", "web_fetch", {"url": "file:///etc/passwd"})])
            elif step == 1 and prompt not in {"WEB_CODE", "WEB_EXCLUDED"}:
                results = [json.loads(m["content"]) for m in body["messages"] if m["role"] == "tool"]
                fetched = results[0]["result"]
                assert fetched["untrusted"] and fetched["truncated"], fetched
                assert fetched["source"]["body_sha256"] == DIGEST, fetched
                assert results[1]["result"]["error"]["kind"] == "unsupported_content", results
                assert results[2]["result"]["error"]["kind"] == "invalid_url", results
                self.calls([("snapshot", "read", {"path": fetched["full_path"]})])
            else:
                last = json.loads(body["messages"][-1]["content"])
                if prompt == "WEB_EXCLUDED":
                    assert last["is_error"], last
                else:
                    assert not last["is_error"], last
                    assert "WEB_FETCH_DOCUMENT" in json.dumps(last), last
                    assert "UNEXPECTED_SCRIPT" not in json.dumps(last), last
                    if prompt == "WEB_CODE":
                        assert DIGEST in json.dumps(last), last
                self.events({"content": "WEB_FETCH_OK"}, "stop")
        except Exception as error:
            failures.append(repr(error))
            self.send_error(500, str(error))

    def search_response(self, body):
        if self.path == "/proxy/v1/responses":
            assert self.headers["Authorization"] == "Bearer model-secret"
            assert body["model"] == "search-fixture" and body["input"] == SEARCH_QUERY, body
            assert body["store"] is False and body["max_tool_calls"] == 1, body
            assert "messages" not in body and "PARENT_PRIVATE_CONTEXT" not in json.dumps(body), body
            result = {"status": "completed", "output": [{"type": "web_search_call", "status": "completed", "action": {"sources": [self.search_source()]}}], "usage": {"input_tokens": 13, "output_tokens": 7}}
        else:
            assert self.headers["Authorization"] == "Bearer search-secret"
            assert body["query"] == SEARCH_QUERY, body
            if self.path == "/search/exa":
                assert body["type"] == "auto" and body["numResults"] == 2, body
            else:
                assert body["count"] == 2 and body["need_content"] is False, body
            result = {"results": [self.search_source()]}
        self.search_json(result)

    def do_GET(self):
        try:
            from urllib.parse import parse_qs, urlsplit
            target = urlsplit(self.path)
            assert target.path == "/search/brave", self.path
            assert self.headers["X-Subscription-Token"] == "search-secret"
            assert "Authorization" not in self.headers
            assert parse_qs(target.query) == {"q": [SEARCH_QUERY], "count": ["2"]}
            self.search_json({"web": {"results": [self.search_source()]}})
        except Exception as error:
            failures.append(repr(error))
            self.send_error(500)

    def search_source(self):
        return {"url": f"http://127.0.0.1:{web.server_port}/document", "title": "Fixture documentation", "summary": "Independent source", "description": "Independent source", "highlights": ["Independent source"]}

    def search_json(self, result):
        wire = json.dumps(result).encode()
        search_calls.append((self.path, hashlib.sha256(wire).hexdigest()))
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(wire)))
        self.end_headers()
        self.wfile.write(wire)

    def search_step(self, body, prompt):
        step = steps.get(prompt, 0)
        steps[prompt] = step + 1
        args = {"query": SEARCH_QUERY, "max_results": 2}
        composed = prompt in {"SEARCH_CODE_PARENT_PRIVATE_CONTEXT", "SEARCH_EXCLUDED_PARENT_PRIVATE_CONTEXT"}
        excluded = prompt == "SEARCH_EXCLUDED_PARENT_PRIVATE_CONTEXT"
        if step == 0:
            names = {t["function"]["name"] for t in body["tools"]}
            assert ("web_search" in names) == (not excluded), names
            if prompt == "SEARCH_READONLY_PARENT_PRIVATE_CONTEXT":
                assert "exec" not in names and "write" not in names, names
            if composed:
                code = "const s=await tools.web_search(" + json.dumps(args) + ");"
                if not excluded:
                    code += "if(s.is_error||!s.value.untrusted)throw new Error('search failed');const f=await tools.web_fetch({url:s.value.results[0].url});if(f.is_error)throw new Error('fetch failed');text({search:s.value,content:f.value.content});"
                self.calls([("search-code", "code_mode", {"code": code})])
            else:
                self.calls([("search", "web_search", args)])
        elif step == 1 and not composed:
            output = json.loads(body["messages"][-1]["content"])
            assert not output["is_error"], output
            result = output["result"]
            assert result["untrusted"] and result["results"][0]["url"] == self.search_source()["url"], result
            self.calls([("searched-page", "web_fetch", {"url": result["results"][0]["url"]})])
        else:
            output = json.loads(body["messages"][-1]["content"])
            assert output["is_error"] == excluded, output
            if not excluded:
                assert "WEB_FETCH_DOCUMENT" in json.dumps(output), output
                assert "UNEXPECTED_SCRIPT" not in json.dumps(output), output
            self.events({"content": "SEARCH_FETCH_OK"}, "stop")

    def calls(self, calls):
        self.events({"tool_calls": [{"index": i, "id": ident, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}} for i, (ident, name, arguments) in enumerate(calls)]}, "tool_calls")

    def events(self, delta, finish):
        events = [{"id": "web", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}, {"id": "web", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}]
        wire = "".join("data: " + json.dumps(e) + "\n\n" for e in events) + "data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(wire.encode())))
        self.end_headers()
        self.wfile.write(wire.encode())

    def log_message(self, *_):
        pass


if __name__ == "__main__":
    web = ThreadingHTTPServer(("127.0.0.1", 0), Web)
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threads = [threading.Thread(target=s.serve_forever) for s in (web, provider)]
    for thread in threads:
        thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="ion-web-smoke-") as temporary:
            root = Path(temporary)
            for part in ("home", "tmp", "work"):
                (root / part).mkdir()
            # No user credentials, browser state, proxy routes or shell startup.
            env = {k: os.environ[k] for k in ("PATH", "SYSTEMROOT") if k in os.environ}
            env.update(HOME=str(root / "home"), ION_HOME=str(root / "ion"), TMPDIR=str(root / "tmp"), XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"), LANG="C.UTF-8")

            def run(*args):
                return subprocess.run([str(BINARY), "--cwd", str(root / "work"), *args], env=env, capture_output=True, text=True, timeout=45, check=True)

            run("use", "fixture", "web", "--endpoint", f"http://127.0.0.1:{provider.server_port}/v1", "--wire", "chat-completions")
            for prompt, options in [("WEB_DIRECT", []), ("WEB_READONLY", ["--read-only"]), ("WEB_CODE", ["--code-mode"]), ("WEB_EXCLUDED", ["--code-mode", "--exclude-tools", "web_fetch"])]:
                before = len(web_calls)
                result = run(*options, "run", prompt)
                assert result.stdout.strip() == "WEB_FETCH_OK", result
                assert not failures, failures
                assert len(web_calls) - before == (0 if prompt == "WEB_EXCLUDED" else 3), web_calls
                view = json.loads(run("--continue", "inspect").stdout)
                assert view["unfinished_turn"] is None, view
                if prompt == "WEB_EXCLUDED":
                    assert not any(e["kind"] == "child_tool_admitted" for e in view["entries"]), view
                    continue
                outputs = []
                for entry in view["entries"]:
                    if entry["kind"] == "tool_result":
                        outcome = entry["data"]["result"]["outcome"]
                    elif entry["kind"] == "child_tool_result":
                        outcome = entry["data"]["outcome"]
                    else:
                        continue
                    if outcome["state"] == "observed":
                        outputs.append(outcome["output"]["value"])
                fetched = next(o for o in outputs if o.get("source", {}).get("body_sha256") == DIGEST)
                assert fetched["source"]["final_url"] == f"http://localhost:{web.server_port}/document", fetched
                assert fetched["source"]["body_bytes"] == len(DOCUMENT), fetched
                snapshot = Path(fetched["full_path"])
                assert snapshot.is_relative_to(root / "tmp"), snapshot
                text = snapshot.read_text()
                assert "WEB_FETCH_DOCUMENT" in text and "UNEXPECTED_SCRIPT" not in text, text
                assert f"http://localhost:{web.server_port}/reference" in text, text
            env.update(MODEL_TOKEN="model-secret", SEARCH_TOKEN="search-secret")
            cases = [
                ("SEARCH_NATIVE", "openai", []),
                ("SEARCH_READONLY", "openai", ["--read-only"]),
                ("SEARCH_CODE", "openai", ["--code-mode"]),
                ("SEARCH_EXCLUDED", "openai", ["--code-mode", "--exclude-tools", "web_search"]),
                ("SEARCH_EXA", "fixture", []),
                ("SEARCH_DEEPSEEK", "fixture", []),
                ("SEARCH_BRAVE", "fixture", []),
            ]
            for mode, active, options in cases:
                backend = mode.removeprefix("SEARCH_").lower()
                fallback = [{"provider": backend, "endpoint": f"http://127.0.0.1:{provider.server_port}/search/{backend}", "api_key_env": "SEARCH_TOKEN"}] if active == "fixture" else []
                (root / "ion/settings.json").write_text(json.dumps({"search": {"fallbacks": fallback}}))
                run("use", active, "search-fixture", "--endpoint", f"http://127.0.0.1:{provider.server_port}/proxy/v1", "--wire", "chat-completions", "--api-key-env", "MODEL_TOKEN")
                before_search, before_web = len(search_calls), len(web_calls)
                result = run(*options, "run", mode + "_PARENT_PRIVATE_CONTEXT")
                assert result.stdout.strip() == "SEARCH_FETCH_OK", result
                assert not failures, failures
                count = 0 if mode == "SEARCH_EXCLUDED" else 1
                assert len(search_calls) - before_search == count, search_calls
                assert len(web_calls) - before_web == count, web_calls
                view = json.loads(run("--continue", "inspect").stdout)
                assert view["unfinished_turn"] is None, view
                outputs = []
                for entry in view["entries"]:
                    if entry["kind"] == "tool_result":
                        outcome = entry["data"]["result"]["outcome"]
                    elif entry["kind"] == "child_tool_result":
                        outcome = entry["data"]["outcome"]
                    else:
                        continue
                    if outcome["state"] == "observed":
                        outputs.append(outcome["output"]["value"])
                if mode == "SEARCH_EXCLUDED":
                    assert not any(e["kind"] == "child_tool_admitted" for e in view["entries"]), view
                else:
                    observed = next(v for v in outputs if "results" in v)
                    assert observed["source"]["body_sha256"] == search_calls[-1][1], observed
                    assert observed["source"]["backend"] == ("openai" if active == "openai" else backend), observed
                    assert observed["source"]["native"] == (active == "openai"), observed
                    assert observed["results"][0]["url"] == f"http://127.0.0.1:{web.server_port}/document", observed
                    assert "model-secret" not in json.dumps(observed) and "search-secret" not in json.dumps(observed), observed
            for _, headers in web_calls:
                assert not ({key.lower() for key in headers} & {"cookie", "authorization", "proxy-authorization", "referer"}), headers
            print("Ion fetch and provider-first search→fetch: default/read-only/Code Mode/exclusion, native profile/auth, Exa/DeepSeek/Brave and persisted sources: OK")
    finally:
        for server in (web, provider):
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=5)
            assert not thread.is_alive()
