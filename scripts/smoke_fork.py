"""Exercise selected-point Session forks through the built CLI and terminal."""

import errno
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


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
        events = [
            {"id": "fork", "choices": [{"index": 0, "delta": {"content": "FORK_OK"}, "finish_reason": None}]},
            {"id": "fork", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        ]
        payload = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events) + b"data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


with tempfile.TemporaryDirectory(prefix="ion-fork-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    env = {**os.environ, "ION_HOME": str(work / "ion"), "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state")}
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}/v1/chat/completions"
        subprocess.run([binary, "use", "smoke", "fork-model", "--endpoint", endpoint, "--wire", "openrouter-chat"], env=env, check=True, capture_output=True)

        def ion(*args, check=True):
            return subprocess.run([binary, "--cwd", workspace, *args], env=env, capture_output=True, text=True, check=check)

        first = ion("run", "first request")
        assert first.stdout.strip() == "FORK_OK"
        source_id = first.stderr.split("[session: ")[1].split("]")[0]
        ion("--session", source_id, "run", "second request")
        source_provider_id = requests[0]["session_id"]
        assert requests[1]["session_id"] == source_provider_id
        assert len(source_provider_id) == 36
        listing = ion("--session", source_id, "turns").stdout
        assert "1\tended\tfirst request" in listing and "2\tended\tsecond request" in listing
        fork_id = ion("--session", source_id, "fork", "2").stdout.split()[-1]
        fork_view = json.loads(ion("--session", fork_id, "inspect").stdout)
        starts = [entry["data"]["turn"] for entry in fork_view["entries"] if entry["kind"] == "turn_started"]
        assert starts == [1], starts
        assert fork_view["last_model"] == {"provider": "smoke", "model": "fork-model"}
        ion("--session", fork_id, "run", "alternative second request")
        fork_provider_id = requests[-1]["session_id"]
        assert fork_provider_id != source_provider_id
        source_view = json.loads(ion("--session", source_id, "inspect").stdout)
        assert [entry["data"]["turn"] for entry in source_view["entries"] if entry["kind"] == "turn_started"] == [1, 2]
        # Failed confirmation does not undo a committed copy or retain its lease.
        for action, expected_turns, expected_messages in (
            (["clone"], [1, 2], source_view["messages"]),
            (["fork", "2"], [1], source_view["messages"][:2]),
        ):
            before_paths = set((work / "ion/sessions").rglob("*.sqlite"))
            before_requests = len(requests)
            reader, writer = os.pipe()
            os.close(reader)
            try:
                failed = subprocess.run([binary, "--cwd", workspace, "--session", source_id, *action], env=env, stdout=writer, stderr=subprocess.PIPE, timeout=8)
            finally:
                os.close(writer)
            assert failed.returncode == 1 and b"panicked" not in failed.stderr, failed
            children = set((work / "ion/sessions").rglob("*.sqlite")) - before_paths
            assert len(children) == 1, "failed confirmation lost or duplicated the saved copy"
            saved = children.pop()
            copied = json.loads(ion("--session", saved, "inspect").stdout)
            assert copied["id"] != source_id
            assert [entry["data"]["turn"] for entry in copied["entries"] if entry["kind"] == "turn_started"] == expected_turns
            assert copied["messages"] == expected_messages and copied["unfinished_turn"] is None, copied
            assert copied["last_model"] == source_view["last_model"]
            reopened = subprocess.Popen([binary, "--cwd", workspace, "--session", copied["id"], "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
            try:
                assert select.select([reopened.stdout], [], [], 8)[0], "copied Session writer remained leased"
                ready = json.loads(reopened.stdout.readline())
                assert ready["type"] == "ready" and ready["session"] == copied["id"], ready
                reopened.stdin.close()
                assert reopened.wait(timeout=8) == 0, reopened.stderr.read()
            finally:
                if reopened.poll() is None:
                    reopened.kill()
                    reopened.wait(timeout=5)
            assert json.loads(ion("--session", source_id, "inspect").stdout) == source_view, "copy failure changed source facts"
            assert len(requests) == before_requests, "copy confirmation failure dispatched model work"
        after_id = ion("--session", source_id, "fork", "1", "--after").stdout.split()[-1]
        after_view = json.loads(ion("--session", after_id, "inspect").stdout)
        assert [entry["data"]["turn"] for entry in after_view["entries"] if entry["kind"] == "turn_started"] == [1]
        ion("--session", after_id, "run", "continue after first request")
        after_provider_id = requests[-1]["session_id"]
        clone_id = ion("--session", source_id, "clone").stdout.split()[-1]
        ion("--session", clone_id, "run", "continue cloned conversation")
        clone_provider_id = requests[-1]["session_id"]
        assert len({source_provider_id, fork_provider_id, after_provider_id, clone_provider_id}) == 4
        before_compact = len(requests)
        ion("--session", source_id, "compact")
        assert len(requests) > before_compact, "compaction did not call the provider"
        assert all(request["session_id"] == source_provider_id for request in requests[before_compact:])
        assert all(request["session_id"] not in json.dumps(request["messages"]) for request in requests)
        missing = ion("--session", source_id, "fork", "99", check=False)
        assert missing.returncode != 0 and "boundary" in missing.stderr

        rpc = subprocess.Popen([binary, "--cwd", workspace, "--session", source_id, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            assert json.loads(rpc.stdout.readline())["type"] == "ready"
            rpc.stdin.write(b'{"id":"turns","type":"list_turns"}\n')
            rpc.stdin.flush()
            listed = json.loads(rpc.stdout.readline())
            assert [turn["turn"] for turn in listed["data"]] == [1, 2]
            rpc.stdin.write(b'{"id":"fork","type":"fork","turn":1,"after":true}\n')
            rpc.stdin.flush()
            forked = json.loads(rpc.stdout.readline())
            assert forked["success"] and forked["data"]["session"] != source_id
            rpc.stdin.close()
            assert rpc.wait(timeout=8) == 0
        finally:
            if rpc.poll() is None:
                rpc.kill()
                rpc.wait()

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

        def attach_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        child = subprocess.Popen([binary, "--cwd", workspace, "--session", source_id, "chat"], env={**env, "TERM": "xterm-256color"}, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
        os.close(slave)
        output = bytearray()
        sent = quit_sent = False
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
                if b"\xe2\x80\xba" in output and not sent:
                    assert b"\x1b[?1049h" not in output, "fork smoke entered alternate screen at inline startup"
                    os.write(master, b"/fork 2\r")
                    sent = True
                if sent and b"Forked as" in output and not quit_sent:
                    os.write(master, b"\x03\x03")
                    quit_sent = True
                if child.poll() is not None:
                    break
            child.wait(timeout=5)
            assert child.returncode == 0 and b"Forked as" in output, output[-1200:]
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
        # The no-argument picker exposes only committed ended Turns, and choosing
        # one copies facts without dispatching the restored Turn.
        source_before = json.loads(ion("--session", source_id, "inspect").stdout)
        before_paths = set((work / "ion/sessions").rglob("*.sqlite"))
        before_requests = len(requests)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        child = subprocess.Popen([binary, "--cwd", workspace, "--session", source_id, "chat"], env={**env, "TERM": "xterm-256color"}, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
        os.close(slave)
        output = bytearray()
        picker_sent = selected = quit_sent = False
        deadline = time.monotonic() + 12
        try:
            while time.monotonic() < deadline:
                readable, _, _ = select.select([master], [], [], 0.05)
                if readable:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        data = b""
                    output.extend(data)
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[2;1R")
                if b"\xe2\x80\xba" in output and not picker_sent:
                    os.write(master, b"/fork-after\r")
                    picker_sent = True
                if picker_sent and b"Fork after Turn" in output and b"Turn 1  ended completed" in output and b"Turn 2  ended completed" in output and not selected:
                    os.write(master, b"\r")
                    selected = True
                if selected and b"Forked as" in output and not quit_sent:
                    os.write(master, b"\x03")
                    quit_sent = True
                if child.poll() is not None:
                    break
            child.wait(timeout=5)
            assert child.returncode == 0 and b"Forked as" in output, output[-1200:]
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGKILL)
                child.wait()
            os.close(master)
        assert len(requests) == before_requests, "picked /fork-after dispatched Turn work"
        children = set((work / "ion/sessions").rglob("*.sqlite")) - before_paths
        assert len(children) == 1, "picked /fork-after did not save exactly one copy"
        picked = json.loads(ion("--session", children.pop(), "inspect").stdout)
        assert [entry["data"]["turn"] for entry in picked["entries"] if entry["kind"] == "turn_started"] == [1]
        assert picked["messages"] == source_before["messages"][:2]
        assert picked["unfinished_turn"] is None
        assert picked["last_model"] == source_before["last_model"]
        assert json.loads(ion("--session", source_id, "inspect").stdout) == source_before, "picked /fork-after changed source facts"

        # The normalized image carries a coordinate note. Fork restoration must
        # keep that note with its image, not fold it into the editable prompt.
        def png_chunk(kind, payload):
            return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload) & 0xffffffff)

        picture = workspace / "wide.png"
        picture.write_bytes(b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 2001, 1, 8, 6, 0, 0, 0)) + png_chunk(b"IDAT", zlib.compress(b"\x00" + b"\xff\x00\x00\xff" * 2001)) + png_chunk(b"IEND", b""))
        subprocess.run([binary, "use", "smoke", "fork-model", "--endpoint", endpoint, "--wire", "openrouter-chat", "--images"], env=env, check=True, capture_output=True)
        image_turn = ion("--image", "wide.png", "run", "Preserve image coordinates.")
        image_session = image_turn.stderr.split("[session: ")[1].split("]")[0]
        original = requests[-1]["messages"][-1]["content"]
        assert [part["type"] for part in original] == ["text", "text", "image_url"]
        image_view = json.loads(ion("--session", image_session, "inspect").stdout)
        image_entry = next(index + 1 for index, entry in enumerate(image_view["entries"]) if entry["kind"] == "turn_started")
        image_affinity = requests[-1]["session_id"]
        for command, notice, mode in ((command, notice, mode) for command, notice in ((b"/fork 1\r", b"Forked as"), (f"/tree {image_entry}\r".encode(), b"Context selected")) for mode in ("inline", "fullscreen")):
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
            child = subprocess.Popen([binary, "--cwd", workspace, "--session", image_session, "--tui-mode", mode, "chat"], env={**env, "TERM": "xterm-256color"}, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
            os.close(slave)
            output = bytearray()
            fork_sent = submit_sent = quit_sent = False
            before = len(requests)
            deadline = time.monotonic() + 12
            try:
                while time.monotonic() < deadline:
                    readable, _, _ = select.select([master], [], [], 0.05)
                    if readable:
                        try:
                            data = os.read(master, 65536)
                        except OSError as error:
                            if error.errno != errno.EIO:
                                raise
                            data = b""
                        output.extend(data)
                        if b"\x1b[6n" in data:
                            os.write(master, b"\x1b[2;1R")
                    if b"\xe2\x80\xba" in output and not fork_sent:
                        os.write(master, command)
                        fork_sent = True
                    if fork_sent and notice in output and not submit_sent:
                        assert len(requests) == before, "fork dispatched restored input"
                        os.write(master, b"\r")
                        output.clear()
                        submit_sent = True
                    if submit_sent and len(requests) > before and b"FORK_OK" in output and not quit_sent:
                        assert len(requests) == before + 1
                        assert requests[-1]["messages"][-1]["content"] == original, "history selection changed prompt/image-note associations"
                        if command.startswith(b"/tree"):
                            assert requests[-1]["session_id"] == image_affinity, "context navigation changed logical transport affinity"
                        # Visible answer text can precede Host settlement.
                        # Explicit quit joins either state; Ctrl-C only stops active work.
                        os.write(master, b"\x04")
                        quit_sent = True
                    if child.poll() is not None:
                        break
                assert quit_sent, (mode, fork_sent, submit_sent, len(requests) - before, output[-1200:])
                child.wait(timeout=5)
                assert child.returncode == 0, output[-1200:]
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
                os.close(master)
        tree_view = json.loads(ion("--session", image_session, "inspect").stdout)
        assert tree_view["entries"][:len(image_view["entries"])] == image_view["entries"], "tree rewrote original facts"
        assert len([entry for entry in tree_view["entries"] if entry["kind"] == "context_selected"]) == 2
        # Export through the passive CLI after a real same-Session branch change.
        # A hidden branch must not survive inside HTML's source or media payload.
        selected = max(index + 1 for index, entry in enumerate(tree_view["entries"])
                       if entry["kind"] == "assistant" and index + 1 in tree_view["context_path"])
        ion("--session", image_session, "run", "HIDDEN_ALTERNATIVE_EXPORT")
        rpc = subprocess.Popen([binary, "--cwd", workspace, "--session", image_session, "rpc"], env=env,
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            assert json.loads(rpc.stdout.readline())["type"] == "ready"
            rpc.stdin.write(b'{"id":"position","type":"get_context"}\n')
            rpc.stdin.flush()
            position = json.loads(rpc.stdout.readline())["data"]["position"]
            rpc.stdin.write((json.dumps({"id": "select", "type": "navigate_context", "expected": position,
                                        "head_entry": selected}) + "\n").encode())
            rpc.stdin.flush()
            assert json.loads(rpc.stdout.readline())["success"]
            rpc.stdin.close()
            assert rpc.wait(timeout=8) == 0, rpc.stderr.read()
        finally:
            if rpc.poll() is None:
                rpc.kill()
                rpc.wait(timeout=5)
        before = json.loads(ion("--session", image_session, "inspect").stdout)
        assert "HIDDEN_ALTERNATIVE_EXPORT" in json.dumps(before), "fixture lost the alternative"
        before_requests = len(requests)
        html_path = workspace / "selected branch.HTML"
        exported_path = ion("--session", image_session, "export", html_path.name).stdout.strip()
        assert Path(exported_path) == Path(before["cwd"]) / html_path.name, (exported_path, before["cwd"])
        assert Path(exported_path).resolve() == html_path.resolve()
        exported = html_path.read_text()
        assert exported.startswith("<!DOCTYPE html>")
        assert "HIDDEN_ALTERNATIVE_EXPORT" not in exported
        assert original[-1]["image_url"]["url"] in exported, "HTML lost the saved image"
        assert "FORK_OK" in exported
        assert html_path.stat().st_mode & 0o777 == 0o600
        refused = ion("--session", image_session, "export", html_path.name, check=False)
        assert refused.returncode != 0 and html_path.read_text() == exported
        text = ion("--session", image_session, "export").stdout
        assert text.startswith("Ion transcript\n") and "[image: image/png]" in text
        assert "HIDDEN_ALTERNATIVE_EXPORT" not in text and "data:image" not in text
        assert json.loads(ion("--session", image_session, "inspect").stdout) == before, "export changed Session facts"
        assert len(requests) == before_requests, "export dispatched model work"
        print("Ion fork/clone, durable context navigation, selected HTML/text export, provider affinity, terminal control and image-note restoration: OK")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
