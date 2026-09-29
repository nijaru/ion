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
        assert "Check RPC." in str(requests[0]["messages"])

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

        send(child, {"id": "vision-turn", "type": "prompt", "message": "SLOW"})
        until(child, lambda r: r.get("id") == "vision-turn")
        send(child, {"id": "vision-steer", "type": "steer", "message": "Inspect the image", "images": ["pixel.png"]})
        steered = until(child, lambda r: r.get("id") == "vision-steer")[-1]
        assert steered["success"], steered
        records = until(child, lambda r: r["type"] == "turn_end")
        assert records[-1]["status"] == "completed", records[-1]
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
        send(child, {"id": "abort", "type": "abort"})
        records = until(child, lambda r: r["type"] == "turn_end")
        assert any(r.get("id") == "abort" and r["success"] for r in records)
        assert records[-1]["status"] == "cancelled", records

        send(child, {"id": "new", "type": "new_session"})
        fresh = read(child)
        assert fresh["success"] and fresh["data"]["session"] != session
        send(child, {"id": "switch", "type": "switch_session", "session": session})
        switched = read(child)
        assert switched["success"] and switched["data"]["session"] == session
        child.stdin.close()
        assert child.wait(timeout=8) == 0, child.stderr.read()
        closing = subprocess.Popen([binary, "--cwd", workspace, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        assert read(closing)["type"] == "ready"
        send(closing, {"id": "closing", "type": "prompt", "message": "SLOW"})
        until(closing, lambda r: r.get("id") == "closing")
        closing.stdin.close()
        settled = until(closing, lambda r: r["type"] == "turn_end")[-1]
        assert settled["status"] == "cancelled", settled
        assert closing.wait(timeout=8) == 0, closing.stderr.read()
        print("Ion RPC acceptance, inline images, typed image steering, settlement, abort, resources and session control: OK")
    finally:
        if child and child.poll() is None:
            child.kill()
            child.wait()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
