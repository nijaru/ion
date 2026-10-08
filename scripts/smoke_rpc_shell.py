"""Exercise direct RPC shell authority and settlement without a model call."""

import json
import os
import select
import signal
import socket
import sqlite3
import struct
import subprocess
import tempfile
import time
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))


def read(child):
    assert select.select([child.stdout], [], [], 8)[0], "RPC shell output timed out"
    line = child.stdout.readline()
    assert line, (child.poll(), "RPC shell output ended")
    return json.loads(line)


def until(child, predicate):
    while True:
        record = read(child)
        if predicate(record):
            return record


def qualify(case):
    with tempfile.TemporaryDirectory(prefix="ion-rpc-shell-") as temporary:
        work = Path(temporary)
        cwd = work / "workspace"
        cwd.mkdir()
        session = work / "session.sqlite"
        env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
        env.update(HOME=str(work / "home"), XDG_CONFIG_HOME=str(work / "config"), XDG_STATE_HOME=str(work / "state"))
        subprocess.run([binary, "use", "offline", "shell", "--endpoint", "http://127.0.0.1:9/v1/chat/completions", "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        reader = client = listener = None
        if case == "input":
            listener = socket.socket()
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            client = socket.create_connection(listener.getsockname())
            reader, _ = listener.accept()
        child = subprocess.Popen([binary, "--cwd", cwd, "--session", session, "rpc"], env=env, stdin=reader if reader else subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        pid = None
        pid_captured = False

        def send(command):
            data = json.dumps(command).encode() + b"\n"
            if client:
                client.sendall(data)
            else:
                child.stdin.write(data)
                child.stdin.flush()

        def inspect():
            return json.loads(subprocess.run([binary, "--cwd", cwd, "--session", session, "inspect"], env=env, capture_output=True, check=True, timeout=8).stdout)

        try:
            assert read(child)["type"] == "ready"
            if case in ("admission", "result"):
                kind = "user_shell_admitted" if case == "admission" else "user_shell_settled"
                with sqlite3.connect(session) as connection:
                    connection.execute(f"CREATE TRIGGER reject_shell BEFORE INSERT ON entries WHEN json_extract(CAST(NEW.body AS TEXT), '$.kind') = '{kind}' BEGIN SELECT RAISE(ABORT, 'shell storage unavailable'); END;")
            command = "printf 'effect\\n' >> effects; printf 'BEFORE_STOP\\n'"
            if case not in ("admission", "result"):
                command += "; printf '%s' $$ > shell.pid; touch ready; exec sleep 30"
            excluded = case == "crash"
            send({"id": "direct", "type": "shell", "command": command, "exclude_from_context": excluded})
            ack = read(child)
            assert ack["id"] == "direct" and ack["success"] and ack["data"]["disposition"] == "started", ack
            if case in ("admission", "result"):
                ended = until(child, lambda record: record["type"] == "shell_end")
                assert ended["id"] == "direct" and ended["status"] == "failed" and "shell storage unavailable" in ended["error"], ended
                assert "outcome" not in ended, "storage failure published an observation"
                child.stdin.close()
                assert child.wait(timeout=8) == 0, child.stderr.read()
            else:
                deadline = time.monotonic() + 8
                while not (cwd / "ready").exists():
                    assert time.monotonic() < deadline and child.poll() is None, "native shell did not start"
                    time.sleep(0.01)
                pid = int((cwd / "shell.pid").read_text())
                pid_captured = True
                if case == "abort":
                    send({"id": "state", "type": "get_state"})
                    state = read(child)["data"]
                    assert state["busy"] and state["operation"] == "shell", state
                    for kind in ("shell", "prompt", "compact", "new_session", "steer", "follow_up"):
                        send({"id": kind, "type": kind, "command": "touch forbidden", "message": "forbidden"})
                        refused = read(child)
                        assert refused["id"] == kind and not refused["success"], refused
                    send({"id": "stop", "type": "abort"})
                    assert read(child)["data"]["disposition"] == "requested"
                elif case == "eof":
                    child.stdin.close()
                elif case == "output":
                    child.stdout.close()
                    send({"type": "get_state"})
                elif case == "input":
                    client.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
                    client.close()
                elif case == "crash":
                    child.kill()
                    child.wait(timeout=8)
                    # Crash provides no termination promise. Clean up our owned
                    # native process explicitly before testing passive recovery.
                    os.killpg(pid, signal.SIGKILL)
                    pid = None
                if case not in ("output", "crash"):
                    ended = until(child, lambda record: record["type"] == "shell_end")
                    assert ended["id"] == "direct" and ended["status"] == "cancelled", ended
                    assert ended["outcome"]["kind"] == "observed" and ended["outcome"]["output"]["cancelled"], ended
                if case == "abort":
                    child.stdin.close()
                if case != "crash":
                    assert (child.wait(timeout=8) == 0) == (case in ("abort", "eof")), child.stderr.read()
                    if case in ("input", "output"):
                        diagnostic = child.stderr.read()
                        assert (b"Connection reset" if case == "input" else b"Broken pipe") in diagnostic, diagnostic
            before = inspect()
            admissions = [entry for entry in before["entries"] if entry["kind"] == "user_shell_admitted"]
            outcomes = [entry["data"]["outcome"] for entry in before["entries"] if entry["kind"] == "user_shell_settled"]
            assert not any(entry["kind"].startswith("turn_") or entry["kind"] == "assistant" for entry in before["entries"]), "direct shell fabricated coding facts"
            assert not (cwd / "forbidden").exists()
            if case == "admission":
                assert not admissions and not outcomes and not (cwd / "effects").exists(), before
            else:
                assert (cwd / "effects").read_text() == "effect\n"
                assert len(admissions) == 1 and admissions[0]["data"] == {"command": command, "exclude_from_context": excluded}, admissions
                if case in ("result", "crash"):
                    assert not outcomes and before["unfinished_user_shell"]["effect"] == "unknown", before
                    assert ("external effect unknown" in json.dumps(before["messages"])) == (not excluded), before
                else:
                    assert before["unfinished_user_shell"] is None and len(outcomes) == 1, before
                    output = outcomes[0]["output"]
                    assert outcomes[0]["kind"] == "observed" and output["stdout"] == "BEFORE_STOP\n" and output["cancelled"] and output["wait_error"] is None, output
            if pid is not None:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    pid = None
                else:
                    raise AssertionError("direct native process survived settled RPC exit")
            assert inspect()["entries"] == before["entries"], "inspection repaired or replayed history"
            if case in ("result", "crash"):
                if case == "result":
                    with sqlite3.connect(session) as connection:
                        connection.execute("DROP TRIGGER reject_shell")
                child = subprocess.Popen([binary, "--cwd", cwd, "--session", session, "rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
                assert read(child)["type"] == "ready"
                send({"id": "passive", "type": "inspect"})
                assert read(child)["data"]["entries"] == before["entries"], "opening repaired or replayed history"
                send({"id": "next", "type": "shell", "command": "printf NEXT"})
                assert read(child)["success"]
                assert until(child, lambda record: record["type"] == "shell_end")["outcome"]["output"]["stdout"] == "NEXT"
                child.stdin.close()
                assert child.wait(timeout=8) == 0, child.stderr.read()
                after = inspect()
                assert after["unfinished_user_shell"] is None
                assert sum(entry["kind"] == "user_shell_admitted" for entry in after["entries"]) == 2
                assert any(entry["kind"] == "user_shell_settled" and entry["data"]["outcome"]["kind"] == "unknown" for entry in after["entries"]), after
                assert (cwd / "effects").read_text() == "effect\n", "recovery replayed the old shell"
            print(f"Ion RPC shell {case}: authority, settlement and no replay: OK")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=8)
            if not pid_captured and (cwd / "shell.pid").exists():
                # Also cover a failure between readiness and PID consumption.
                pid = int((cwd / "shell.pid").read_text())
            if pid is not None:
                try:
                    os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            for resource in (reader, client, listener, child.stdin, child.stdout, child.stderr):
                if resource is not None:
                    resource.close()


for case in ("abort", "eof", "input", "output", "admission", "result", "crash"):
    qualify(case)
