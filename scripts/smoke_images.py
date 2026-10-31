"""Exercise image acceptance, wire replay, inspection and terminal attachment."""

import base64
import fcntl
import json
import os
import pty
import select
import signal
import sqlite3
import struct
import subprocess
import tempfile
import termios
import threading
import time
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
requests = []
long_image_path = "long-subject-" + "x" * 96 + "/red.png"
rpc_request_entered = threading.Event()
release_rpc_request = threading.Event()
raw_inspection_json = {"mime_type": "image/png", "data": "RAW_JSON_VALUE", "nested": {"Image": {"mime_type": "image/jpeg", "data": "NESTED_JSON_VALUE"}}, "images": [{"mime_type": "image/png", "data": "NOT_AN_ATTACHMENT"}]}


def chunk(kind, payload):
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)


def tiny_png():
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00\xff"))
        + chunk(b"IEND", b"")
    )


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        latest_user_index = max(index for index, message in enumerate(body["messages"]) if message["role"] == "user")
        latest_user = body["messages"][latest_user_index]
        held_rpc_request = isinstance(latest_user["content"], list) and latest_user["content"][0].get("text") == "RPC_IMAGE_HOLD"
        if held_rpc_request:
            rpc_request_entered.set()
            assert release_rpc_request.wait(15), "RPC image request was never released"
        if isinstance(latest_user["content"], list) and latest_user["content"][0].get("text") == "INSPECTION_JSON" and not any(message["role"] == "tool" for message in body["messages"][latest_user_index + 1:]):
            code = "return " + json.dumps(raw_inspection_json) + ";"
            events = [
                {"id": "raw-json", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "raw-json", "type": "function", "function": {"name": "code_mode", "arguments": json.dumps({"code": code})}}]}, "finish_reason": None}]},
                {"id": "raw-json", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        elif any(message.get("content") in ("Read workspace picture.", "Read unshared picture.") for message in body["messages"]) and not any(message["role"] == "tool" for message in body["messages"]):
            image_path = long_image_path if any(message.get("content") == "Read unshared picture." for message in body["messages"]) else "red.png"
            events = [
                {"id": "images", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "read-picture", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": image_path})}}]}, "finish_reason": None}]},
                {"id": "images", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
            ]
        else:
            events = [
                {"id": "images", "choices": [{"index": 0, "delta": {"content": "IMAGE_OK"}, "finish_reason": None}]},
                {"id": "images", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            # The owned RPC cancellation case deliberately drops its request.
            if not held_rpc_request:
                raise

    def log_message(self, *_args):
        pass


def user_image_request(request, expected_text):
    users = [message for message in request["messages"] if message["role"] == "user"]
    first = users[0]["content"]
    assert first[0] == {"type": "text", "text": expected_text}, first
    assert first[1]["type"] == "image_url" and first[1]["image_url"]["url"].startswith("data:image/png;base64,"), first
    return users


with tempfile.TemporaryDirectory(prefix="ion-images-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "red.png").write_bytes(tiny_png())
    (workspace / "bad.png").write_bytes(b"\x89PNG\r\n\x1a\ninvalid")
    env = {**os.environ, "ION_HOME": str(work / "ion"), "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "vision", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        first = subprocess.run([binary, "--cwd", workspace, "--image", "red.png", "run", "Inspect the picture."], env=env, check=True, capture_output=True, text=True)
        assert first.stdout.strip() == "IMAGE_OK", first
        resumed = subprocess.run([binary, "--cwd", workspace, "--continue", "run", "Continue."], env=env, check=True, capture_output=True, text=True)
        assert resumed.stdout.strip() == "IMAGE_OK", resumed
        assert len(requests) == 2
        user_image_request(requests[0], "Inspect the picture.")
        users = user_image_request(requests[1], "Inspect the picture.")
        assert users[1]["content"] == "Continue.", users
        inspect = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, check=True, capture_output=True, text=True).stdout
        assert "base64 image data omitted" in inspect
        assert "iVBORw0KGgo" not in inspect
        invalid = subprocess.run([binary, "--cwd", workspace, "--image", "bad.png", "run", "Inspect."], env=env, capture_output=True, text=True)
        assert invalid.returncode != 0 and "invalid image" in invalid.stderr
        assert len(requests) == 2
        subprocess.run([binary, "use", "smoke", "text-only", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        unsupported = subprocess.run([binary, "--cwd", workspace, "--image", "red.png", "run", "Inspect."], env=env, capture_output=True, text=True)
        assert unsupported.returncode != 0 and "does not declare image input" in unsupported.stderr
        assert len(requests) == 2
        subprocess.run([binary, "use", "smoke", "vision", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def attach_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        terminal_session = work / "terminal.sqlite"
        child = subprocess.Popen([binary, "--cwd", workspace, "--session", terminal_session, "--image", "red.png", "--tui-mode", "fullscreen", "chat"], env={**env, "TERM": "xterm-256color"}, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
        os.close(slave)
        output = bytearray()
        sent_first = attached_second = sent_second = quit_sent = False
        deadline = time.monotonic() + 12
        try:
            while time.monotonic() < deadline:
                readable, _, _ = select.select([master], [], [], 0.05)
                if readable:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        data = b""
                    output.extend(data)
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[2;1R")
                if b"\x1b[?1049h" in output and not sent_first:
                    os.write(master, b"Inspect in terminal.\r")
                    sent_first = True
                if sent_first and len(requests) >= 3 and b"IMAGE_OK" in output and not attached_second:
                    os.write(master, b"/image red.png\r")
                    attached_second = True
                if attached_second and b"image(s) attached to the next prompt" in output and not sent_second:
                    os.write(master, b"Inspect again.\r")
                    sent_second = True
                if sent_second and len(requests) >= 4 and not quit_sent:
                    with sqlite3.connect(terminal_session) as connection:
                        ended = connection.execute("SELECT COUNT(*) FROM entries WHERE json_extract(CAST(body AS TEXT), '$.kind') = 'turn_ended'").fetchone()[0]
                    if ended == 2:
                        # A request reaching the peer is not turn settlement.
                        # Quit explicitly after both committed image turns.
                        os.write(master, b"\x04")
                        quit_sent = True
                # Leaving the alternate screen is only one teardown write.
                # Keep draining the PTY until exit so restoration cannot block
                # on a full terminal output queue (notably on macOS).
                if child.poll() is not None:
                    break
            child.wait(timeout=5)
            assert child.returncode == 0, output[-1000:]
            assert sent_second and len(requests) == 4 and b"\x1b[?1049l" in output
            user_image_request(requests[2], "Inspect in terminal.")
            user_image_request(requests[3], "Inspect in terminal.")
            last_user = [message for message in requests[3]["messages"] if message["role"] == "user"][-1]["content"]
            assert last_user[0] == {"type": "text", "text": "Inspect again."}
            assert last_user[1]["type"] == "image_url"
        finally:
            os.close(master)
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait(timeout=5)
        tool_read = subprocess.run([binary, "--cwd", workspace, "run", "Read workspace picture."], env=env, check=True, capture_output=True, text=True)
        assert tool_read.stdout.strip() == "IMAGE_OK", tool_read
        assert len(requests) == 6
        assert requests[4]["messages"][-1]["content"] == "Read workspace picture."
        tool_messages = requests[5]["messages"]
        assert tool_messages[-2]["role"] == "tool"
        assert json.loads(tool_messages[-2]["content"]) == {"is_error": False, "result": {"content": "Read image file [image/png]", "note": None, "path": "red.png"}}
        assert tool_messages[-1]["role"] == "user"
        assert tool_messages[-1]["content"][1]["image_url"]["url"].startswith("data:image/png;base64,")
        inspected = subprocess.run([binary, "--cwd", workspace, "--continue", "inspect"], env=env, check=True, capture_output=True, text=True).stdout
        assert "base64 image data omitted" in inspected
        assert "iVBORw0KGgo" not in inspected
        exported = subprocess.run([binary, "--cwd", workspace, "--continue", "export"], env=env, check=True, capture_output=True, text=True).stdout
        assert "[image: image/png]" in exported
        assert "iVBORw0KGgo" not in exported
        # Model delivery limits must not turn a successful native read into a
        # failed saved activity, or discard its image from full inspection.
        (workspace / long_image_path).parent.mkdir()
        (workspace / long_image_path).write_bytes(tiny_png())
        subprocess.run([binary, "use", "smoke", "text-only", "--endpoint", endpoint, "--wire", "chat-completions"], env=env, check=True, capture_output=True)
        withheld = subprocess.run([binary, "--cwd", workspace, "--json", "run", "Read unshared picture."], env=env, check=True, capture_output=True, text=True)
        records = [json.loads(line) for line in withheld.stdout.splitlines()]
        observed = next(record for record in records if record["type"] == "tool_finished")
        assert observed["model_projection"] == "images_unsupported", observed
        assert observed["is_error"] is False and observed["output"]["path"] == long_image_path
        assert observed["image_mime_types"] == ["image/png"]
        assert len(requests) == 8
        projected = next(message for message in requests[7]["messages"] if message["role"] == "tool")
        projected_output = json.loads(projected["content"])
        assert projected_output["is_error"] is False, projected_output
        assert projected_output["result"]["output_withheld"] == "images_unsupported", projected_output
        assert projected_output["result"]["notice"] and "error" not in projected_output["result"], projected_output
        assert not any(
            part.get("type") == "image_url"
            for message in requests[7]["messages"]
            if isinstance(message.get("content"), list)
            for part in message["content"]
        )
        session_id = next(record["id"] for record in records if record["type"] == "session")
        exported = subprocess.run([binary, "--cwd", workspace, "--session", session_id, "export"], env=env, check=True, capture_output=True, text=True).stdout
        assert "Tool result · read · read-picture · ok" in exported
        assert "Image result not shared with this text-only model" in exported
        assert "[image: image/png]" in exported and "iVBORw0KGgo" not in exported

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 80, 0, 0))
        child = subprocess.Popen([binary, "--cwd", workspace, "--session", session_id, "--tui-mode", "fullscreen", "chat"], env={**env, "TERM": "xterm-256color"}, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
        os.close(slave)
        output = bytearray()
        detail_sent = detail_closed = quit_sent = False
        deadline = time.monotonic() + 12
        try:
            while time.monotonic() < deadline:
                readable, _, _ = select.select([master], [], [], 0.05)
                if readable:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        data = b""
                    output.extend(data)
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[2;1R")
                if not detail_sent and b"result not shared with model" in output:
                    assert b"Read" in output and b"Read failed" not in output
                    output.clear()  # Detail must qualify from fresh output.
                    os.write(master, b"\x0f")
                    detail_sent = True
                if detail_sent and not detail_closed and b"Image result not shared with this text-only model" in output and b"[image: image/png]" in output:
                    os.write(master, b"\x1b")
                    output.clear()
                    detail_closed = True
                if detail_closed and not quit_sent and b"result not shared with model" in output:
                    os.write(master, b"\x04")  # Quit also joins a settling Turn.
                    quit_sent = True
                if child.poll() is not None:
                    break
            child.wait(timeout=5)
            assert child.returncode == 0 and detail_sent and quit_sent, output[-1500:]
        finally:
            os.close(master)
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait(timeout=5)
        # Summaries must carry pixels as typed images, never base64 transcript
        # text. Exercise the actual compact/reopen entry point on a fresh scope.
        summary_work = work / "summary-workspace"
        summary_work.mkdir()
        (summary_work / "red.png").write_bytes(tiny_png())
        subprocess.run([binary, "use", "smoke", "vision", "--endpoint", endpoint, "--wire", "chat-completions", "--images"], env=env, check=True, capture_output=True)
        start = len(requests)
        subprocess.run([binary, "--cwd", summary_work, "--image", "red.png", "run", "Retain the visual findings."], env=env, check=True, capture_output=True)
        before = json.loads(subprocess.run([binary, "--cwd", summary_work, "--continue", "inspect"], env=env, check=True, capture_output=True, text=True).stdout)["entries"]
        subprocess.run([binary, "--cwd", summary_work, "--continue", "compact"], env=env, check=True, capture_output=True)
        assert len(requests) == start + 2
        summary_request = requests[-1]
        assert not summary_request.get("tools"), summary_request
        parts = summary_request["messages"][-1]["content"]
        assert isinstance(parts, list), parts
        image_parts = [part for part in parts if part["type"] == "image_url"]
        assert len(image_parts) == 1 and image_parts[0]["image_url"]["url"].startswith("data:image/png;base64,")
        assert not any("iVBORw0KGgo" in part.get("text", "") for part in parts)
        after = json.loads(subprocess.run([binary, "--cwd", summary_work, "--continue", "inspect"], env=env, check=True, capture_output=True, text=True).stdout)
        assert after["entries"][:len(before)] == before and after["compacted_through"] is not None
        subprocess.run([binary, "--cwd", summary_work, "--continue", "run", "Continue after the visual summary."], env=env, check=True, capture_output=True)
        assert len(requests) == start + 3
        assert not any("iVBORw0KGgo" in json.dumps(message.get("content")) for message in requests[-1]["messages"])
        # Unit preparation tests inject ready messages/tasks. Exercise actual
        # normalization and image custody through the built RPC connection too.
        rpc_work = work / "rpc-workspace"
        rpc_work.mkdir()
        (rpc_work / "red.png").write_bytes(tiny_png())
        rpc = subprocess.Popen([binary, "--cwd", rpc_work, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)

        def rpc_read():
            assert select.select([rpc.stdout], [], [], 10)[0], "image RPC output timed out"
            line = rpc.stdout.readline()
            assert line, f"image RPC closed: {rpc.poll()}"
            return json.loads(line)

        def rpc_send(value):
            rpc.stdin.write((json.dumps(value) + "\n").encode())

        def rpc_until(predicate):
            while True:
                record = rpc_read()
                if predicate(record):
                    return record

        def returned_image(record, text, source):
            assert record["reason"] == "cleared", record
            assert record["input"] == {"Prompt": {"text": text, "images": [source], "files": []}}, record

        try:
            rpc_session = rpc_read()["session"]
            before_rpc = len(requests)
            rpc_send({"id": "picture", "type": "prompt", "message": "RPC_IMAGE_HOLD", "images": ["red.png"]})
            assert rpc_until(lambda record: record.get("id") == "picture")["success"]
            assert rpc_request_entered.wait(8), "RPC image never reached the captured route"
            assert len(requests) == before_rpc + 1
            user_image_request(requests[-1], "RPC_IMAGE_HOLD")
            inline = {"mime_type": "image/png", "data": base64.b64encode(tiny_png()).decode()}
            rpc_send({"id": "bad-picture", "type": "steer", "message": "REJECTED_PICTURE", "images": [inline, {**inline, "mime_type": "image/jpeg"}]})
            rejected = rpc_until(lambda record: record.get("id") == "bad-picture")
            assert not rejected["success"] and "MIME type" in rejected["error"], rejected
            for command, text, source in (("steer", "RETURNED_STEERING_PICTURE", inline), ("follow_up", "RETURNED_FOLLOW_UP_PICTURE", "red.png")):
                rpc_send({"id": command, "type": command, "message": text, "images": [source]})
                assert rpc_until(lambda record: record.get("id") == command)["success"]
            rpc_send({"id": "clear", "type": "clear_queue"})
            assert rpc_until(lambda record: record.get("id") == "clear")["success"]
            returned = [rpc_read(), rpc_read()]
            assert {(record["type"], record["id"]) for record in returned} == {("uncommitted_steering", "steer"), ("uncommitted_follow_up", "follow_up")}, returned
            by_id = {record["id"]: record for record in returned}
            returned_image(by_id["steer"], "RETURNED_STEERING_PICTURE", {"Encoded": inline})
            returned_image(by_id["follow_up"], "RETURNED_FOLLOW_UP_PICTURE", {"Path": "red.png"})
            rpc_send({"id": "stop-picture", "type": "abort"})
            assert rpc_until(lambda record: record.get("id") == "stop-picture")["success"]
            assert rpc_until(lambda record: record["type"] == "turn_end")["status"] == "cancelled"
            release_rpc_request.set()
            rpc_send({"id": "recover-picture", "type": "prompt", "message": "RPC_IMAGE_RECOVERY", "images": ["red.png"]})
            assert rpc_until(lambda record: record.get("id") == "recover-picture")["success"]
            assert rpc_until(lambda record: record["type"] == "turn_end")["status"] == "completed"
            assert len(requests) == before_rpc + 2
            rpc.stdin.close()
            rpc.wait(timeout=8)
            assert rpc.returncode == 0, rpc.stderr.read()
        finally:
            release_rpc_request.set()
            if rpc.poll() is None:
                rpc.kill()
                rpc.wait(timeout=8)
            if not rpc.stdin.closed:
                rpc.stdin.close()
            rpc.stdout.close()
            rpc.stderr.close()
        rpc_view = json.loads(subprocess.run([binary, "--cwd", rpc_work, "--session", rpc_session, "inspect"], env=env, check=True, capture_output=True, text=True).stdout)
        assert rpc_view["unfinished_turn"] is None, rpc_view
        assert [entry["data"]["reason"] for entry in rpc_view["entries"] if entry["kind"] == "turn_ended"] == ["cancelled", "completed"], rpc_view
        # Reopen must retain accepted images, not resurrect withdrawn queues.
        subprocess.run([binary, "--cwd", rpc_work, "--session", rpc_session, "run", "RPC_IMAGE_REOPEN"], env=env, check=True, capture_output=True)
        replay_users = user_image_request(requests[-1], "RPC_IMAGE_HOLD")
        assert replay_users[1]["content"][0] == {"type": "text", "text": "RPC_IMAGE_RECOVERY"}, replay_users
        assert replay_users[1]["content"][1]["type"] == "image_url", replay_users
        assert not any(marker in json.dumps(requests[-1]) for marker in ("REJECTED_PICTURE", "RETURNED_STEERING_PICTURE", "RETURNED_FOLLOW_UP_PICTURE"))
        # Real attachments and image-shaped raw values coexist in one Session.
        # CLI/RPC inspection must omit only the former, including after reopen.
        raw_work = work / "raw-inspection"
        raw_work.mkdir()
        (raw_work / "red.png").write_bytes(tiny_png())
        raw_run = subprocess.run([binary, "--cwd", raw_work, "--code-mode", "--image", "red.png", "--json", "run", "INSPECTION_JSON"], env=env, check=True, capture_output=True, text=True)
        raw_records = [json.loads(line) for line in raw_run.stdout.splitlines()]
        observed = next(record for record in raw_records if record["type"] == "tool_finished" and record["name"] == "code_mode")
        assert observed["is_error"] is False and observed["output"]["result"] == raw_inspection_json, observed
        raw_session = next(record["id"] for record in raw_records if record["type"] == "session")
        cli_inspection = json.loads(subprocess.run([binary, "--cwd", raw_work, "--session", raw_session, "inspect"], env=env, check=True, capture_output=True, text=True).stdout)
        result = next(entry["data"]["result"] for entry in cli_inspection["entries"] if entry["kind"] == "tool_result")
        assert result["outcome"]["output"]["value"]["result"] == raw_inspection_json, result
        tool_message = next(message for message in cli_inspection["messages"] if message["role"] == "Tool")
        assert tool_message["content"][0]["ToolResult"]["result"]["result"] == raw_inspection_json, tool_message
        typed_image = next(entry["data"]["input"]["content"][1]["Image"] for entry in cli_inspection["entries"] if entry["kind"] == "turn_started")
        assert typed_image["mime_type"] == "image/png" and typed_image["data"].startswith("[base64 image data omitted:"), typed_image
        assert "iVBORw0KGgo" not in json.dumps(cli_inspection)
        rpc = subprocess.Popen([binary, "--cwd", raw_work, "--session", raw_session, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            assert rpc_read()["type"] == "ready"
            rpc_send({"type": "inspect", "id": "raw-inspection"})
            reply = rpc_until(lambda record: record.get("id") == "raw-inspection")
            assert reply["success"] and reply["data"] == cli_inspection, reply
            rpc.stdin.close()
            rpc.wait(timeout=8)
            assert rpc.returncode == 0, rpc.stderr.read()
        finally:
            if rpc.poll() is None:
                rpc.kill()
                rpc.wait(timeout=8)
            if not rpc.stdin.closed:
                rpc.stdin.close()
            rpc.stdout.close()
            rpc.stderr.close()
        print("Ion images, RPC custody/cancel/reopen, raw-JSON inspection fidelity and typed summary images: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
