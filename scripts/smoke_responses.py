"""Qualify stateless Responses replay and effect/retry fences through the CLI."""

import base64
import hashlib
import json
import os
import shlex
import struct
import subprocess
import sys
import tempfile
import threading
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = {}
errors = []
coding = {}
overflow = {}


def event(kind, **fields):
    return f"event: {kind}\ndata: {json.dumps({'type': kind, **fields})}\n\n".encode()


def call(arguments, *, status="completed", name="write"):
    return dict(type="function_call", id="fc_1", call_id="call_1", name=name,
                arguments=json.dumps(arguments) if not isinstance(arguments, str) else arguments,
                status=status)


def message(identity, text, *, phase="final_answer", refusal=False):
    part = dict(type="refusal", refusal=text) if refusal else dict(type="output_text", text=text, annotations=[])
    return dict(type="message", id=identity, role="assistant", status="completed", phase=phase, content=[part])


def finish(output, *, incomplete=False, output_tokens=8):
    response = dict(id="resp_1", model="mock", status="incomplete" if incomplete else "completed", output=output,
                    usage=dict(input_tokens=10, output_tokens=output_tokens, total_tokens=10 + output_tokens,
                               input_tokens_details=dict(cached_tokens=6), output_tokens_details=dict(reasoning_tokens=3)))
    if incomplete:
        response["incomplete_details"] = dict(reason="max_output_tokens")
    return event("response.incomplete" if incomplete else "response.completed", response=response)


def failed(output, usage, *, code="server_error"):
    return event("response.failed", response=dict(id="resp_1", status="failed", output=output,
                 usage=usage, error=dict(code=code, message="fixture failure")))


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
            history = requests.setdefault(self.path, [])
            history.append(body)
            step = len(history)
            if (self.path == "/coding/v1/responses" and coding["phase"] == "summary") or (self.path in overflow and step == 4):
                assert "tools" not in body
            else:
                assert all(tool["type"] == "function" and tool["strict"] is False for tool in body["tools"])
            if self.path == "/v1/responses":
                payload = self.main_response(body, step)
            elif self.path == "/blocks/v1/responses":
                items = [message("msg_a", "Aa", phase="commentary"), message("msg_b", "B")]
                if step == 1:
                    payload = b""
                    for index, item in enumerate(items):
                        initial = {**item, "status": "in_progress", "content": []}
                        payload += event("response.output_item.added", output_index=index, item=initial)
                    for index, delta in [(0, "A"), (1, "B"), (0, "a")]:
                        payload += event("response.output_text.delta", output_index=index, item_id=items[index]["id"], content_index=0, delta=delta)
                    payload += finish(items)
                else:
                    assert step == 2
                    assert [item for item in body["input"] if item.get("id") in ("msg_a", "msg_b")] == items
                    payload = finish([message("msg_blocks_reopen", "BLOCKS_REOPENED")])
            elif self.path == "/coding/v1/responses":
                payload = self.coding_response(body, step)
            elif self.path in overflow:
                payload = self.overflow_response(body, step)
            elif self.path == "/retry/v1/responses":
                if step == 1:
                    prefix = event("response.created", response=dict(id="resp_1", usage=dict(input_tokens=11, output_tokens=2, input_tokens_details=dict(cached_tokens=0))))
                    payload = prefix + failed([], dict(input_tokens=17, output_tokens=0))
                else:
                    payload = finish([message("msg_retry", "RETRIED")])
            elif self.path == "/post-output/v1/responses":
                # No earlier delta: the failed snapshot itself must fence retry.
                payload = failed([call(dict(path="forbidden.txt", content="NEVER"))],
                                 dict(input_tokens=23, output_tokens=5, input_tokens_details=dict(cached_tokens=7)))
            elif self.path == "/namespace/v1/responses":
                foreign = call(dict(path="namespace-forbidden.txt", content="NEVER"))
                foreign["namespace"] = "foreign"
                payload = finish([foreign]) if step == 1 else finish([message("msg_namespace", "WRONG_LOCAL_TOOL_EXECUTED")])
            elif self.path == "/truncated/v1/responses":
                if step == 1:
                    payload = finish([call('{"path":"forbidden.txt"', status="incomplete")], incomplete=True, output_tokens=body["max_output_tokens"])
                else:
                    retained = [item for item in body["input"] if item["type"] == "function_call"]
                    assert retained[0]["arguments"] == '{"path":"forbidden.txt"'
                    results = [item for item in body["input"] if item["type"] == "function_call_output"]
                    assert all(json.loads(result["output"])["is_error"] is True for result in results)
                    if step == 2:
                        # Valid JSON is still not execution authority when the
                        # response is truncated, even with a completed call item.
                        valid = call(dict(path="valid-truncated.txt", content="NEVER"))
                        valid.update(id="fc_valid", call_id="call_valid")
                        payload = finish([valid], incomplete=True, output_tokens=body["max_output_tokens"])
                    else:
                        assert step == 3 and len(results) == 2
                        assert json.loads(retained[1]["arguments"])["path"] == "valid-truncated.txt"
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

    def coding_response(self, body, step):
        # This controlled provider chooses the actions, not the agent runtime.
        phase = coding["phase"]
        if phase == "summary":
            assert step == 9
            text = json.dumps(body["input"])
            assert "CODING_CHECK_FAILED" in text
            assert "CODING_CIPHER" not in text, "opaque continuation leaked into summary prose"
            return finish([message("msg_coding_summary", "REPAIR_SUMMARY: answer.py now returns 2; the failed check and stale edit were resolved; the check passed.")])
        if phase == "reopen":
            assert step == 10
            text = json.dumps(body["input"])
            assert "REPAIR_SUMMARY" in text
            assert "CODING_CHECK_FAILED" not in text, "compacted raw failure remained in working context"
            assert not any(item["type"] == "function_call" for item in body["input"])
            return finish([message("msg_coding_reopen", "REPAIR_REOPENED_WITHOUT_REPLAY")])
        if phase == "padding":
            assert step == 8
            results = {item["call_id"]: json.loads(item["output"])["is_error"] for item in body["input"] if item["type"] == "function_call_output"}
            assert results == {f"call_coding_{index}": failed for index, failed in enumerate([True, False, True, False, False, False], 1)}
            return finish([message("msg_coding_padding", "CODING_PADDING_COMPLETE")])
        assert phase == "repair" and 1 <= step <= 7
        inputs = body["input"]
        if step > 1:
            assert next(item for item in inputs if item.get("id") == "rs_coding") == dict(type="reasoning", id="rs_coding", summary=[], encrypted_content="CODING_CIPHER")
        def result(previous):
            output = next(item for item in inputs if item["type"] == "function_call_output" and item["call_id"] == f"call_coding_{previous}")
            envelope = json.loads(output["output"])
            assert isinstance(envelope["is_error"], bool)
            payload = envelope["result"]
            if "artifacts" in payload:
                assert "entry" in payload
                payload = payload["output"]
            return envelope["is_error"], payload
        if step == 1:
            name, arguments = "exec", dict(command=coding["command"], timeout_ms=5000)
        elif step == 2:
            failed, output = result(1)
            assert failed and output["exit_code"] == 1 and "CODING_CHECK_FAILED" in output["stdout"]
            assert coding["counter"].read_text() == "0\n"
            name, arguments = "read", dict(path="answer.py")
        elif step in (3, 5):
            failed, output = result(step - 1)
            expected = coding["initial"] if step == 3 else coding["before"]
            assert not failed and output["content"] == expected
            digest = output["base_digest"]
            assert digest == hashlib.sha256(expected.encode()).hexdigest()
            if step == 3:
                # An independent writer changes the file after the committed read.
                # The model then issues its edit with that real, now-stale digest.
                coding["source"].write_text(coding["before"])
            name, arguments = "edit", dict(path="answer.py", base_digest=digest,
                edits=[dict(old_text="return 0", new_text="return 2")])
        elif step == 4:
            failed, output = result(3)
            assert failed and "base_digest does not match" in output["error"]
            assert coding["source"].read_text() == coding["before"], "rejected edit changed the file"
            name, arguments = "read", dict(path="answer.py")
        elif step == 6:
            failed, output = result(5)
            assert not failed and output["new_digest"] == hashlib.sha256(coding["after"].encode()).hexdigest()
            assert coding["source"].read_text() == coding["after"]
            assert coding["counter"].read_text() == "0\n"
            name, arguments = "exec", dict(command=coding["command"], timeout_ms=5000)
        else:
            failed, output = result(6)
            assert not failed and output["exit_code"] == 0 and "CODING_CHECK_PASSED" in output["stdout"]
            assert coding["counter"].read_text() == "0\n2\n"
            return finish([message("msg_coding_final", "REPAIR_VERIFIED")])
        invocation = call(arguments, name=name)
        invocation.update(id=f"fc_coding_{step}", call_id=f"call_coding_{step}")
        output = [invocation]
        if step == 1:
            output.insert(0, dict(type="reasoning", id="rs_coding", summary=[], encrypted_content="CODING_CIPHER"))
        return finish(output)

    def overflow_response(self, body, step):
        fixture = overflow[self.path]
        if step == 1:
            invocation = call(dict(command=fixture["command"]), name="exec")
            invocation.update(id="fc_overflow_seed", call_id="call_overflow_seed")
            return finish([dict(type="reasoning", id="rs_overflow", summary=[], encrypted_content="OVERFLOW_CIPHER"), invocation])
        if step == 2:
            result = next(item for item in body["input"] if item["type"] == "function_call_output")
            result = json.loads(result["output"])
            assert not result["is_error"] and fixture["counter"].read_text() == "once\n"
            return finish([message("msg_overflow_seed", "SEED_COMPLETE")])
        if step == 3:
            assert "OVERFLOW_CIPHER" in json.dumps(body["input"])
            assert any(item["type"] == "function_call_output" for item in body["input"])
            output = []
            if fixture["mode"] == "observed":
                # The failed terminal snapshot itself is output observation;
                # a complete native call is still not execution authority.
                invocation = call(dict(command=fixture["forbidden_command"]), name="exec")
                invocation.update(id="fc_overflow_forbidden", call_id="call_overflow_forbidden")
                output = [invocation]
            return failed(output, dict(input_tokens=31, output_tokens=5 if output else 0), code="context_length_exceeded")
        if step == 4:
            assert fixture["mode"] != "observed", "compacted after observed output"
            assert "tools" not in body
            summary_input = json.dumps(body["input"])
            assert "OVERFLOW_CIPHER" not in summary_input, "opaque reasoning entered summary input"
            records = [json.loads(line) for item in body["input"] if item["type"] == "message"
                       for part in item["content"] if part["type"] == "input_text"
                       for line in part["text"].splitlines() if line.startswith("{")]
            results = [part["ToolResult"] for record in records for part in record["content"] if "ToolResult" in part]
            assert len(results) == 1 and results[0]["call_id"] == "call_overflow_seed" and not results[0]["is_error"]
            observed = results[0]["result"]["Full"]
            observed = observed.get("output", observed)
            assert observed["exit_code"] == 0 and observed["stdout"] == "PRESSURE_EFFECT_OBSERVED\n"
            return finish([message("msg_overflow_summary", "PRESSURE_RECOVERY_SUMMARY: the counter already ran once.")])
        assert step == 5 and fixture["mode"] != "observed", (self.path, step)
        assert "PRESSURE_RECOVERY_SUMMARY" in json.dumps(body["input"])
        assert "OVERFLOW_CIPHER" not in json.dumps(body["input"])
        assert not any(item["type"] in ("function_call", "function_call_output") for item in body["input"]), "old effect stayed in recovered working context"
        assert fixture["counter"].read_text() == "once\n"
        if fixture["mode"] == "twice":
            return failed([], dict(input_tokens=19, output_tokens=0), code="context_length_exceeded")
        return finish([message("msg_overflow_recovered", "PRESSURE_RECOVERED")])

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

    def select(prefix="", *args):
        run(env, "use", "responses-fixture", "mock", "--endpoint", base + prefix + "/v1", "--wire", "openai-responses", "--images", "--api-key-env", "RESPONSES_FIXTURE_KEY", *args)

    def submit(name, prompt, *args, success=True):
        return run(env, "--json", "--cwd", workspace, "--session", work / f"{name}.sqlite", *args, "run", prompt, success=success)

    def inspect(name):
        result = subprocess.run([binary, "--cwd", workspace, "--session", work / f"{name}.sqlite", "inspect"], env=env, capture_output=True, text=True, check=True, timeout=20)
        return json.loads(result.stdout)["entries"]

    try:
        select()
        first = submit("main", "Create created.txt and read this image.", "--image", "red.png")
        assert first[-1] == dict(type="run_end", status="completed"), first
        assert (workspace / "created.txt").read_text() == "INDEPENDENT_EFFECT\n"
        reopened = submit("main", "Continue")
        assert reopened[-1] == dict(type="run_end", status="completed"), reopened
        assert len(requests["/v1/responses"]) == 4
        entries = inspect("main")
        replay = next(entry["data"]["message"]["provider_replay"] for entry in entries
                      if entry["kind"] == "assistant" and entry["data"]["message"]["provider_replay"])
        assert replay["data"]["output"][0]["encrypted_content"] == "TERMINAL_CIPHER"
        assert not any("TERMINAL_CIPHER" in item.get("text", "") for item in first), "ciphertext entered human progress"

        select("/blocks")
        blocks = submit("blocks", "Keep interleaved text blocks distinct")
        assert [item for item in blocks if item["type"] == "text_delta"] == [
            dict(type="text_delta", block=0, text="A"),
            dict(type="text_delta", block=1, text="B"),
            dict(type="text_delta", block=0, text="a"),
        ], blocks
        entries = inspect("blocks")
        messages = [entry["data"]["message"]["content"] for entry in entries if entry["kind"] == "assistant"]
        assert messages == [[dict(Text="Aa"), dict(Text="B")]], messages
        assert inspect("blocks") == entries
        reopened_blocks = submit("blocks", "Continue with the original native messages")
        assert [item for item in reopened_blocks if item["type"] == "text_delta"] == [dict(type="text_delta", block=0, text="BLOCKS_REOPENED")]
        assert len(requests["/blocks/v1/responses"]) == 2

        select("/coding", "--context-window", "65536", "--max-output-tokens", "4096")
        coding.update(phase="repair", source=workspace / "answer.py", counter=workspace / "coding-checks", initial="def answer():\n    return 0\n", before="def answer():\n    return 0\n\n# Independent writer's comment must survive the repair.\n", after="def answer():\n    return 2\n\n# Independent writer's comment must survive the repair.\n")
        coding["source"].write_text(coding["initial"])
        check = workspace / "check.py"
        check.write_text("import pathlib, runpy, sys\nroot = pathlib.Path(__file__).parent\nvalue = runpy.run_path(str(root / 'answer.py'))['answer']()\nwith (root / 'coding-checks').open('a') as output:\n    output.write(f'{value}\\n')\nprint('CODING_CHECK_PASSED' if value == 2 else 'CODING_CHECK_FAILED')\nsys.exit(0 if value == 2 else 1)\n")
        check_digest = hashlib.sha256(check.read_bytes()).hexdigest()
        coding["command"] = f"{shlex.quote(sys.executable)} -I -B check.py"
        repaired = submit("coding", "Repair answer.py and verify it.", "--tools", "read,edit,exec")
        assert repaired[-1] == dict(type="run_end", status="completed"), repaired
        assert sum(item["type"] == "tool_started" for item in repaired) == 6
        assert coding["source"].read_text() == coding["after"]
        assert coding["counter"].read_text() == "0\n2\n"
        entries = inspect("coding")
        outcomes = [entry["data"]["result"]["outcome"]["output"]["is_error"] for entry in entries if entry["kind"] == "tool_result"]
        assert outcomes == [True, False, True, False, False, False], outcomes
        # Retain a large settled suffix so the cut genuinely removes every old
        # repair result from working context, not just an unrelated early entry.
        coding["phase"] = "padding"
        submit("coding", "Settled context:\n" + "retain recent context\n" * 4096, "--tools", "read,edit,exec")
        before_compaction = inspect("coding")
        coding["phase"] = "summary"
        run(env, "--cwd", workspace, "--session", work / "coding.sqlite", "compact")
        compacted = inspect("coding")
        assert compacted[:len(before_compaction)] == before_compaction, "compaction rewrote raw facts"
        cut = next(entry["data"]["through_entry"] for entry in reversed(compacted) if entry["kind"] == "compacted")
        last_result = max(index + 1 for index, entry in enumerate(entries) if entry["kind"] == "tool_result")
        assert cut >= last_result, "fixture left old coding results in working context"
        coding["phase"] = "reopen"
        continued = submit("coding", "Recall the verified repair without running it again.", "--tools", "read")
        assert continued[-1] == dict(type="run_end", status="completed"), continued
        reopened = inspect("coding")
        assert reopened[:len(compacted)] == compacted
        assert not any(entry["kind"] == "tool_result" for entry in reopened[len(compacted):])
        assert inspect("coding") == reopened, "passive reopen changed history"
        assert coding["source"].read_text() == coding["after"] and coding["counter"].read_text() == "0\n2\n"
        assert hashlib.sha256(check.read_bytes()).hexdigest() == check_digest, "check source changed"
        assert len(requests["/coding/v1/responses"]) == 10

        for mode in ("recover", "observed", "twice"):
            name = f"overflow-{mode}"
            prefix = f"/{name}"
            counter = workspace / f"{name}-effects"
            forbidden = workspace / f"{name}-forbidden"
            overflow[prefix + "/v1/responses"] = dict(mode=mode, counter=counter,
                command=f"printf 'once\\n' >> {shlex.quote(counter.name)}; printf 'PRESSURE_EFFECT_OBSERVED\\n'",
                forbidden_command=f"printf 'forbidden\\n' >> {shlex.quote(forbidden.name)}")
            select(prefix)
            seeded = submit(name, "Record the initial effect.", "--tools", "exec")
            assert seeded[-1] == dict(type="run_end", status="completed"), seeded
            before = inspect(name)
            assert counter.read_text() == "once\n"
            continued = submit(name, "Continue after context pressure.", "--tools", "exec", success=mode == "recover")
            assert not any(item["type"] in ("provider_retry", "tool_started", "tool_finished") for item in continued), continued
            assert counter.read_text() == "once\n" and not forbidden.exists(), "recovery replayed or dispatched an effect"
            after = inspect(name)
            assert after[:len(before)] == before, "recovery rewrote the committed prefix"
            added = after[len(before):]
            cuts = [entry["data"]["through_entry"] for entry in added if entry["kind"] == "compacted"]
            discarded = [entry["data"] for entry in added if entry["kind"] == "uncommitted_usage"]
            assert len(discarded) == (2 if mode == "twice" else 1), discarded
            assert all(entry["purpose"] == "assistant" for entry in discarded)
            expected_usage = [(31, 5 if mode == "observed" else 0)]
            if mode == "twice":
                expected_usage.append((19, 0))
            assert [(entry["usage"]["input_tokens"], entry["usage"]["output_tokens"]) for entry in discarded] == expected_usage
            starts = [entry["data"]["turn"] for entry in added if entry["kind"] == "turn_started"]
            ends = [entry["data"] for entry in added if entry["kind"] == "turn_ended"]
            assert len(starts) == 1 and len(ends) == 1 and ends[0]["turn"] == starts[0], "recovery created another Turn"
            assert all(entry["data"]["turn"] == starts[0] for entry in added if entry["kind"] == "assistant")
            if mode == "observed":
                assert not cuts and len(requests[prefix + "/v1/responses"]) == 3
                assert not any(entry["kind"] in ("assistant", "tool_result") for entry in added), "failed native output entered model history"
            else:
                assert len(cuts) == 1 and len(requests[prefix + "/v1/responses"]) == 5
                result_entry = next(index + 1 for index, entry in enumerate(before) if entry["kind"] == "tool_result")
                assert cuts[0] >= result_entry, "cut retained the old native effect"
                assert len([entry for entry in added if entry["kind"] == "assistant"]) == (1 if mode == "recover" else 0)
            if mode == "recover":
                assert continued[-1] == dict(type="run_end", status="completed"), continued
            assert inspect(name) == after, "passive inspection changed recovery facts"
            print(f"Responses overflow {mode}: {len(requests[prefix + '/v1/responses'])} requests, one effect, {len(cuts)} compaction(s): OK", flush=True)

        select("/retry")
        retried = submit("retry", "Retry before output")
        assert sum(item["type"] == "provider_retry" for item in retried) == 1, retried
        assert requests["/retry/v1/responses"][0] == requests["/retry/v1/responses"][1]
        entries = inspect("retry")
        discarded = [entry["data"] for entry in entries if entry["kind"] == "uncommitted_usage"]
        assert len(discarded) == 1, discarded
        assert discarded[0]["purpose"] == "assistant"
        assert discarded[0]["execution"]["route"]["reason"] == "user_request"
        assert discarded[0]["usage"] == dict(input_tokens=17, output_tokens=0, cache_read_input_tokens=0, cache_write_input_tokens=None), discarded
        accepted = [entry["data"] for entry in entries if entry["kind"] == "assistant"]
        assert len(accepted) == 1 and accepted[0]["execution"]["route"]["reason"] == "retry", accepted
        assert sum(sample["usage"]["input_tokens"] for sample in [*discarded, *accepted]) == 27
        assert sum(sample["usage"]["output_tokens"] for sample in [*discarded, *accepted]) == 8
        assert inspect("retry") == entries, "read-only reopen changed accounting"

        select("/post-output")
        failed_run = submit("failed", "Do not execute incomplete output", success=False)
        assert not any(item["type"] in ("provider_retry", "tool_started", "tool_finished") for item in failed_run), failed_run
        assert len(requests["/post-output/v1/responses"]) == 1
        assert not (workspace / "forbidden.txt").exists()
        entries = inspect("failed")
        discarded = [entry["data"] for entry in entries if entry["kind"] == "uncommitted_usage"]
        assert len(discarded) == 1, discarded
        assert discarded[0]["usage"] == dict(input_tokens=23, output_tokens=5, cache_read_input_tokens=7, cache_write_input_tokens=None), discarded
        assert not any(entry["kind"] in ("assistant", "tool_result") for entry in entries), "failed output entered durable model context"

        select("/namespace")
        namespace = submit("namespace", "Reject foreign function identities", success=False)
        assert not any(item["type"] in ("tool_started", "tool_finished") for item in namespace), namespace
        assert len(requests["/namespace/v1/responses"]) == 1
        assert not (workspace / "namespace-forbidden.txt").exists()

        select("/truncated")
        truncated = submit("truncated", "Handle a truncated call safely")
        assert truncated[-1] == dict(type="run_end", status="completed"), truncated
        assert len(requests["/truncated/v1/responses"]) == 3
        assert not (workspace / "forbidden.txt").exists()
        assert not (workspace / "valid-truncated.txt").exists()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        assert not thread.is_alive()

print("Responses stateless replay, indexed text, native images, guarded repair/check failures, compaction/reopen, bounded overflow recovery, failed-attempt usage and effect fences: ok")
