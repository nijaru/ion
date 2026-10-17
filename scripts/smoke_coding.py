#!/usr/bin/env python3
"""Credential-free fixed coding task with an independent workspace oracle."""
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
SOURCE_DIGEST = hashlib.sha256(SOURCE.encode()).hexdigest()
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
                calls = [
                    tool_call("read-test", "read", {"path": "test_calculator.py"}),
                    tool_call("read-source", "read", {"path": "calculator.py"}),
                ]
                self.send_calls(calls)
            elif count == 1:
                tool_messages = [m for m in body["messages"] if m["role"] == "tool"]
                assert len(tool_messages) == 2, tool_messages
                calls = [
                    tool_call(
                        "edit-source",
                        "edit",
                        {
                            "path": "calculator.py",
                            "base_digest": SOURCE_DIGEST,
                            "edits": [
                                {
                                    "old_text": "def add(a, b):\n    return a - b",
                                    "new_text": "def add(a, b):\n    return a + b",
                                }
                            ],
                        },
                    )
                ]
                self.send_calls(calls)
            elif count == 2:
                tool_messages = [m for m in body["messages"] if m["role"] == "tool"]
                assert len(tool_messages) == 3, tool_messages
                envelope = json.loads(tool_messages[-1]["content"])
                assert envelope["is_error"] is False, envelope
                self.send_calls(
                    [
                        tool_call(
                            "run-tests",
                            "exec",
                            {"command": "python3 -m unittest -q"},
                        )
                    ]
                )
            elif count == 3:
                tool_messages = [m for m in body["messages"] if m["role"] == "tool"]
                assert len(tool_messages) == 4, tool_messages
                envelope = json.loads(tool_messages[-1]["content"])
                assert envelope["is_error"] is False, envelope
                assert envelope["result"]["exit_code"] == 0, envelope
                self.send_final("CODING_SMOKE_OK")
            else:
                raise AssertionError(f"unexpected model request {count}")
        except Exception as error:
            print(f"mock provider request {count} failed: {error!r}", file=sys.stderr, flush=True)
            self.send_error(500, str(error))

    def send_calls(self, calls):
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
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
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
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_STATE_HOME=str(root / "state"),
            TMPDIR=str(root / "tmp"),
            LANG="C.UTF-8",
            LC_ALL="C.UTF-8",
        )
        (root / "home").mkdir()
        (root / "tmp").mkdir()
        try:
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
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

        assert len(requests) == 4, len(requests)
        oracle = subprocess.run(
            [sys.executable, "-m", "unittest", "-q"],
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

        inspected = subprocess.run(
            [str(BINARY), "--cwd", str(workspace), "--continue", "inspect"],
            env=env,
            check=True,
            capture_output=True,
            text=True,
        )
        view = json.loads(inspected.stdout)
        assert view["unfinished_turn"] is None, view["unfinished_turn"]
        kinds = [entry["kind"] for entry in view["entries"]]
        assert kinds.count("turn_started") == 1, kinds
        assert kinds.count("turn_ended") == 1, kinds
        assert kinds.count("tool_result") == 4, kinds
        print("Ion fixed a failing fixture under independent test verification: OK")


if __name__ == "__main__":
    main()
