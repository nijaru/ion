"""Check actual inline publication in tmux, whose full-screen erase saves history."""

import fcntl
import hashlib
import json
import os
import shlex
import shutil
import signal
import sqlite3
import struct
import subprocess
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
assert shutil.which("tmux"), "the native-scrollback smoke requires tmux"

with tempfile.TemporaryDirectory(prefix="ion-scrollback-") as temporary:
    work = Path(temporary)
    workspace = work / "workspace"
    workspace.mkdir()
    (workspace / "data.txt").write_text("observed data\n")
    data_digest = hashlib.sha256(b"observed data\n").hexdigest()
    home = work / "home"
    home.mkdir()
    env = os.environ.copy()
    env.update(
        HOME=str(home),
        ION_HOME=str(work / "ion"), XDG_CONFIG_HOME=str(work / "config"),
        XDG_STATE_HOME=str(work / "state"),
        TERM="xterm-256color",
    )
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(body)
            if len(requests) in (1, 3):
                calls = [
                    ("read", {"path": "data.txt"}),
                    ("edit", {"path": "data.txt", "base_digest": data_digest, "edits": [{"old_text": "observed data", "new_text": "UPDATED_SNAPSHOT"}]}),
                    ("exec", {"command": "printf 'FIRST_%s\\nmid-one\\nmid-two\\nmid-three\\nCOMMAND_OUTPUT_%s\\n' HIDDEN ONCE; seq -f 'LIVE_%02g' 25 >&2; while ! test -f release-preview; do sleep 0.05; done"}),
                ] if len(requests) == 1 else [
                    ("exec", {"command": "printf 'FUTURE_%s\\nmid-one\\nmid-two\\nfuture-last\\n' FIRST"}),
                ]
                delta = {
                    "content": "NARRATIVE_ONCE" if len(requests) == 1 else "FUTURE_NARRATIVE",
                    "tool_calls": [
                        {"index": index, "id": f"call-{index}", "type": "function",
                         "function": {"name": name, "arguments": json.dumps(arguments)}}
                        for index, (name, arguments) in enumerate(calls)
                    ],
                }
                finish = "tool_calls"
            else:
                assert len(requests) in (2, 4, 5), "unexpected additional model request"
                # Leave the completed calls visible in the mutable surface
                # before the final observation is committed and published.
                time.sleep(0.75)
                answer = {2: "FINAL_ONCE", 4: "FUTURE_DONE", 5: "JSONL_ANSWER"}[len(requests)]
                delta, finish = {"content": answer}, "stop"
            thinking = f"THINK_{len(requests)}"
            delta.update(reasoning=thinking, reasoning_details=[
                {"type": "reasoning.text", "text": thinking, "signature": "SIGNATURE_PRIVATE", "index": 0},
                {"type": "reasoning.summary", "summary": f"SAFE_SUMMARY_{len(requests)}", "index": 1},
                {"type": "reasoning.encrypted", "data": "ENCRYPTED_PRIVATE", "index": 2},
            ])
            payload = b"data: " + json.dumps({
                "id": "scrollback", "model": "smoke-model",
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            }).encode() + b"\n\ndata: [DONE]\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    serving = threading.Thread(target=server.serve_forever)
    serving.start()
    socket = str(work / "tmux.sock")

    def tmux(*args):
        return subprocess.check_output(["tmux", "-S", socket, *args], env=env).decode()

    def composer_ready(pane):
        # A painted composer is not sufficient during canonical Session I/O.
        return "›" in pane and not any(message in pane for message in (
            "Working", "Session refresh", "Reading local state", "Preparing local command",
        ))

    def wait_completed_turns(count):
        # Provisional answer text can paint before Core settlement and native
        # publication. Require committed completion and the emulator's idle
        # surface before asserting exact-once scrollback content.
        while True:
            sessions = list((work / "ion/sessions").rglob("*.sqlite"))
            assert len(sessions) == 1, "expected one owned main Session"
            with sqlite3.connect(f"file:{sessions[0]}?mode=ro", uri=True) as db:
                completed = db.execute("SELECT count(*) FROM entries WHERE json_extract(body,'$.kind')='turn_ended' AND json_extract(body,'$.data.reason')='completed'").fetchone()[0]
            pane = tmux("capture-pane", "-p", "-t", "ion")
            if completed == count and composer_ready(pane):
                return
            assert time.monotonic() < deadline, f"inline Turn/publication did not settle:\n{pane}"
            time.sleep(0.02)

    native_pid = None
    try:
        subprocess.run(
            [binary, "use", "smoke", "smoke-model", "--endpoint",
             f"http://127.0.0.1:{server.server_port}/v1/chat/completions",
             "--wire", "openrouter-chat"], env=env, check=True, capture_output=True,
        )
        exit_path = work / "exit-status"
        launcher = work / "launch.sh"
        launcher.write_text(
            f"#!/bin/sh\n{shlex.quote(str(binary))} chat\nstatus=$?\n"
            f"printf '%s\\n' \"$status\" > {shlex.quote(str(exit_path))}\nexit \"$status\"\n"
        )
        launcher.chmod(0o700)
        tmux("new-session", "-d", "-s", "ion", "-x", "100", "-y", "24",
             "-c", str(workspace), str(launcher))
        tmux("set-option", "-w", "-t", "ion", "remain-on-exit", "on")
        deadline = time.monotonic() + 35
        while not composer_ready(pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"inline composer did not become ready:\n{pane}"
            time.sleep(0.02)
        tmux("send-keys", "-t", "ion", "-l", "INPUT_ONCE: read data.txt")
        tmux("send-keys", "-t", "ion", "C-j")
        tmux("send-keys", "-t", "ion", "-l", "and check it")
        while "and check it" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"multiline draft did not paint:\n{pane}"
            time.sleep(0.02)
        assert "INPUT_ONCE: read data.txt" in pane and not requests, pane
        tmux("send-keys", "-t", "ion", "Enter")
        while True:
            pane = tmux("capture-pane", "-p", "-t", "ion")
            if "LIVE_25" in [row.strip() for row in pane.splitlines()]:
                break
            assert time.monotonic() < deadline, f"live output tail did not paint:\n{pane}"
            time.sleep(0.02)
        assert "Running printf" in pane and "›" in pane, (
            f"command output crowded out the transcript or composer:\n{pane}"
        )
        (workspace / "release-preview").touch()
        while "FINAL_ONCE" not in tmux("capture-pane", "-p", "-t", "ion"):
            assert time.monotonic() < deadline, "inline Turn did not finish"
            time.sleep(0.02)

        wait_completed_turns(1)
        tmux("resize-window", "-t", "ion", "-x", "100", "-y", "30")
        assert requests[0]["messages"][-1]["content"] == "INPUT_ONCE: read data.txt\nand check it"

        def check_history():
            history = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
            for marker in ("INPUT_ONCE", "NARRATIVE_ONCE", "FINAL_ONCE", "-observed data", "+UPDATED_SNAPSHOT"):
                assert history.count(marker) == 1, f"{marker} was republished or lost:\n{history}"
            assert history.count("1 earlier stdout lines · /tools") == 1, f"shared scrollback lost the preview disclosure:\n{history}"
            assert history.count("COMMAND_OUTPUT_ONCE") == 1, f"command tail was lost or duplicated:\n{history}"
            assert "Working" not in history, f"mutable operation chrome leaked into history:\n{history}"
            assert "THINK_1" not in history and "THINK_2" not in history, "hidden thinking was published retroactively"
            assert "SIGNATURE_PRIVATE" not in history and "ENCRYPTED_PRIVATE" not in history, "opaque reasoning leaked"

        check_history()
        assert (workspace / "data.txt").read_text() == "UPDATED_SNAPSHOT\n"

        # Dismissing a tall inline completion menu must reclaim its rows
        # immediately, without a new Turn or a fullscreen round-trip.
        tmux("send-keys", "-t", "ion", "-l", "/")
        while "Tab complete" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"slash completion did not open:\n{pane}"
            time.sleep(0.02)
        tmux("send-keys", "-t", "ion", "C-u")
        collapse_deadline = time.monotonic() + 2
        while True:
            pane = tmux("capture-pane", "-p", "-t", "ion")
            rows = pane.splitlines()
            final = next((i for i, row in enumerate(rows) if "FINAL_ONCE" in row), None)
            rule = "─" * 100
            if (final is not None and final + 3 < len(rows)
                    and rows[final + 1] == rows[final + 3] == rule
                    and rows[final + 2].startswith("›") and "Tab complete" not in pane):
                break
            assert time.monotonic() < collapse_deadline, f"completion left a gap after dismissal:\n{pane}"
            time.sleep(0.02)
        history = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
        assert history.splitlines().count(rule) == 2, f"composer rules leaked into scrollback:\n{history}"
        check_history()
        tmux("send-keys", "-t", "ion", "-l", "/tool 2")
        tmux("send-keys", "-t", "ion", "Enter")
        while "Recorded edit diff" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"recorded edit inspection did not open:\n{pane}"
            time.sleep(0.02)
        detail = tmux("capture-pane", "-p", "-t", "ion")
        assert "-observed data" in detail and "+UPDATED_SNAPSHOT" in detail, detail
        tmux("send-keys", "-t", "ion", "Left")
        while "Tool 1: read" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"previous committed tool did not open:\n{pane}"
            time.sleep(0.02)
        tmux("send-keys", "-t", "ion", "Right")
        while "Tool 2: edit" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"next committed tool did not open:\n{pane}"
            time.sleep(0.02)
        assert "Recorded edit diff" in pane
        tmux("send-keys", "-t", "ion", "Escape")
        time.sleep(0.1)
        check_history()

        # Search for a marker omitted from the preview and not present in
        # the exec arguments, then open the original numbered evidence.
        tmux("send-keys", "-t", "ion", "-l", "/tools output FIRST_HIDDEN")
        tmux("send-keys", "-t", "ion", "Enter")
        while "Recorded tool output · 1 match(es)" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"recorded output search did not find the command:\n{pane}"
            time.sleep(0.02)
        assert "3 · exec" in pane, pane
        tmux("send-keys", "-t", "ion", "Enter")
        while '"stdout": (text)\n    FIRST_HIDDEN' not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"saved stdout did not open:\n{pane}"
            time.sleep(0.02)
        assert "COMMAND_OUTPUT_ONCE" in pane, pane
        tmux("send-keys", "-t", "ion", "Escape")
        while "Recorded tool output · 1 match(es)" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"closing evidence lost the output search:\n{pane}"
            time.sleep(0.02)
        tmux("send-keys", "-t", "ion", "Escape")
        while "Recorded tool output" in (pane := tmux("capture-pane", "-p", "-t", "ion")) or not composer_ready(pane):
            assert time.monotonic() < deadline, f"output picker did not return to the composer:\n{pane}"
            time.sleep(0.02)
        assert len(requests) == 2, "searching saved results made a model request"
        check_history()

        # Inspection in a shared pane must keep the inline draft and
        # leave previously published history untouched.
        viewport_draft = "EXPANDED_UNSENT café 界 👩‍💻"
        tmux("send-keys", "-t", "ion", "-l", viewport_draft)
        while viewport_draft not in tmux("capture-pane", "-p", "-t", "ion"):
            assert time.monotonic() < deadline, "expanded-view draft did not paint"
            time.sleep(0.02)
        # The read-only transcript uses a separate viewport, not the composer
        # or primary scrollback. Try input and reflow before returning to it.
        tmux("send-keys", "-t", "ion", "M-t")
        history_deadline = time.monotonic() + 6

        def wait_transcript(*markers, rows=None):
            while True:
                pane = tmux("capture-pane", "-p", "-t", "ion")
                lines = pane.splitlines()
                if ((rows is None or len(lines) == rows)
                        and lines and lines[-1].startswith("Transcript")
                        and all(marker in pane for marker in markers)):
                    return pane
                assert time.monotonic() < history_deadline, f"transcript missing {markers}:\n{pane}"
                time.sleep(0.02)

        def alternate_screen():
            return tmux("display-message", "-p", "-t", "ion", "#{alternate_on}").strip() == "1"

        pane = wait_transcript("FINAL_ONCE")
        assert alternate_screen() and viewport_draft not in pane, pane
        tmux("send-keys", "-t", "ion", "x", "Enter")
        tmux("set-buffer", "IGNORED_TRANSCRIPT_PASTE")
        tmux("paste-buffer", "-p", "-d", "-t", "ion")
        tmux("resize-window", "-t", "ion", "-x", "72", "-y", "6")
        wait_transcript("FINAL_ONCE", rows=6)
        tmux("send-keys", "-t", "ion", "PPage", "PPage", "PPage", "PPage", "PPage")
        wait_transcript("NARRATIVE_ONCE")
        tmux("resize-window", "-t", "ion", "-x", "68", "-y", "7")
        wait_transcript("NARRATIVE_ONCE", rows=7)
        tmux("send-keys", "-t", "ion", "C-o")
        while "Conversation ·" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < history_deadline, f"transcript inspector did not open:\n{pane}"
            time.sleep(0.02)
        tmux("send-keys", "-t", "ion", "Escape")
        wait_transcript("NARRATIVE_ONCE", rows=7)
        assert alternate_screen(), "closing the inspector left the transcript's screen"
        tmux("send-keys", "-t", "ion", "End")
        wait_transcript("FINAL_ONCE")
        tmux("resize-window", "-t", "ion", "-x", "100", "-y", "24")
        tmux("send-keys", "-t", "ion", "Escape")
        while alternate_screen() or viewport_draft not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < history_deadline, "transcript did not restore the composer"
            time.sleep(0.02)
        assert "IGNORED_TRANSCRIPT_PASTE" not in pane and f"{viewport_draft}x" not in pane, pane
        assert len(requests) == 2, "transcript submitted the draft"
        check_history()

        tmux("send-keys", "-t", "ion", "C-o")
        while True:
            pane = tmux("capture-pane", "-p", "-t", "ion")
            if "Tool 3: exec" in pane and "FIRST_HIDDEN" in pane:
                break
            assert time.monotonic() < deadline, f"Ctrl-O did not show recorded output:\n{pane}"
            time.sleep(0.02)
        assert tmux("display-message", "-p", "-t", "ion", "#{alternate_on}").strip() == "1"
        tmux("send-keys", "-t", "ion", "End")
        while "Call ID: call-2" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"End did not reach the bottom of recorded output:\n{pane}"
            time.sleep(0.02)
        assert "FIRST_HIDDEN" not in pane, pane
        tmux("send-keys", "-t", "ion", "Home")
        while "FIRST_HIDDEN" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert time.monotonic() < deadline, f"Home did not return to the start of recorded output:\n{pane}"
            time.sleep(0.02)
        assert "Tool 3: exec" in pane, pane
        tmux("send-keys", "-t", "ion", "C-o")
        while True:
            pane = tmux("capture-pane", "-p", "-t", "ion")
            alternate = tmux("display-message", "-p", "-t", "ion", "#{alternate_on}").strip()
            if alternate == "0" and "Details closed" in pane and viewport_draft in pane:
                break
            assert time.monotonic() < deadline, f"closing inspection did not restore the draft (alternate={alternate}):\n{pane}"
            time.sleep(0.02)
        restored = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
        assert restored.count(viewport_draft) == 1, f"expanded viewport custody failed (alternate={tmux('display-message', '-p', '-t', 'ion', '#{alternate_on}').strip()}):\n{restored}"
        tmux("send-keys", "-t", "ion", "C-u")
        while viewport_draft in tmux("capture-pane", "-p", "-t", "ion", "-S", "-"):
            assert time.monotonic() < deadline, "expansion archived provisional input"
            time.sleep(0.02)
        check_history()
        assert len(requests) == 2, "expansion submitted the draft"

        def command(text, expected):
            tmux("send-keys", "-t", "ion", "-l", text)
            tmux("send-keys", "-t", "ion", "Enter")
            while expected not in tmux("capture-pane", "-p", "-t", "ion"):
                assert time.monotonic() < deadline, f"command did not complete: {text}"
                time.sleep(0.02)

        def resize_unsent_draft(sizes):
            draft = "UNSENT_RESIZE café 界 👩‍💻"
            tmux("send-keys", "-t", "ion", "-l", draft)
            while draft not in tmux("capture-pane", "-p", "-t", "ion"):
                assert time.monotonic() < deadline, "Unicode draft did not paint"
                time.sleep(0.02)
            def cursor_on_line(column, text, prefix=False):
                pane = tmux("capture-pane", "-p", "-t", "ion").splitlines()
                x, y, visible = tmux("display-message", "-p", "-t", "ion", "#{cursor_x} #{cursor_y} #{cursor_flag}").split()
                return visible == "1" and int(x) == column and int(y) < len(pane) and (pane[int(y)].startswith(text) if prefix else pane[int(y)].rstrip() == text)

            for columns, rows in sizes:
                tmux("resize-window", "-t", "ion", "-x", str(columns), "-y", str(rows))
            # Wait for Ion's indented continuation/caret, not merely tmux reflow.
            while not cursor_on_line(4, "  👩‍💻"):
                assert time.monotonic() < deadline, "narrow composer did not reconcile its cursor"
                time.sleep(0.02)
            # Recovery/stop metadata may leave only the caret's source row at
            # this height. Navigate to the hidden prefix rather than assuming
            # the entire literal fits; neither viewport may archive a copy.
            tmux("send-keys", "-t", "ion", "Home")
            while not cursor_on_line(2, "› UNSENT_RESIZE", prefix=True):
                assert time.monotonic() < deadline, "narrow composer lost its source-relative start"
                time.sleep(0.02)
            resized = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
            assert resized.count("UNSENT_RESIZE") == 1, f"resize duplicated or lost unsent input:\n{resized}"
            tmux("send-keys", "-t", "ion", "End")
            while not cursor_on_line(4, "  👩‍💻"):
                assert time.monotonic() < deadline, "narrow composer lost its source-relative end"
                time.sleep(0.02)
            tmux("resize-window", "-t", "ion", "-x", "100", "-y", "30")
            while not cursor_on_line(26, "› " + draft):
                assert time.monotonic() < deadline, "restored composer did not reconcile its cursor"
                time.sleep(0.02)
            restored = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
            assert restored.count(draft) == 1 and restored.count("UNSENT_RESIZE") == 1, f"resize changed or duplicated the literal draft:\n{restored}"
            check_history()

        # Idle resize must schedule its own redraw rather than wait for a key.
        command("!exit 7", "Shell finished with an error")
        resize_unsent_draft([(24, 7)])
        tmux("send-keys", "-t", "ion", "C-c", "C-c")

        # A key can reach Ion after kernel resize but before the next paint.
        # Up should move within the draft using the last painted wrapping.
        tmux("set-buffer", "RESIZE_TOP\nRESIZE_BOTTOM")
        tmux("paste-buffer", "-p", "-d", "-t", "ion")
        while "RESIZE_BOTTOM" not in tmux("capture-pane", "-p", "-t", "ion"):
            assert time.monotonic() < deadline, "multiline draft did not paint"
            time.sleep(0.02)
        tmux("resize-window", "-t", "ion", "-x", "56", "-y", "28", ";",
             "send-keys", "-t", "ion", "Up", "!")
        while "RESIZE_TOP!" not in (pane := tmux("capture-pane", "-p", "-t", "ion")):
            assert "RESIZE_BOTTOM!" not in pane, f"resize swallowed Up:\n{pane}"
            assert time.monotonic() < deadline, f"resize draft did not reconcile:\n{pane}"
            time.sleep(0.02)
        tmux("send-keys", "-t", "ion", "C-c", "C-c")
        tmux("resize-window", "-t", "ion", "-x", "100", "-y", "30")

        # tmux reflows immediately but throttles queued PTY sizes by 250 ms.
        # This must not turn transient geometry into cancellation of native work.
        command("!printf 'once\\n' >> resize-once; echo $$ > resize.pid; exec sleep 30", "Running shell")
        while not (workspace / "resize.pid").exists():
            assert time.monotonic() < deadline, "native resize probe did not start"
            time.sleep(0.02)
        native_pid = int((workspace / "resize.pid").read_text())
        resize_unsent_draft([(60, 20), (24, 7), (50, 10), (24, 7)])
        os.kill(native_pid, 0)
        tmux("send-keys", "-t", "ion", "C-c", "C-c")
        while "Shell finished" not in tmux("capture-pane", "-p", "-t", "ion"):
            assert time.monotonic() < deadline, "native cancellation did not settle"
            time.sleep(0.02)
        try:
            os.kill(native_pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("cancelled native root remains alive")
        assert (workspace / "resize-once").read_text() == "once\n"
        native_pid = None
        # Stop stays latched after settlement; Home resets it without changing
        # the preserved literal before a separate guarded clear intent.
        tmux("send-keys", "-t", "ion", "Home", "C-c", "C-c")
        time.sleep(0.05)
        assert len(requests) == 2, "resize admitted unsent input"

        assert "FIRST_HIDDEN" not in tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
        command("/settings expanded", "Tool output: expanded")
        check_history()
        tmux("resize-window", "-t", "ion", "-x", "100", "-y", "60")
        command("/tui fullscreen", "FIRST_HIDDEN")
        assert tmux("capture-pane", "-p", "-t", "ion").count("COMMAND_OUTPUT_ONCE") == 1, "expanded source output lost or duplicated"
        assert '--- "data.txt"' in tmux("capture-pane", "-p", "-t", "ion"), "expanded diff header missing"
        command("/settings thinking show", "Thinking: Shown")
        view = tmux("capture-pane", "-p", "-t", "ion")
        assert "THINK_2" in view and "SAFE_SUMMARY_2" in view, "thinking visibility did not show recorded human blocks"
        assert "SIGNATURE_PRIVATE" not in view and "ENCRYPTED_PRIVATE" not in view, "opaque reasoning leaked"
        command("/settings thinking hide", "Thinking: Hidden")
        assert "THINK_2" not in tmux("capture-pane", "-p", "-t", "ion"), "thinking did not hide"
        command("/settings compact", "Tool output: compact")
        time.sleep(0.1)
        assert "FIRST_HIDDEN" not in tmux("capture-pane", "-p", "-t", "ion"), "compact view exposed omitted output"
        command("/tui inline", "native terminal scrollback")
        check_history()
        assert "FIRST_HIDDEN" not in tmux("capture-pane", "-p", "-t", "ion", "-S", "-"), "settings republished old native output"
        command("/settings expanded", "Tool output: expanded")
        command("/settings thinking show", "Thinking: Shown")
        command("NEXT_INPUT: run a follow-up command", "FUTURE_DONE")
        wait_completed_turns(2)
        history = tmux("capture-pane", "-p", "-t", "ion", "-S", "-")
        assert history.count("FUTURE_FIRST") == 1, "expanded future publication omitted or duplicated recorded output"
        assert history.count("THINK_3") == 1 and history.count("THINK_4") == 1, "future thinking publication was lost or duplicated"
        assert len(requests) == 4, "settings changed model requests"
        assert requests[1]["messages"][2]["content"] == "NARRATIVE_ONCE", "human thinking became answer text on the wire"
        assert requests[1]["messages"][2]["reasoning_details"][2]["data"] == "ENCRYPTED_PRIVATE", "opaque replay was changed"
        check_history()
        tmux("resize-window", "-t", "ion", "-x", "60", "-y", "20")
        time.sleep(0.15)
        check_history()
        # Local responses publish completely, even when longer than the whole
        # viewport; they do not become a clipped composer preview.
        tmux("resize-window", "-t", "ion", "-x", "40", "-y", "12")
        time.sleep(0.4)
        # Identify both ends of one help publication, not the changing command
        # inventory between them (for example /tree before /fork).
        head, tail = "CommandsSessions:/new/clone/", "Ctrl-Cstopswork"

        def help_counts():
            text = "".join(tmux("capture-pane", "-p", "-t", "ion", "-S", "-").split())
            return text.count(head), text.count(tail)

        for count in (1, 2):
            tmux("send-keys", "-t", "ion", "-l", "/help")
            tmux("send-keys", "-t", "ion", "Enter")
            deadline = time.monotonic() + 5
            while help_counts() < (count, count):
                assert time.monotonic() < deadline, f"command output missing from native history: {help_counts()}"
                time.sleep(0.02)
            assert help_counts() == (count, count), "explicit output lost or duplicated"
        tmux("resize-window", "-t", "ion", "-x", "40", "-y", "60")
        time.sleep(0.4)
        command("/tui fullscreen", "Fullscreen TUI")
        view = "".join(tmux("capture-pane", "-p", "-t", "ion").split())
        assert view.count(head) == view.count(tail) == 2, f"fullscreen lost command output ({view.count(head)} heads, {view.count(tail)} tails): {view}"
        command("/tui inline", "native terminal scrollback")
        assert help_counts() == (2, 2), "viewport change republished local output"
        check_history()
        assert len(requests) == 4, "terminal-local output reached the model"
        for mode in ("inline", "fullscreen"):
            target = f"tiny-{mode}"
            tiny_exit = work / f"{target}.exit"
            tiny_launcher = work / f"{target}.sh"
            tiny_launcher.write_text(
                f"#!/bin/sh\n{shlex.quote(str(binary))} --session {shlex.quote(str(work / (target + '.sqlite')))} chat --tui-mode {mode}\n"
                f"printf '%s\\n' \"$?\" > {shlex.quote(str(tiny_exit))}\n"
            )
            tiny_launcher.chmod(0o700)
            tmux("new-session", "-d", "-s", target, "-x", "30", "-y", "2",
                 "-c", str(workspace), str(tiny_launcher))
            tmux("set-option", "-w", "-t", target, "remain-on-exit", "on")
            tiny_deadline = time.monotonic() + 8

            def until_tiny(predicate):
                while not predicate():
                    assert time.monotonic() < tiny_deadline, (
                        target, tmux("capture-pane", "-p", "-t", target),
                        tmux("display-message", "-p", "-t", target, "#{pane_dead} #{pane_dead_status} #{pane_current_command} #{pane_width}x#{pane_height}"),
                        tiny_exit.read_text() if tiny_exit.exists() else "no exit receipt",
                        subprocess.check_output(["ps", "-t", tmux("display-message", "-p", "-t", target, "#{pane_tty}").strip(), "-o", "pid,ppid,pgid,stat,command"], text=True),
                    )
                    time.sleep(0.02)

            until_tiny(lambda: "›" in tmux("capture-pane", "-p", "-t", target))
            tmux("send-keys", "-t", target, "-l", "\x1b[200~FIRST_LINE\nSECOND_LINE\nTHIRD_LINE\nFOURTH_LINE\x1b[201~")
            tmux("send-keys", "-t", target, "Up", "Up", "Up", "Home")

            def cursor_on(text):
                pane = tmux("capture-pane", "-p", "-t", target).splitlines()
                row, visible = tmux("display-message", "-p", "-t", target, "#{cursor_y} #{cursor_flag}").split()
                return visible == "1" and int(row) < len(pane) and pane[int(row)].strip() == text

            until_tiny(lambda: cursor_on("› FIRST_LINE"))
            tmux("send-keys", "-t", target, "Down", "Down", "Down", "End")
            until_tiny(lambda: cursor_on("FOURTH_LINE"))
            assert len(requests) == 4, "composer navigation submitted a model request"
            tmux("send-keys", "-t", target, "C-c", "C-c")  # Clear, never cascade to quit.
            time.sleep(0.05)
            tmux("send-keys", "-t", target, "C-d", "C-d")
            until_tiny(tiny_exit.exists)
            assert tiny_exit.read_text().strip() == "0", "tiny composer failed on exit"
            tmux("kill-session", "-t", target)
            print(f"Ion {mode} two-row cursor-containing composer: OK")

        exit_deadline = time.monotonic() + 5
        tmux("send-keys", "-t", "ion", "C-d", "C-d")
        while not exit_path.exists():
            assert time.monotonic() < exit_deadline, (
                "inline client did not exit", tmux("capture-pane", "-p", "-t", "ion"),
            )
            time.sleep(0.02)
        assert exit_path.read_text().strip() == "0", "inline client failed on exit"
        # Check again after shutdown's final reconciliation and native paint.
        check_history()
        assert help_counts() == (2, 2), "shutdown republished local output"
        tmux("kill-session", "-t", "ion")
        session = next((work / "ion/sessions").rglob("*.sqlite"))
        inspected = json.loads(subprocess.run([binary, "--cwd", workspace, "--session", session, "inspect"], env=env, capture_output=True, text=True, check=True).stdout)
        assistants = [entry["data"]["message"] for entry in inspected["entries"] if entry["kind"] == "assistant"]
        assert assistants[0]["content"][:2] == [{"Thinking": "THINK_1"}, {"Thinking": "SAFE_SUMMARY_1"}], "saved human thinking lost its block boundaries"
        exported = subprocess.run([binary, "--cwd", workspace, "--session", session, "export"], env=env, capture_output=True, text=True, check=True).stdout
        assert "THINK_1" in exported and "SAFE_SUMMARY_1" in exported and "SIGNATURE_PRIVATE" not in exported and "ENCRYPTED_PRIVATE" not in exported, "export lost human thinking or exposed opaque replay"
        output = subprocess.run([binary, "--json", "--cwd", workspace, "--session", work / "jsonl.sqlite", "run", "Return an answer"], env=env, capture_output=True, text=True, check=True).stdout
        events = [json.loads(line) for line in output.splitlines()]
        thinking = [(event["block"], event["text"]) for event in events if event["type"] == "thinking_delta"]
        assert thinking == [(0, "THINK_5"), (1, "SAFE_SUMMARY_5")], "JSONL thinking was absent or duplicated"
        assert any(event["type"] == "final" and event["text"] == "JSONL_ANSWER" for event in events), "thinking changed the final answer"
        assert "SIGNATURE_PRIVATE" not in output and "ENCRYPTED_PRIVATE" not in output, "JSONL exposed opaque replay"

        # Use real committed shell records as fixtures so this exercises reopen
        # and both directions across the reader's record limit without running
        # dozens of shells just to make the transcript long enough to page.
        archive = work / "paged.sqlite"
        with sqlite3.connect(session) as source, sqlite3.connect(archive) as history:
            source.backup(history)
            settlement = next(entry for entry in inspected["entries"] if entry["kind"] == "user_shell_settled")
            for index in range(58):
                entry = {"kind": "user_shell_admitted", "data": {
                    "command": f"PAGED_SHELL_{index:03}", "exclude_from_context": True,
                }}
                admission = history.execute("INSERT INTO entries(body) VALUES (?)", (json.dumps(entry).encode(),)).lastrowid
                record = {"kind": settlement["kind"], "data": {**settlement["data"], "admission_entry": admission}}
                if index == 57:
                    outcome = record["data"]["outcome"]
                    output = outcome["output"]
                    record["data"]["outcome"] = {**outcome, "output": {
                        **output, "value": {**output["value"], "fixture_padding": "x" * (270 * 1024)},
                    }}
                history.execute("INSERT INTO entries(body) VALUES (?)", (json.dumps(record).encode(),))

        for mode in ("inline", "fullscreen"):
            target = f"paged-{mode}"
            launch = [binary, "--session", archive, "chat", "--tui-mode", mode]
            tmux("new-session", "-d", "-s", target, "-x", "100", "-y", "22", "-c", str(workspace), shlex.join(map(str, launch)))
            tmux("set-option", "-w", "-t", target, "remain-on-exit", "on")
            end = time.monotonic() + 12

            def capture():
                return tmux("capture-pane", "-p", "-t", target)

            def wait_for(predicate):
                while not predicate(pane := capture()):
                    assert time.monotonic() < end, f"{mode}: page state did not settle:\n{pane}"
                    time.sleep(0.02)
                return pane

            wait_for(composer_ready)
            tmux("send-keys", "-t", target, "-l", "PAGED_UNSENT")
            tmux("send-keys", "-t", target, "M-t")
            wait_for(lambda pane: "Transcript" in pane and "PAGED_SHELL_056" in pane and "older history" in pane)
            for _ in range(30):
                if "PAGED_SHELL_009" in capture():
                    break
                tmux("send-keys", "-t", target, "PPage")
                time.sleep(0.05)
            wait_for(lambda pane: "Transcript" in pane and "PAGED_SHELL_009" in pane and "older history" not in pane)
            tmux("resize-window", "-t", target, "-x", "72", "-y", "20")
            wait_for(lambda pane: "Transcript" in pane and "PAGED_SHELL_009" in pane)
            tmux("send-keys", "-t", target, "C-o")
            wait_for(lambda pane: "Conversation ·" in pane and "PAGED_SHELL_00" in pane)
            tmux("resize-window", "-t", target, "-x", "72", "-y", "46")
            tmux("send-keys", "-t", target, "Right", "End")
            inspected_page = wait_for(lambda pane: "Conversation ·" in pane and "PAGED_SHELL_009" in pane)
            assert "PAGED_SHELL_056" not in inspected_page, "old page inspection jumped to the latest chat"
            tmux("resize-window", "-t", target, "-x", "72", "-y", "20")
            tmux("send-keys", "-t", target, "Escape")
            wait_for(lambda pane: "Transcript" in pane and "PAGED_SHELL_009" in pane)
            for _ in range(16):
                if "older history" in capture():
                    break
                tmux("send-keys", "-t", target, "NPage")
                time.sleep(0.05)
            wait_for(lambda pane: "Transcript" in pane and "older history" in pane)
            tmux("send-keys", "-t", target, "End")
            wait_for(lambda pane: "Transcript" in pane and "PAGED_SHELL_056" in pane)
            # Force the placeholder to rewrap, so the following assertion
            # cannot pass on tmux's cropped copy of the old viewport.
            tmux("resize-window", "-t", target, "-x", "40", "-y", "2")
            wait_for(lambda pane: "Transcript" in pane and "/export" in pane and "exceeds the page limit" not in pane)
            tmux("send-keys", "-t", target, "C-o")
            wait_for(lambda pane: "Conversation ·" in pane)
            tmux("resize-window", "-t", target, "-x", "100", "-y", "22")
            wait_for(lambda pane: "Conversation ·" in pane and "PAGED_SHELL_057" in pane and "PAGED_SHELL_056" not in pane)
            tmux("send-keys", "-t", target, "Escape")
            wait_for(lambda pane: "Transcript" in pane and "exceeds the page limit" in pane)
            tmux("send-keys", "-t", target, "Escape")
            wait_for(lambda pane: "PAGED_UNSENT" in pane and composer_ready(pane))
            if mode == "inline":
                assert tmux("capture-pane", "-p", "-t", target, "-S", "-").count("PAGED_SHELL_057") == 1
            tmux("send-keys", "-t", target, "C-c", "C-c")
            wait_for(lambda pane: "PAGED_UNSENT" not in pane and composer_ready(pane))
            tmux("send-keys", "-t", target, "C-d", "C-d")
            while tmux("display-message", "-p", "-t", target, "#{pane_dead}").strip() != "1":
                assert time.monotonic() < end, f"{mode}: reopened pager did not exit"
                time.sleep(0.02)
            assert tmux("display-message", "-p", "-t", target, "#{pane_dead_status}").strip() == "0"
            tmux("kill-session", "-t", target)
            assert len(requests) == 5, "passive paging issued a model request"
            print(f"Ion {mode} committed transcript pages: older, resize, page evidence, oversized evidence, newer, end, exact draft: OK")
    finally:
        if native_pid is not None:
            try:
                os.kill(native_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        subprocess.run(["tmux", "-S", socket, "kill-server"], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        server.shutdown()
        serving.join()
        server.server_close()

print("Ion inline native history: publish once, no mutable chrome, resize: OK")
