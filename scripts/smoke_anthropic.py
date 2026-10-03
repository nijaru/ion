"""Exercise signed Anthropic Messages replay through the built headless CLI."""

import json
import os
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []
summary_requests = []


def event(kind, **fields):
    value = {"type": kind, **fields}
    return f"event: {kind}\ndata: {json.dumps(value)}\n\n".encode()


def response(index):
    frames = [
        event(
            "message_start",
            message={
                "type": "message",
                "role": "assistant",
                "model": "mock",
                "content": [],
                "stop_reason": None,
                "usage": {"input_tokens": 10, "output_tokens": 0},
            },
        )
    ]
    if 0 < index <= 4:
        frames.extend(
            [
                event(
                    "content_block_start",
                    index=0,
                    content_block={"type": "thinking", "thinking": "", "signature": ""},
                ),
                event(
                    "content_block_delta",
                    index=0,
                    delta={"type": "signature_delta", "signature": f"signed-{index}"},
                ),
                event("content_block_stop", index=0),
            ]
        )
    block_index = int(0 < index <= 4)
    if index == 1:
        frames.append(
            event(
                "content_block_start",
                index=block_index,
                content_block={
                    "type": "tool_use",
                    "id": "tool_1",
                    "name": "read",
                    "input": {"path": "data.txt"},
                },
            )
        )
        frames.append(event("content_block_stop", index=block_index))
        stop_reason = "tool_use"
    else:
        frames.append(
            event(
                "content_block_start",
                index=block_index,
                content_block={
                    "type": "text",
                    "text": "SUMMARY_OK" if index == 0 else f"TURN_{index - 1}_DONE",
                },
            )
        )
        frames.append(event("content_block_stop", index=block_index))
        stop_reason = "end_turn"
    frames.extend(
        [
            event(
                "message_delta",
                delta={"stop_reason": stop_reason},
                usage={"output_tokens": 8},
            ),
            event("message_stop"),
        ]
    )
    return b"".join(frames)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if str(body.get("system", "")).startswith("Summarize the coding conversation"):
            summary_requests.append(body)
            payload = response(0)
        else:
            requests.append(body)
            payload = response(len(requests))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format, *args):
        pass


def run(binary, env, *args):
    result = subprocess.run([binary, *map(str, args)], env=env, capture_output=True, text=True)
    assert result.returncode == 0, (result.args, result.stdout, result.stderr)
    return [json.loads(line) for line in result.stdout.splitlines()] if "--json" in args else []


with tempfile.TemporaryDirectory(prefix="ion-anthropic-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "data.txt").write_text("data\n")
    instructions = workspace / "AGENTS.md"
    instructions.write_text("PROJECT_ALPHA\n")
    env = os.environ.copy()
    env.update(XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/messages"
        run(binary, env, "use", "smoke", "mock", "--endpoint", endpoint, "--wire", "anthropic-messages")
        first = run(binary, env, "--json", "--cwd", workspace, "run", "Read data.txt")
        assert first[-1] == {"type": "run_end", "status": "completed"}, first
        assert any(item.get("type") == "tool_finished" for item in first), first
        second = run(binary, env, "--json", "--cwd", workspace, "--continue", "run", "Continue")
        assert second[-1] == {"type": "run_end", "status": "completed"}, second
        assert len(requests) == 3, requests
        signed = requests[1]["messages"][1]["content"][0]
        assert signed == {"type": "thinking", "thinking": "", "signature": "signed-1"}, signed
        assert requests[1]["messages"][1]["content"][1]["id"] == "tool_1"
        assert requests[1]["messages"][2]["content"][0]["tool_use_id"] == "tool_1"
        assert sum(
            block["type"] == "thinking"
            for message in requests[2]["messages"]
            for block in message["content"]
        ) == 2
        run(binary, env, "--cwd", workspace, "--continue", "compact")
        assert len(summary_requests) >= 1
        compacted = run(binary, env, "--json", "--cwd", workspace, "--continue", "run", "After compaction")
        assert compacted[-1] == {"type": "run_end", "status": "completed"}, compacted
        assert any(item.get("type") == "provider_replay_rebased" for item in compacted), compacted
        assert len(requests) == 4, requests
        assert all(
            block["type"] not in ("thinking", "redacted_thinking")
            for message in requests[3]["messages"]
            for block in message["content"]
        )
        instructions.write_text("PROJECT_BETA\n")
        third = run(binary, env, "--json", "--cwd", workspace, "--continue", "run", "Continue again")
        assert third[-1] == {"type": "run_end", "status": "completed"}, third
        assert any(item.get("type") == "provider_replay_rebased" for item in third), third
        assert len(requests) == 5, requests
        assert all(
            block["type"] not in ("thinking", "redacted_thinking")
            for message in requests[4]["messages"]
            for block in message["content"]
        )
        inspected = subprocess.run(
            [binary, "--cwd", workspace, "--continue", "inspect"],
            env=env,
            check=True,
            capture_output=True,
            text=True,
        )
        entries = json.loads(inspected.stdout)["entries"]
        assert sum(entry["kind"] == "provider_replay_rebased" for entry in entries) == 2
        assert sum(
            entry["kind"] == "assistant" and entry["data"]["message"]["provider_replay"] is not None
            for entry in entries
        ) >= 2
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

print("Anthropic signed headless continuation, compaction and resource rebase: ok")
