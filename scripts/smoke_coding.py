#!/usr/bin/env python3
"""Offline fail/correct/verify and saved-Session follow-up with real coding tools."""
import hashlib
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

SOURCE = """def add(a, b):
    return a - b
"""
TEST = """import unittest
from calculator import add


class CalculatorTest(unittest.TestCase):
    def test_positive(self):
        self.assertEqual(add(2, 3), 5)

    def test_negative(self):
        self.assertEqual(add(-2, -3), -5)

    def test_zero(self):
        self.assertEqual(add(0, 0), 0)


if __name__ == "__main__":
    unittest.main()
"""
PROMPT = (
    "Fix the failing calculator test in this workspace. Read the source and test, "
    "make the smallest correct edit, run the native test suite, and report the result."
)
FOLLOWUP = "Describe the verified calculator change without running any tools."
# The correction keeps the source size unchanged. Disable bytecode caching so
# same-second writes cannot make a later check import the broken version.
CHECK_COMMAND = "python3 -B -m unittest -q"
requests = []


def tool_call(call_id, name, arguments):
    return {
        "index": 0,
        "id": call_id,
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        count = len(requests) - 1
        try:
            if count == 0:
                names = {tool["function"]["name"] for tool in body["tools"]}
                assert {"read", "edit", "write", "exec", "subagent"} <= names, names
                assert body["messages"][-1]["role"] == "user", body["messages"][-1]
                # Valid-looking arguments are still unsafe when the response
                # hits its output limit. Neither mutation may be dispatched.
                self.send_calls([
                    tool_call("truncated-write", "write", {"path": "truncated-write.txt", "content": "wrong"}),
                    tool_call("truncated-exec", "exec", {"command": "printf wrong > truncated-exec.txt"}),
                ], finish_reason="length")
            elif count == 1:
                rejected = [message for message in body["messages"] if message["role"] == "tool"]
                assert [message["tool_call_id"] for message in rejected] == ["truncated-write", "truncated-exec"], rejected
                for message in rejected:
                    envelope = json.loads(message["content"])
                    assert envelope["is_error"] is True, envelope
                    assert "output token limit" in json.dumps(envelope), envelope
                calls = [
                    tool_call("read-test", "read", {"path": "test_calculator.py"}),
                    tool_call("read-source", "read", {"path": "calculator.py"}),
                ]
                self.send_calls(calls)
            elif count == 2:
                tool_messages = [m for m in body["messages"] if m["role"] == "tool"]
                assert len(tool_messages) == 4, tool_messages
                reads = {message["tool_call_id"]: json.loads(message["content"]) for message in tool_messages[2:]}
                assert set(reads) == {"read-test", "read-source"}, reads
                for call_id, expected in [("read-test", TEST), ("read-source", SOURCE)]:
                    envelope = reads[call_id]
                    assert envelope["is_error"] is False, envelope
                    result = envelope["result"]
                    assert result["content"] == expected, result
                    assert result["base_digest"] == hashlib.sha256(expected.encode()).hexdigest(), result
                    assert result["start_line"] == 1 and result["has_more"] is False, result
                self.send_calls([tool_call("baseline-tests", "exec", {"command": CHECK_COMMAND})])
            elif count == 3:
                tool_messages = [m for m in body["messages"] if m["role"] == "tool"]
                assert len(tool_messages) == 5, tool_messages
                assert tool_messages[-1]["tool_call_id"] == "baseline-tests", tool_messages[-1]
                envelope = json.loads(tool_messages[-1]["content"])
                assert envelope["is_error"] is True, envelope
                result = envelope["result"]
                assert result["exit_code"] == 1 and result["signal"] is None, result
                assert not result["cancelled"] and not result["timed_out"], result
                assert result["wait_error"] is None, result
                assert "Ran 3 tests" in result["stderr"] and "FAILED (failures=2)" in result["stderr"], result
                assert "test_positive" in result["stderr"] and "test_negative" in result["stderr"], result
                reads = {message["tool_call_id"]: json.loads(message["content"]) for message in tool_messages[2:4]}
                observed_source = reads["read-source"]["result"]
                calls = [
                    tool_call(
                        "edit-source",
                        "edit",
                        {
                            "path": "calculator.py",
                            "base_digest": observed_source["base_digest"],
                            "edits": [
                                {
                                    "old_text": observed_source["content"],
                                    "new_text": observed_source["content"].replace("return a - b", "return a + b"),
                                }
                            ],
                        },
                    )
                ]
                self.send_calls(calls)
            elif count == 4:
                tool_messages = [m for m in body["messages"] if m["role"] == "tool"]
                assert len(tool_messages) == 6, tool_messages
                envelope = json.loads(tool_messages[-1]["content"])
                assert envelope["is_error"] is False, envelope
                self.send_calls(
                    [
                        tool_call(
                            "run-tests",
                            "exec",
                            {"command": CHECK_COMMAND},
                        )
                    ]
                )
            elif count == 5:
                tool_messages = [m for m in body["messages"] if m["role"] == "tool"]
                assert len(tool_messages) == 7, tool_messages
                assert tool_messages[-1]["tool_call_id"] == "run-tests", tool_messages[-1]
                envelope = json.loads(tool_messages[-1]["content"])
                assert envelope["is_error"] is False, envelope
                assert envelope["result"]["exit_code"] == 0, envelope
                assert "Ran 3 tests" in envelope["result"]["stderr"] and "OK" in envelope["result"]["stderr"], envelope
                self.send_final("CODING_SMOKE_OK")
            elif count == 6:
                assert [message["content"] for message in body["messages"] if message["role"] == "user"] == [PROMPT, FOLLOWUP], body
                assert any(message["role"] == "assistant" and message.get("content") == "CODING_SMOKE_OK" for message in body["messages"]), body
                restored = [message for message in body["messages"] if message["role"] == "tool"]
                previous = [message for message in requests[5]["messages"] if message["role"] == "tool"]
                assert restored == previous, (restored, previous)
                self.send_final("CODING_FOLLOWUP_OK")
            else:
                raise AssertionError(f"unexpected model request {count}")
        except Exception as error:
            print(f"mock provider request {count} failed: {error!r}", file=sys.stderr, flush=True)
            self.send_error(500, str(error))

    def send_calls(self, calls, finish_reason="tool_calls"):
        for index, call in enumerate(calls):
            call["index"] = index
        events = [
            {
                "id": "coding",
                "choices": [
                    {
                        "index": 0,
                        "delta": {"tool_calls": calls},
                        "finish_reason": None,
                    }
                ],
            },
            {
                "id": "coding",
                "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}],
            },
        ]
        self.send_events(events)

    def send_final(self, text):
        events = [
            {
                "id": "coding",
                "choices": [
                    {
                        "index": 0,
                        "delta": {"content": text},
                        "finish_reason": None,
                    }
                ],
            },
            {
                "id": "coding",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            },
        ]
        self.send_events(events)

    def send_events(self, events):
        payload = b"".join(
            b"data: " + json.dumps(event).encode() + b"\n\n" for event in events
        ) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


def inspect_session(workspace, env):
    inspected = subprocess.run(
        [str(BINARY), "--cwd", str(workspace), "--continue", "inspect"],
        env=env,
        check=True,
        capture_output=True,
        text=True,
        timeout=60,
    )
    return json.loads(inspected.stdout)


def main():
    with tempfile.TemporaryDirectory(prefix="ion-coding-smoke-") as temporary:
        root = Path(temporary)
        workspace = root / "workspace"
        workspace.mkdir()
        (workspace / "calculator.py").write_text(SOURCE)
        (workspace / "test_calculator.py").write_text(TEST)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        env = {
            key: value
            for key, value in os.environ.items()
            if key
            not in {
                "DEEPSEEK_API_KEY",
                "OPENROUTER_API_KEY",
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "XIAOMI_API_KEY",
                "ION_CUSTOM_API_KEY",
            }
        }
        env.update(
            HOME=str(root / "home"),
            ION_HOME=str(root / "ion"), XDG_CONFIG_HOME=str(root / "config"),
            XDG_STATE_HOME=str(root / "state"),
            TMPDIR=str(root / "tmp"),
            LANG="C.UTF-8",
            LC_ALL="C.UTF-8",
        )
        (root / "home").mkdir()
        (root / "tmp").mkdir()
        try:
            baseline = subprocess.run(
                [sys.executable, "-B", "-m", "unittest", "-q"],
                cwd=workspace,
                env=env,
                check=False,
                capture_output=True,
                text=True,
                timeout=60,
            )
            assert baseline.returncode == 1 and "FAILED (failures=2)" in baseline.stderr, baseline
            endpoint = f"http://127.0.0.1:{server.server_port}/v1"
            subprocess.run(
                [
                    str(BINARY),
                    "use",
                    "smoke",
                    "coding-model",
                    "--endpoint",
                    endpoint,
                    "--wire",
                    "chat-completions",
                ],
                env=env,
                check=True,
                capture_output=True,
                text=True,
            )
            result = subprocess.run(
                [str(BINARY), "--cwd", str(workspace), "run", PROMPT],
                env=env,
                check=False,
                capture_output=True,
                text=True,
                timeout=120,
            )
            assert result.returncode == 0, (result.stdout, result.stderr)
            assert result.stdout.strip() == "CODING_SMOKE_OK", result
            first_view = inspect_session(workspace, env)
            assert first_view["unfinished_turn"] is None, first_view
            assert first_view["entries"][-1]["kind"] == "turn_ended", first_view["entries"][-1]
            assert first_view["entries"][-1]["data"]["reason"] == "completed", first_view["entries"][-1]
            followup = subprocess.run(
                [str(BINARY), "--cwd", str(workspace), "--continue", "run", FOLLOWUP],
                env=env,
                check=False,
                capture_output=True,
                text=True,
                timeout=120,
            )
            assert followup.returncode == 0, (followup.stdout, followup.stderr)
            assert followup.stdout.strip() == "CODING_FOLLOWUP_OK", followup
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

        assert len(requests) == 7, len(requests)
        assert not (workspace / "truncated-write.txt").exists()
        assert not (workspace / "truncated-exec.txt").exists()
        oracle = subprocess.run(
            [sys.executable, "-B", "-m", "unittest", "-q"],
            cwd=workspace,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert oracle.returncode == 0, oracle
        source = (workspace / "calculator.py").read_text()
        assert "return a + b" in source and "return a - b" not in source, source

        assert (workspace / "test_calculator.py").read_text() == TEST
        assert {path.name for path in workspace.iterdir()} == {"calculator.py", "test_calculator.py"}
        view = inspect_session(workspace, env)
        assert view["id"] == first_view["id"], (view["id"], first_view["id"])
        assert view["entries"][:len(first_view["entries"])] == first_view["entries"]
        assert view["unfinished_turn"] is None, view["unfinished_turn"]
        kinds = [entry["kind"] for entry in view["entries"]]
        assert kinds.count("turn_started") == 2, kinds
        assert kinds.count("turn_ended") == 2, kinds
        assert kinds.count("tool_result") == 7, kinds
        assert [entry["data"]["reason"] for entry in view["entries"] if entry["kind"] == "turn_ended"] == ["completed", "completed"], view
        results = [entry["data"]["result"] for entry in view["entries"] if entry["kind"] == "tool_result"]
        assert [result["call_id"] for result in results] == ["truncated-write", "truncated-exec", "read-test", "read-source", "baseline-tests", "edit-source", "run-tests"], results
        assert [result["outcome"]["state"] for result in results] == ["not_dispatched"] * 2 + ["observed"] * 5, results
        wire_results = {message["tool_call_id"]: json.loads(message["content"]) for message in requests[-1]["messages"] if message["role"] == "tool"}
        for result in results[2:]:
            output = result["outcome"]["output"]
            assert output["value"] == wire_results[result["call_id"]]["result"], result
            assert output["is_error"] == wire_results[result["call_id"]]["is_error"], result
        assert results[4]["outcome"]["output"]["is_error"] is True, results[4]
        assert results[-1]["outcome"]["output"]["is_error"] is False, results[-1]
        assistants = [entry["data"] for entry in view["entries"] if entry["kind"] == "assistant"]
        assert assistants[0]["termination"] == {"Incomplete": "MaxOutputTokens"}, assistants[0]
        reasons = [assistant["execution"]["route"]["reason"] for assistant in assistants]
        assert reasons == ["user_request"] + ["tool_continuation"] * 5 + ["user_request"], reasons
        print("Ion rejected truncated mutations, observed a failed check, corrected and verified the fixture, and reopened for a tool-free follow-up: OK")


if __name__ == "__main__":
    main()
