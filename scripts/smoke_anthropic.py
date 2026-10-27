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
retry_requests = []
legacy_requests = []
migration_requests = []


def event(kind, **fields):
    value = {"type": kind, **fields}
    return f"event: {kind}\ndata: {json.dumps(value)}\n\n".encode()


def response(index, *, overloaded=False):
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
    if overloaded:
        return b"".join(frames) + event(
            "error", error={"type": "overloaded_error", "message": "busy"}
        )
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
        if self.path == "/migration-chat/v1/chat/completions":
            legacy_requests.append(body)
            calls = len(legacy_requests) == 1
            delta = {"tool_calls": [
                {"index": index, "id": identity, "type": "function", "function": {
                    "name": "read", "arguments": json.dumps({"path": "data.txt"}),
                }} for index, identity in enumerate(["functions.echo:0", "ion_0"])
            ]} if calls else {"content": "LEGACY_DONE"}
            chunk = {"model": "legacy", "choices": [{"index": 0, "delta": delta,
                "finish_reason": "tool_calls" if calls else "stop"}]}
            payload = f"data: {json.dumps(chunk)}\n\ndata: [DONE]\n\n".encode()
        elif self.path == "/migration/v1/messages":
            migration_requests.append(body)
            payload = response(len(migration_requests))
        elif self.path == "/retry/v1/messages":
            retry_requests.append(body)
            payload = response(0, overloaded=len(retry_requests) == 1)
        elif str(body.get("system", "")).startswith("Summarize the conversation"):
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
    result = subprocess.run(
        [binary, *map(str, args)], env=env, capture_output=True, text=True, timeout=20
    )
    assert result.returncode == 0, (result.args, result.stdout, result.stderr)
    return [json.loads(line) for line in result.stdout.splitlines()] if "--json" in args else []


with tempfile.TemporaryDirectory(prefix="ion-anthropic-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "data.txt").write_text("data\n")
    instructions = workspace / "AGENTS.md"
    instructions.write_text("PROJECT_ALPHA\n")
    home = work / "home"
    home.mkdir()
    temporary_files = work / "tmp"
    temporary_files.mkdir()
    env = {key: os.environ[key] for key in ("PATH", "SHELL") if key in os.environ}
    env.update(
        HOME=str(home), ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"),
        XDG_STATE_HOME=str(work / "state"), XDG_DATA_HOME=str(work / "data"),
        TMPDIR=str(temporary_files),
    )
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

        retry_endpoint = f"http://127.0.0.1:{server.server_port}/retry/v1/messages"
        run(binary, env, "use", "smoke", "mock", "--endpoint", retry_endpoint, "--wire", "anthropic-messages")
        retried = run(binary, env, "--json", "--cwd", workspace, "run", "Retry before output")
        assert retried[-1] == {"type": "run_end", "status": "completed"}, retried
        retries = [i for i, item in enumerate(retried) if item["type"] == "provider_retry"]
        assert len(retries) == 1, retried
        assert not any(
            item["type"] in ("text_delta", "thinking_delta", "model_output_observed")
            for item in retried[:retries[0]]
        ), retried
        assert len(retry_requests) == 2, retry_requests
        assert retry_requests[0] == retry_requests[1], "retry changed the prepared request"

        migration = work / "migration.sqlite"
        base = f"http://127.0.0.1:{server.server_port}"
        run(binary, env, "use", "smoke", "legacy", "--endpoint", f"{base}/migration-chat/v1/chat/completions", "--wire", "chat-completions")
        legacy = run(binary, env, "--json", "--cwd", workspace, "--session", migration, "run", "Legacy reads")
        assert legacy[-1] == {"type": "run_end", "status": "completed"}, legacy
        assert len(legacy_requests) == 2, legacy_requests

        def migration_view():
            inspected = subprocess.run(
                [binary, "--cwd", workspace, "--session", migration, "inspect"],
                env=env, check=True, capture_output=True, text=True, timeout=20,
            )
            return json.loads(inspected.stdout)

        original_entries = migration_view()["entries"]
        raw_ids = [part["ToolCall"]["id"]
            for entry in original_entries if entry["kind"] == "assistant"
            for part in entry["data"]["message"]["content"] if "ToolCall" in part]
        assert raw_ids == ["functions.echo:0", "ion_0"], raw_ids
        run(binary, env, "use", "smoke", "mock", "--endpoint", f"{base}/migration/v1/messages", "--wire", "anthropic-messages")
        migrated = run(binary, env, "--json", "--cwd", workspace, "--session", migration, "--provider", "smoke", "--model", "mock", "run", "Native continuation")
        assert migrated[-1] == {"type": "run_end", "status": "completed"}, migrated
        reopened = run(binary, env, "--json", "--cwd", workspace, "--session", migration, "run", "Reopen signed continuation")
        assert reopened[-1] == {"type": "run_end", "status": "completed"}, reopened
        assert len(migration_requests) == 3, migration_requests
        assert not any(item["type"] == "provider_replay_rebased" for item in reopened), reopened
        first_wire = migration_requests[0]["messages"]
        migrated_ids = [block["id"] for message in first_wire for block in message["content"] if block["type"] == "tool_use"]
        assert len(set(migrated_ids)) == len(raw_ids), migrated_ids
        assert all(identity and len(identity) <= 64 and identity.isascii()
            and all(c.isalnum() or c in "_-" for c in identity) for identity in migrated_ids), migrated_ids
        result_ids = [block["tool_use_id"] for message in first_wire for block in message["content"] if block["type"] == "tool_result"]
        assert set(result_ids) == set(migrated_ids), (migrated_ids, result_ids)
        for body in migration_requests[1:]:
            assert body["messages"][:len(first_wire)] == first_wire, "migration changed an encoded prefix"
            signed = next(message for message in body["messages"]
                if any(block.get("id") == "tool_1" for block in message["content"]))
            assert signed["content"][0] == {"type":"thinking", "thinking":"", "signature":"signed-1"}, signed
            assert signed["content"][1]["id"] == "tool_1", signed
            assert any(block.get("tool_use_id") == "tool_1"
                for message in body["messages"] for block in message["content"]), body
        assert migration_view()["entries"][:len(original_entries)] == original_entries, "migration rewrote committed history"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

print("Anthropic signed continuation, compaction/rebase, pre-output retry and reopened tool-ID migration: ok")
