"""Exercise image acceptance, wire replay, inspection and terminal attachment."""

import fcntl
import json
import os
import pty
import select
import signal
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
        if any(message.get("content") in ("Read workspace picture.", "Read unshared picture.") for message in body["messages"]) and not any(message["role"] == "tool" for message in body["messages"]):
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
        self.wfile.write(payload)

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
    env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
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

        child = subprocess.Popen([binary, "--cwd", workspace, "--image", "red.png", "--tui-mode", "fullscreen", "chat"], env={**env, "TERM": "xterm-256color"}, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
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
                    os.write(master, b"\x03")
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
        assert "Tool result · read · ok" in exported
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
                    os.write(master, b"\x03")
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
        print("Ion image input, resume, inspection, terminal attachment, withheld-result truth and typed summary images: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
