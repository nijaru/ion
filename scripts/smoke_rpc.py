"""Exercise the built long-lived JSONL client against a local streaming route."""

import base64
import json
import os
import select
import struct
import subprocess
import tempfile
import threading
import time
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        users = [message for message in body["messages"] if message.get("role") == "user"]
        slow = bool(users) and "SLOW" in str(users[-1].get("content"))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def event(delta, reason=None):
            payload = {"id": "rpc", "choices": [{"index": 0, "delta": delta, "finish_reason": reason}]}
            self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
            self.wfile.flush()

        try:
            if users and "IO_SETTLEMENT" in str(users[-1].get("content")):
                arguments = {"command": "printf '%s\\n' $$ > io.pid; printf 'OBSERVED_BEFORE_DISCONNECT\\n'; touch io.ready; exec sleep 30", "timeout_ms": 120000}
                event({"tool_calls": [{"index": 0, "id": "io-call", "type": "function", "function": {"name": "exec", "arguments": json.dumps(arguments)}}]}, "tool_calls")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
                return
            event({"content": "PROVISIONAL" if slow else "RPC_OK"})
            if slow:
                time.sleep(2)
            event({}, "stop")
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except BrokenPipeError:
            pass

    def log_message(self, *_args):
        pass


def send(child, command):
    child.stdin.write(json.dumps(command).encode() + b"\n")
    child.stdin.flush()


def read(child):
    assert select.select([child.stdout], [], [], 8)[0], "RPC output timed out"
    line = child.stdout.readline()
    assert line, f"RPC ended unexpectedly: {child.poll()}"
    return json.loads(line)


def until(child, predicate):
    records = []
    while True:
        record = read(child)
        records.append(record)
        if predicate(record):
            return records


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def tiny_png():
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00\xff"))
            + chunk(b"IEND", b""))


with tempfile.TemporaryDirectory(prefix="ion-rpc-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / ".git").mkdir()
    (workspace / "pixel.png").write_bytes(tiny_png())
    prompts = workspace / ".ion" / "prompts"
    prompts.mkdir(parents=True)
    (prompts / "check.md").write_text("Check $1.\n")
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    child = None
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "rpc-smoke", "rpc-model", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "use", "rpc-alt", "alt-model", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        subprocess.run([binary, "use", "rpc-smoke", "rpc-model"], env=env, check=True, capture_output=True)
        child = subprocess.Popen([binary, "--cwd", workspace, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        ready = read(child)
        assert ready["type"] == "ready" and ready["cwd"] == str(workspace.resolve()), ready
        session = ready["session"]

        child.stdin.write(b"{bad}\n")
        child.stdin.flush()
        assert read(child)["command"] == "parse"
        send(child, {"id": "resources", "type": "list_resources"})
        resources = read(child)
        assert resources["id"] == "resources" and resources["success"]
        assert resources["data"]["prompts"][0]["name"] == "check"

        send(child, {"id": "missing-image", "type": "prompt", "message": "Look", "images": ["missing.png"]})
        rejected = read(child)
        assert rejected["id"] == "missing-image" and rejected["success"] is False
        send(child, {"id": "empty-state", "type": "get_state"})
        assert read(child)["data"]["entries"] == 0

        send(child, {"id": "first", "type": "prompt", "message": "/check RPC"})
        records = until(child, lambda r: r["type"] == "turn_end")
        accepted = [r for r in records if r.get("id") == "first"]
        assert len(accepted) == 1 and accepted[0]["success"]
        assert records[-1]["status"] == "completed"
        assert any(r["type"] == "final" and r["text"] == "RPC_OK" for r in records)
        committed = [r for r in records if r["type"] == "assistant_committed"]
        assert len(committed) == 1 and committed[0]["turn"] == accepted[0]["data"]["turn"], records
        assert "RPC_OK" in str(committed[0]["content"]), committed
        assert records.index(committed[0]) < next(i for i, r in enumerate(records) if r["type"] == "final"), records
        assert "Check RPC." in str(requests[0]["messages"])

        send(child, {"id": "fragment-parent", "type": "prompt", "message": "SLOW"})
        until(child, lambda r: r.get("id") == "fragment-parent")
        fragmented = json.dumps({"id": "fragment-state", "type": "get_state"}).encode()
        child.stdin.write(fragmented[:-2])
        child.stdin.flush()
        # Progress and completion must not discard an unfinished command frame.
        records = until(child, lambda r: r["type"] == "turn_end")
        assert records[-1]["status"] == "completed", records
        child.stdin.write(fragmented[-2:] + b"\n")
        child.stdin.flush()
        state = read(child)
        assert state.get("id") == "fragment-state" and state["success"], state
        assert state["data"]["busy"] is False, state

        send(child, {"id": "compact", "type": "compact"})
        compact_ack = read(child)
        assert compact_ack["id"] == "compact" and compact_ack["success"]
        assert compact_ack["data"]["disposition"] == "started", compact_ack
        compact_records = until(child, lambda r: r["type"] == "compact_end")
        assert compact_records[-1]["id"] == "compact"
        assert compact_records[-1]["status"] == "completed", compact_records

        inline = {"mime_type": "image/png", "data": base64.b64encode(tiny_png()).decode()}
        send(child, {"id": "bad-inline", "type": "prompt", "message": "Look", "images": [{**inline, "mime_type": "image/jpeg"}]})
        rejected = read(child)
        assert rejected["id"] == "bad-inline" and rejected["success"] is False
        send(child, {"id": "inline", "type": "prompt", "message": "", "images": [inline]})
        records = until(child, lambda r: r["type"] == "turn_end")
        assert records[-1]["status"] == "completed", records
        latest = [message for message in requests[-1]["messages"] if message["role"] == "user"][-1]["content"]
        assert len(latest) == 1 and latest[0]["type"] == "image_url", latest
        assert latest[0]["image_url"]["url"].startswith("data:image/png;base64,"), latest
        send(child, {"id": "inline-inspect", "type": "inspect"})
        inspected = read(child)
        assert inspected["success"] and "base64 image data omitted" in str(inspected["data"])

        send(child, {"id": "follow-parent", "type": "prompt", "message": "SLOW"})
        until(child, lambda r: r.get("id") == "follow-parent")
        send(child, {"id": "follow-child", "type": "follow_up", "message": "AFTER", "images": [inline]})
        queued = until(child, lambda r: r.get("id") == "follow-child")[-1]
        assert queued["success"] and queued["data"]["disposition"] == "queued", queued
        parent_records = until(child, lambda r: r["type"] == "turn_end")
        assert parent_records[-1]["status"] == "completed"
        follow_records = until(child, lambda r: r["type"] == "turn_end")
        started = [r for r in follow_records if r["type"] == "follow_up_started"]
        assert len(started) == 1 and started[0]["id"] == "follow-child", follow_records
        assert follow_records[-1]["status"] == "completed"
        latest = [message for message in requests[-1]["messages"] if message["role"] == "user"][-1]["content"]
        assert latest[0] == {"type": "text", "text": "AFTER"} and latest[1]["type"] == "image_url", latest

        send(child, {"id": "cancel-parent", "type": "prompt", "message": "SLOW"})
        until(child, lambda r: r.get("id") == "cancel-parent")
        send(child, {"id": "after-abort", "type": "follow_up", "message": "AFTER_ABORT"})
        assert until(child, lambda r: r.get("id") == "after-abort")[-1]["success"]
        send(child, {"id": "cancel-parent-now", "type": "abort"})
        parent_records = until(child, lambda r: r["type"] == "turn_end")
        assert any(r.get("id") == "cancel-parent-now" and r["success"] for r in parent_records)
        assert parent_records[-1]["status"] == "cancelled", parent_records
        follow_records = until(child, lambda r: r["type"] == "turn_end")
        assert any(r["type"] == "follow_up_started" and r["id"] == "after-abort" for r in follow_records)
        assert follow_records[-1]["status"] == "completed", follow_records

        send(child, {"id": "vision-turn", "type": "prompt", "message": "SLOW"})
        until(child, lambda r: r.get("id") == "vision-turn")
        send(child, {"id": "vision-steer", "type": "steer", "message": "Inspect the image", "images": ["pixel.png"]})
        steered = until(child, lambda r: r.get("id") == "vision-steer")[-1]
        assert steered["success"], steered
        records = until(child, lambda r: r["type"] == "turn_end")
        assert records[-1]["status"] == "completed", records[-1]
        commits = [r for r in records if r["type"] in {"assistant_committed", "steering_committed"}]
        assert [r["type"] for r in commits] == ["assistant_committed", "steering_committed", "assistant_committed"], records
        assert "Inspect the image" in str(commits[1]["input"]["content"]), commits
        latest = [message for message in requests[-1]["messages"] if message["role"] == "user"][-1]["content"]
        assert latest[0] == {"type": "text", "text": "Inspect the image"}, latest
        assert latest[1]["type"] == "image_url" and latest[1]["image_url"]["url"].startswith("data:image/png;base64,"), latest
        send(child, {"id": "vision-inspect", "type": "inspect"})
        inspected = read(child)
        assert inspected["success"] and "base64 image data omitted" in str(inspected["data"])

        send(child, {"id": "state", "type": "get_state"})
        state = read(child)
        assert state["id"] == "state" and state["data"]["busy"] is False
        send(child, {"id": "slow", "type": "prompt", "message": "SLOW"})
        until(child, lambda r: r.get("id") == "slow")
        send(child, {"id": "blocked", "type": "new_session"})
        blocked = until(child, lambda r: r.get("id") == "blocked")[-1]
        assert blocked["success"] is False  # A running Turn owns its Session.
        send(child, {"id": "clear-me", "type": "follow_up", "message": "NEVER"})
        assert until(child, lambda r: r.get("id") == "clear-me")[-1]["success"]
        send(child, {"id": "clear", "type": "clear_queue"})
        cleared = until(child, lambda r: r.get("id") == "clear")[-1]
        assert cleared["success"] and len(cleared["data"]["follow_up"]) == 1, cleared
        assert cleared["data"]["follow_up"][0]["id"] == "clear-me", cleared
        send(child, {"id": "abort", "type": "abort"})
        records = until(child, lambda r: r["type"] == "turn_end")
        assert any(r.get("id") == "abort" and r["success"] for r in records)
        assert records[-1]["status"] == "cancelled", records

        send(child, {"id": "model-alt", "type": "set_model", "provider": "rpc-alt", "model": "alt-model"})
        changed = read(child)
        assert changed["success"] and changed["data"]["model"]["provider"] == "rpc-alt", changed
        send(child, {"id": "model-inspect", "type": "inspect"})
        inspected = read(child)
        assert inspected["success"] and inspected["data"]["last_model"]["provider"] == "rpc-alt", inspected
        source_before_clone = inspected["data"]

        send(child, {"id": "clone", "type": "clone_session"})
        cloned = read(child)
        assert cloned["success"] and cloned["data"]["session"] != session, cloned
        assert cloned["data"]["model"]["provider"] == "rpc-alt", cloned
        send(child, {"id": "clone-inspect", "type": "inspect"})
        clone_view = read(child)
        assert clone_view["success"], clone_view
        assert clone_view["data"]["entries"] == source_before_clone["entries"], clone_view
        assert clone_view["data"]["cwd"] == source_before_clone["cwd"], clone_view
        assert clone_view["data"]["last_model"]["provider"] == "rpc-alt", clone_view
        send(child, {"id": "clone-source", "type": "switch_session", "session": session})
        source_again = read(child)
        assert source_again["success"] and source_again["data"]["session"] == session, source_again
        assert source_again["data"]["model"]["provider"] == "rpc-alt", source_again

        (prompts / "late.md").write_text("A later prompt.\n")
        send(child, {"id": "new", "type": "new_session"})
        fresh = read(child)
        assert fresh["success"] and fresh["data"]["session"] != session, fresh
        assert fresh["data"]["model"]["provider"] == "rpc-smoke", fresh
        send(child, {"id": "new-resources", "type": "list_resources"})
        updated = read(child)
        assert updated["success"] and {prompt["name"] for prompt in updated["data"]["prompts"]} == {"check", "late"}, updated
        send(child, {"id": "fresh-prompt", "type": "prompt", "message": "Fresh session"})
        assert until(child, lambda r: r["type"] == "turn_end")[-1]["status"] == "completed"
        assert requests[-1]["model"] == "rpc-model", requests[-1]
        moved = work / "workspace-moved"
        workspace.rename(moved)
        try:
            send(child, {"id": "switch-unavailable", "type": "switch_session", "session": session})
            failed = read(child)
            assert failed["id"] == "switch-unavailable" and failed["success"] is False, failed
            send(child, {"id": "state-after-failed-switch", "type": "get_state"})
            unchanged = read(child)
            assert unchanged["success"] and unchanged["data"]["session"] == fresh["data"]["session"], unchanged
            assert unchanged["data"]["model"]["provider"] == "rpc-smoke", unchanged
        finally:
            moved.rename(workspace)
        send(child, {"id": "switch", "type": "switch_session", "session": session})
        switched = read(child)
        assert switched["success"] and switched["data"]["session"] == session, switched
        assert switched["data"]["model"]["provider"] == "rpc-alt", switched
        send(child, {"id": "switch-same", "type": "switch_session", "session": session})
        same = read(child)
        assert same["success"] and same["data"]["session"] == session, same
        send(child, {"id": "switched-prompt", "type": "prompt", "message": "Switched session"})
        assert until(child, lambda r: r["type"] == "turn_end")[-1]["status"] == "completed"
        assert requests[-1]["model"] == "alt-model", requests[-1]
        child.stdin.close()
        assert child.wait(timeout=8) == 0, child.stderr.read()
        reopened = subprocess.Popen([binary, "--cwd", workspace, "--session", session, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        assert read(reopened)["type"] == "ready"
        send(reopened, {"id": "reopened-model", "type": "get_state"})
        selected = read(reopened)
        assert selected["success"] and selected["data"]["model"]["provider"] == "rpc-alt", selected
        reopened.stdin.close()
        assert reopened.wait(timeout=8) == 0, reopened.stderr.read()
        closing = subprocess.Popen([binary, "--cwd", workspace, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        assert read(closing)["type"] == "ready"
        send(closing, {"id": "closing", "type": "prompt", "message": "SLOW"})
        until(closing, lambda r: r.get("id") == "closing")
        send(closing, {"id": "uncommitted", "type": "follow_up", "message": "LATER"})
        assert until(closing, lambda r: r.get("id") == "uncommitted")[-1]["success"]
        closing.stdin.close()
        settled = until(closing, lambda r: r["type"] == "turn_end")[-1]
        assert settled["status"] == "cancelled", settled
        returned = until(closing, lambda r: r["type"] == "uncommitted_follow_up")[-1]
        assert returned["id"] == "uncommitted" and returned["input"]["content"][0]["Text"] == "LATER", returned
        assert closing.wait(timeout=8) == 0, closing.stderr.read()
        incomplete = subprocess.run(
            [binary, "--cwd", workspace, "rpc"],
            env=env,
            input=b'{"type":"get_state"}',
            capture_output=True,
            timeout=8,
            check=True,
        )
        records = [json.loads(line) for line in incomplete.stdout.splitlines()]
        assert [record["type"] for record in records] == ["ready", "response"], records
        assert records[1]["command"] == "parse" and "final newline" in records[1]["error"], records
        disconnected = subprocess.Popen([binary, "--cwd", workspace, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        pid = None
        try:
            disconnected_session = read(disconnected)["session"]
            before = len(requests)
            send(disconnected, {"type": "prompt", "message": "IO_SETTLEMENT"})
            until(disconnected, lambda record: record["type"] == "tool_started")
            deadline = time.monotonic() + 8
            while not (workspace / "io.ready").exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            assert (workspace / "io.ready").exists(), "native command did not become ready"
            pid = int((workspace / "io.pid").read_text())
            disconnected.stdout.close()
            send(disconnected, {"type": "get_state"})
            assert disconnected.wait(timeout=8) != 0, "broken output was reported as success"
            assert b"Broken pipe" in disconnected.stderr.read()
            view = json.loads(subprocess.run([binary, "--cwd", workspace, "--session", disconnected_session, "inspect"], env=env, capture_output=True, check=True).stdout)
            results = [entry["data"]["result"] for entry in view["entries"] if entry["kind"] == "tool_result"]
            assert len(results) == 1, ("native outcome was not committed on output failure", [entry["kind"] for entry in view["entries"]], view["unfinished_turn"])
            output = results[0]["result"]
            assert output["stdout"] == "OBSERVED_BEFORE_DISCONNECT\n" and output["cancelled"] is True, output
            assert output["signal"] is not None and output["wait_error"] is None, output
            assert view["unfinished_turn"] is None and view["entries"][-1]["data"]["reason"] == "cancelled", view
            assert len(requests) == before + 1, "disconnect dispatched another model request"
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                pid = None
            else:
                raise AssertionError("direct native command still exists after RPC exit")
        finally:
            if disconnected.poll() is None:
                disconnected.kill()
                disconnected.wait()
            if pid is not None:
                try:
                    os.kill(pid, 9)
                except ProcessLookupError:
                    pass
            disconnected.stdin.close()
        print("Ion RPC acceptance, compaction, inline images, steering, follow-ups, settlement, abort and session clone/control: OK")
    finally:
        if child and child.poll() is None:
            child.kill()
            child.wait()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
