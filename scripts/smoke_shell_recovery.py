"""Qualify durable direct-shell admission, lost observation and no replay."""

import argparse
import errno
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
import time
from pathlib import Path


root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--case", choices=("admission", "result", "crash"))
args = parser.parse_args()


def qualify(case, excluded=False):
    with tempfile.TemporaryDirectory(prefix="ion-shell-recovery-") as temporary:
        work = Path(temporary)
        workspace = work / "workspace"
        workspace.mkdir()
        session = work / "session.sqlite"
        env = {**os.environ, "XDG_CONFIG_HOME": str(work / "config"), "XDG_STATE_HOME": str(work / "state"), "TERM": "xterm-256color"}
        subprocess.run([binary, "use", "smoke", "shell-model", "--endpoint", "http://127.0.0.1:9/v1/chat/completions", "--wire", "chat-completions"], env=env, capture_output=True, check=True)
        mode = "fullscreen" if excluded else "inline"
        master, slave = pty.openpty()
        slave_path = os.ttyname(slave)
        os.close(slave)

        def launch():
            slave = os.open(slave_path, os.O_RDWR | os.O_NOCTTY)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))

            def attach_terminal():
                os.setsid()
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

            try:
                return subprocess.Popen([binary, "--cwd", workspace, "--session", session, "--tui-mode", mode, "chat"], env=env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_terminal)
            finally:
                os.close(slave)

        output = bytearray()

        def wait_until(predicate):
            deadline = time.monotonic() + 8
            while not predicate():
                assert time.monotonic() < deadline, bytes(output[-1500:])
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO and predicate():
                            return
                        raise
                    output.extend(data)
                    if b"\x1b[6n" in data:
                        os.write(master, b"\x1b[2;1R")

        def inspect():
            return json.loads(subprocess.run([binary, "--cwd", workspace, "--session", session, "inspect"], env=env, capture_output=True, check=True).stdout)

        child = launch()
        shell_pid = None
        try:
            wait_until(lambda: "› ".encode() in output)
            if case != "crash":
                kind = "user_shell_admitted" if case == "admission" else "user_shell_settled"
                with sqlite3.connect(session) as connection:
                    connection.execute(f"CREATE TRIGGER reject_shell BEFORE INSERT ON entries WHEN json_extract(CAST(NEW.body AS TEXT), '$.kind') = '{kind}' BEGIN SELECT RAISE(ABORT, 'shell storage unavailable'); END;")
            command = "printf 'effect\\n' >> effects"
            if case == "crash":
                command += "; printf '%s' $$ > shell.pid; touch ready; exec sleep 30"
            os.write(master, (("!!" if excluded else "!") + command + "\r").encode())
            if case == "crash":
                wait_until(lambda: (workspace / "ready").exists())
                shell_pid = int((workspace / "shell.pid").read_text())
                child.kill()
                child.wait(timeout=8)
                # Ion cannot claim descendant termination after SIGKILL. End the
                # test's orphan explicitly so it does not hold the Session lease.
                os.killpg(shell_pid, signal.SIGKILL)
                shell_pid = None
            else:
                wait_until(lambda: b"shell storage unavailable" in output)
                if case == "admission":
                    os.write(master, b"/exit\r")
                    wait_until(lambda: child.poll() is not None)
                    assert child.returncode == 0

            before = inspect()
            admissions = [(index, entry["data"]) for index, entry in enumerate(before["entries"], 1) if entry["kind"] == "user_shell_admitted"]
            if case == "admission":
                assert not (workspace / "effects").exists(), "failed admission dispatched host work"
                assert not admissions and before["unfinished_user_shell"] is None, before
                print("Ion shell admission refusal before dispatch: OK")
                return
            assert (workspace / "effects").read_text() == "effect\n"
            assert len(admissions) == 1, "started command was not durably admitted"
            occurrence, intent = admissions[0]
            assert intent == {"command": command, "exclude_from_context": excluded}, intent
            assert before["unfinished_user_shell"] == {"effect": "unknown", "admission_entry": occurrence} and before["unfinished_turn"] is None, before
            assert not any(entry["kind"] == "user_shell_settled" for entry in before["entries"]), before
            assert ("external effect unknown" in json.dumps(before["messages"])) == (not excluded), before["messages"]
            exported = subprocess.run([binary, "--cwd", workspace, "--session", session, "export"], env=env, capture_output=True, text=True, check=True).stdout
            assert command in exported and "external effect unknown" in exported, exported
            assert "exit_code" not in exported and "Result marked as successful" not in exported, exported
            assert inspect()["entries"] == before["entries"], "passive inspection repaired history"
            if case == "result":
                with sqlite3.connect(session) as connection:
                    connection.execute("DROP TRIGGER reject_shell")
            output.clear()
            if case == "crash":
                child = launch()
                wait_until(lambda: "› ".encode() in output and b"external effect unknown" in output)
            # Explicit NEW shell admission closes the interruption, not replays it.
            # Result-write faults recover in the same UI; crash cases reopen.
            os.write(master, b"!printf RECOVERY_DONE\r")
            wait_until(lambda: b"stdout: RECOVERY_DONE" in output and b"Shell finished" in output)
            assert ("Shell command: " + command).encode() not in output, "previous shell error remained in the new operation's notices"
            os.write(master, b"/exit\r")
            wait_until(lambda: child.poll() is not None)
            assert child.returncode == 0
            after = inspect()
            assert (workspace / "effects").read_text() == "effect\n", "recovery replayed external effect"
            assert after["unfinished_user_shell"] is None and after["unfinished_turn"] is None, after
            closed = [entry["data"] for entry in after["entries"] if entry["kind"] == "user_shell_settled"]
            assert closed[0] == {"admission_entry": occurrence, "outcome": {"kind": "unknown"}}, closed
            assert closed[1]["outcome"]["kind"] == "observed" and closed[1]["outcome"]["output"]["stdout"] == "RECOVERY_DONE", closed
            assert any(command in part.get("Text", "") for message in after["messages"] for part in message["content"]) == (not excluded), after["messages"]
            print(f"Ion {mode} shell {case} unknown inspection/recovery/no replay ({'!!' if excluded else '!'}): OK")
        finally:
            if child.poll() is None:
                child.kill()
            os.close(master)
            child.wait(timeout=8)
            if shell_pid is not None:
                try:
                    os.killpg(shell_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


for case in ([args.case] if args.case else ("admission", "result", "crash")):
    qualify(case)
    if case == "crash":
        qualify(case, excluded=True)
