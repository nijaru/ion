"""Deterministic local Chat Completions stream for the executable smoke check."""

import hashlib
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


port_file, requests_file = map(Path, sys.argv[1:3])
DATA_DIGEST = hashlib.sha256(b"sample data\nsecond token\n").hexdigest()
steps = [
    ("read", {"path": "data.txt"}),
    ("edit", {"path": "data.txt", "base_digest": DATA_DIGEST, "edits": [
        {"old_text": "second token", "new_text": "second token updated"},
        {"old_text": "sample data", "new_text": "sample data updated"},
    ]}),
    ("write", {"path": "created.txt", "content": "created by ion\n"}),
    ("exec", {"command": "cat data.txt created.txt"}),
]
count = 0
require_steering = os.environ.get("ION_SMOKE_STEERING") == "1"
partial_failure = os.environ.get("ION_SMOKE_PARTIAL_FAILURE") == "1"
stream_error = os.environ.get("ION_SMOKE_STREAM_ERROR") == "1"
salience = os.environ.get("ION_SMOKE_SALIENCE") == "1"
discovery = os.environ.get("ION_SMOKE_DISCOVERY") == "1"
questions = os.environ.get("ION_SMOKE_QUESTIONS")
question_count = 0


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        global count
        try:
            assert self.path == "/v1/chat/completions"
            assert "Authorization" not in self.headers
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with requests_file.open("a") as trace:
                trace.write(json.dumps(body) + "\n")
            assert body["stream"] is True
            if discovery:
                self.discovery_step(count, body)
                count += 1
                return
            if partial_failure:
                assert count == 0
                count += 1
                payload = b'data: {"id":"smoke","model":"smoke-model","choices":[{"index":0,"delta":{"content":"PROVISIONAL_ANSWER"},"finish_reason":null}]}\n\n'
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                return
            if stream_error:
                assert count == 0
                count += 1
                events = [
                    {"choices": [{"index": 0, "delta": {"content": "PROVISIONAL_ANSWER"}, "finish_reason": None}]},
                    {"error": {"code": "server_error", "message": "upstream disconnected"}, "choices": [{"index": 0, "delta": {}, "finish_reason": "error"}]},
                ]
                payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
                return
            if count == 6 and require_steering:
                assert "tools" not in body
            else:
                names = {tool["function"]["name"] for tool in body["tools"]}
                default = {"read", "edit", "write", "exec", "process", "subagent", "history", "ls", "find", "grep", "web_fetch"}
                expected = default | ({"ask_user_question"} if questions or os.environ.get("ION_SMOKE_TERMINAL") == "1" else set())
                assert names == expected, (names, expected)
            if questions:
                self.question_step(body)
                count += 1
                return
            if salience:
                if count == 0:
                    calls = [("exec", {"command": "printf 'FAILURE_MARKER\\n'; exit 7"})]
                    calls.extend(("write", {"path": f"mutation-{index}.txt", "content": "observed mutation\\n"}) for index in range(14))
                    calls.append(("exec", {"command": "touch active.ready; exec sleep 5"}))
                    calls.extend(("read", {"path": f"mutation-{index}.txt"}) for index in range(2))
                    delta = {"tool_calls": [{"index": index, "id": f"salience-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}} for index, (name, arguments) in enumerate(calls)]}
                    finish = "tool_calls"
                else:
                    assert count == 1
                    delta, finish = {"content": "SALIENCE_DONE"}, "stop"
            elif count < 4:
                if count:
                    last = body["messages"][-1]
                    if count == 1 and require_steering:
                        assert last["role"] == "user" and "also check" in last["content"].lower()
                        assert body["messages"][-2]["role"] == "tool"
                        assert body["messages"][-2]["tool_call_id"] == "call-1"
                        assert body["messages"][-3]["tool_calls"][0]["id"] == "call-1"
                    else:
                        assert last["role"] == "tool" and last["tool_call_id"] == f"call-{count}"
                        assert body["messages"][-2]["tool_calls"][0]["id"] == f"call-{count}"
                name, arguments = steps[count]
                delta = {"tool_calls": [{"index": 0, "id": f"call-{count + 1}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
                finish = "tool_calls"
                if count == 0 and require_steering:
                    time.sleep(0.75)
            elif count == 4:
                assert body["messages"][-1]["role"] == "tool"
                assert "sample data updated" in body["messages"][-1]["content"]
                delta, finish = {"content": "# TASK_COMPLETE\n\n**MARKDOWN_BOLD** and `literal_code` [docs](https://example.org/ion)\n\n> - nested preview" if require_steering else "TASK_COMPLETE"}, "stop"
            elif count == 5:
                assert len([m for m in body["messages"] if m["role"] == "user"]) == (3 if require_steering else 2)
                delta, finish = {"content": "RESUMED"}, "stop"
            elif count == 6 and require_steering:
                delta, finish = {"content": "The file repair is complete and tests passed."}, "stop"
            else:
                raise AssertionError("unexpected extra model request")
            count += 1
            events = [
                {"id": "smoke", "model": "smoke-model", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                {"id": "smoke", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]},
            ]
            payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            if salience and finish == "stop":
                # Final text is provisional until stop/DONE is consumed.
                # Keep the active-band fixture honest about that boundary.
                prefix = b"data: " + json.dumps(events[0]).encode() + b"\n\n"
                self.wfile.write(prefix)
                self.wfile.flush()
                time.sleep(0.5)
                self.wfile.write(payload[len(prefix):])
            else:
                self.wfile.write(payload)
        except Exception as error:
            print(f"mock provider request {count} failed: {error!r}", file=sys.stderr, flush=True)
            self.send_error(500, str(error))

    def question_step(self, body):
        global question_count
        if question_count == 0:
            calls = [("question-1", "ask_user_question", {
                "questions": [{
                    "question": "Which route should the fixture take?",
                    "options": [
                        {"label": "Alpha", "description": "first"},
                        {"label": "Beta", "description": "second"},
                    ],
                }],
            })]
            delta = {"tool_calls": [
                {"index": index, "id": call_id, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}
                for index, (call_id, name, arguments) in enumerate(calls)
            ]}
            finish = "tool_calls"
            question_count = 1
        elif question_count == 1:
            last = body["messages"][-1]
            assert last["role"] == "tool" and last["tool_call_id"] == "question-1", last
            envelope = json.loads(last["content"])
            result = envelope.get("result", envelope)
            mode = questions
            if mode == "select":
                assert result["status"] == "answered", result
                assert result["answers"] == [{"kind": "option", "question": 1, "option": 1, "label": "Beta"}], result
                effect = "SELECTED:Beta"
            elif mode == "custom":
                assert result["status"] == "answered", result
                assert result["answers"] == [{"kind": "text", "question": 1, "text": "café 🦀"}], result
                effect = "CUSTOM:café 🦀"
            elif mode == "cancel":
                assert result["status"] == "cancelled" and result["reason"] == "user_cancelled", result
                delta, finish = {"content": "CANCEL_OBSERVED"}, "stop"
                question_count = 3
                effect = None
            else:
                raise AssertionError(f"question mode {mode!r} received a continuation")
            if effect is not None:
                delta = {"tool_calls": [{"index": 0, "id": "effect-1", "type": "function", "function": {"name": "write", "arguments": json.dumps({"path": "question-effect.txt", "content": effect})}}]}
                finish = "tool_calls"
                question_count = 2
        elif question_count == 2:
            last = body["messages"][-1]
            assert last["role"] == "tool" and last["tool_call_id"] == "effect-1", last
            assert "question-effect.txt" in last["content"], last
            delta, finish = {"content": "QUESTION_DONE"}, "stop"
            question_count = 3
        else:
            raise AssertionError("unexpected question continuation")
        events = [
            {"id": "smoke", "model": "smoke-model", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
            {"id": "smoke", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]},
        ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def discovery_step(self, step, body):
        names = {tool["function"]["name"] for tool in body["tools"]}
        assert names == {"read", "ls", "find", "grep", "web_fetch", "history"}, names
        if step == 0:
            calls = [("discovery-ls", "ls", {"path": "."})]
        elif step == 1:
            last = body["messages"][-1]
            assert last["role"] == "tool" and last["tool_call_id"] == "discovery-ls", last
            assert "data.txt" in last["content"] and "nested" in last["content"], last
            calls = [("discovery-find", "find", {"path": ".", "pattern": "*.txt"})]
        elif step == 2:
            last = body["messages"][-1]
            assert last["role"] == "tool" and last["tool_call_id"] == "discovery-find", last
            assert "data.txt" in last["content"] and "nested/note.txt" in last["content"], last
            calls = [("discovery-grep", "grep", {"path": ".", "pattern": "needle"})]
        elif step == 3:
            last = body["messages"][-1]
            assert last["role"] == "tool" and last["tool_call_id"] == "discovery-grep", last
            assert "nested/note.txt" in last["content"] and "needle here" in last["content"], last
            result = json.loads(last["content"])["result"]
            match = result["matches"][0]
            path = Path(result["root"])
            if result["root_kind"] == "directory":
                path /= match["path"]
            calls = [("discovery-read", "read", {"path": str(path), "byte_offset": match["byte_offset"], "byte_limit": 4096})]
        elif step == 4:
            last = body["messages"][-1]
            assert last["role"] == "tool" and last["tool_call_id"] == "discovery-read", last
            assert "needle here" in last["content"], last
            calls = []
        else:
            raise AssertionError("unexpected discovery request")
        if step == 4:
            delta, finish = {"content": "DISCOVERY_SMOKE_OK"}, "stop"
        else:
            delta = {"tool_calls": [
                {"index": index, "id": call_id, "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}
                for index, (call_id, name, arguments) in enumerate(calls)
            ]}
            finish = "tool_calls"
        events = [
            {"id": "smoke", "model": "smoke-model", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
            {"id": "smoke", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]},
        ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format, *args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
port_file.write_text(str(server.server_port))
server.serve_forever()
