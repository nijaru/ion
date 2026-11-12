"""External-editor handoff through the built client and an actual terminal grid."""

import json
import shlex
import subprocess
import sys
import threading

from smoke_input_files import FileTerminal


def exercise_external_editor(binary, work, env, mode, requests, user_text, *,
                             outcomes=("success", "failure", "interrupt", "dirty_surface", "dirty_raw")):
    cwd = work / (mode + "-editor")
    cwd.mkdir()
    helper = cwd / "editor.py"
    helper.write_text('''import json, pathlib, sys, termios, time, tty
root = pathlib.Path(__file__).parent
config = json.loads((root / "config.json").read_text())
draft = pathlib.Path(sys.argv[1])
(root / "received").write_text(draft.read_text())
if config["outcome"] == "dirty_surface":
    print("\\x1b[?1049h\\x1b[2J\\x1b[H", end="", flush=True)
if config["outcome"] == "dirty_raw":
    tty.setraw(0, when=termios.TCSANOW)
print(config["marker"], end="\\r\\n", flush=True)
while not (root / "release").exists():
    time.sleep(0.01)
draft.write_text(config["edited"] + "\\n")
sys.exit(7 if config["fail"] else 0)
''')
    # Keep the fixture's controlling session alive until after observation:
    # macOS revokes the slave TTY when its session leader exits, even with an
    # open descriptor. The launcher records before cleaning its own TTY only.
    launcher = cwd / "launcher.py"
    launcher.write_text('''import json, os, pathlib, select, signal, subprocess, sys, termios, time, tty
signal.signal(signal.SIGINT, signal.SIG_IGN)
before = termios.tcgetattr(0)
try:
    tty.setraw(0, when=termios.TCSANOW)
    os.write(1, b"\\x1b[?1049$p")
    reply = b""
    deadline = time.monotonic() + 0.2
    while time.monotonic() < deadline:
        if select.select([0], [], [], max(0, deadline - time.monotonic()))[0]:
            reply += os.read(0, 1024)
            if reply.endswith(b"$y"):
                break
finally:
    termios.tcsetattr(0, termios.TCSANOW, before)
assert reply in (b"", b"\\x1b[?1049;2$y"), reply
pathlib.Path(sys.argv[1]).with_name("surface-reporting").write_text("native" if reply else "fixture")
child = subprocess.run(sys.argv[2:])
after = termios.tcgetattr(0)
comparison = [list(before), list(after)]
if sys.platform == "darwin":
    # Darwin termios(4): PENDIN is pending-input kernel state, not configuration.
    for attrs in comparison:
        attrs[3] &= ~termios.PENDIN
pathlib.Path(sys.argv[1]).write_text(json.dumps({"before": repr(before), "after": repr(after), "restored": comparison[0] == comparison[1]}))
termios.tcsetattr(0, termios.TCSANOW, before)
sys.exit(child.returncode if child.returncode >= 0 else 128 - child.returncode)
''')
    modes_file = cwd / "exit-modes.json"
    capture = cwd / "terminal.capture"
    terminal = FileTerminal(binary, cwd, dict(env, VISUAL=shlex.join([sys.executable, str(helper)]),
                                            ION_TERMINAL_CAPTURE=str(capture)), mode,
                            launcher=(sys.executable, launcher, modes_file))
    stop_reports = threading.Event()
    report_errors = []
    reporter = None
    seed = "EDITOR_BASE_" + mode
    try:
        terminal.wait(lambda: (cwd / "surface-reporting").exists())
        reporting = (cwd / "surface-reporting").read_text()
        if reporting == "fixture":
            # tmux 3.6a renders/switches real screens but does not answer 1049
            # queries. Complete that protocol boundary from its actual pane
            # state, never from the expected outcome. Ion must still issue the
            # query and perform the reset itself. New tmux answers natively.
            def report_surface():
                seen = 0
                try:
                    while not stop_reports.wait(0.001):
                        count = capture.read_bytes().count(b"\x1b[?1049$p") if capture.exists() else 0
                        for _ in range(seen, count):
                            alternate = terminal.tmux("display-message", "-p", "-t", "ion:0.0", "#{alternate_on}").strip()
                            terminal.send(f"\x1b[?1049;{1 if alternate == '1' else 2}$y")
                        seen = count
                except BaseException as error:
                    report_errors.append(error)
            reporter = threading.Thread(target=report_surface)
            reporter.start()
        terminal.wait(lambda: terminal.editor_matches("", 0, False))
        terminal.send(seed + "\r")
        terminal.wait_turn()
        last_answer = "DONE_" + seed
        for outcome in outcomes:
            fail = outcome != "success"
            prefix = "EDITOR_RETAIN_" + outcome if fail else "EDITOR_ORIGINAL"
            original = prefix + "_TOP_🦀\n" + prefix + "_MIDDLE\n" + prefix + "_BOTTOM"
            edited = "EDITOR_CHANGED_TOP_🦀\nEDITOR_CHANGED_BOTTOM"
            marker = "EDITOR_OUTPUT_" + outcome.upper()
            (cwd / "config.json").write_text(json.dumps({"fail": fail, "marker": marker, "edited": edited, "outcome": outcome}))
            for name in ("received", "release"):
                (cwd / name).unlink(missing_ok=True)
            before = len(requests)
            terminal.send("\x1b[200~" + original + "\x1b[201~")
            terminal.wait(lambda: prefix + "_BOTTOM" in "\n".join(terminal.snapshot()[0]))
            # Park the caret in the middle, not at the live band's anchor.
            terminal.send(b"\x01\x1b[A")

            def middle_caret():
                lines, _, _, column, row = terminal.snapshot()
                return column == 2 and prefix + "_MIDDLE" in lines[row]

            terminal.wait(middle_caret)
            terminal.send(b"\x07")
            terminal.wait(lambda: (cwd / "received").exists() and marker in "\n".join(terminal.snapshot()[0]))
            assert (cwd / "received").read_text() == original, "editor received a partial draft"
            during = "\n".join(terminal.snapshot()[0])
            assert prefix not in during, ("unsent draft survived terminal handoff", mode, during)
            assert len(requests) == before, "editor handoff submitted the draft"
            if outcome == "interrupt":
                # Cooked inherited input delivers SIGINT to the editor AND Ion's
                # process group. Only the foreground editor owns that Ctrl-C.
                terminal.send(b"\x03")
            else:
                (cwd / "release").touch()
            status = "original draft retained" if fail else "Draft returned from editor"
            terminal.wait(lambda: status in "\n".join(terminal.snapshot()[0]))
            lines, dead, _, column, row = terminal.snapshot()
            assert not dead, ("editor cancellation exited Ion", lines)
            start = next(index for index, line in enumerate(lines) if line.startswith("› "))
            expected = original if fail else edited
            assert [line[2:].rstrip() for line in lines[start:start + len(expected.splitlines())]] == expected.splitlines(), lines
            if fail:
                assert (column, row - start) == (2, 1), "failed editor moved the original caret"
            else:
                assert prefix not in "\n".join(lines), "old draft leaked into primary history"
            alternate = terminal.tmux("display-message", "-p", "-t", "ion:0.0", "#{alternate_on}").strip()
            assert alternate == ("1" if mode == "fullscreen" else "0"), "editor changed the selected viewport"
            # Earlier answers may legitimately scroll out of this longer
            # matrix. The newest committed answer must remain visible once.
            assert "\n".join(lines).count(last_answer) == 1, ("editor handoff lost or duplicated the conversation", mode, outcome, lines)
            assert len(requests) == before, "returning from editor submitted input"
            if outcome == "interrupt":
                # A retained dead pane can briefly show the right draft. Prove
                # live input and the original caret before submitting anything.
                terminal.send("KEPT_")
                terminal.wait(lambda: "KEPT_" + prefix + "_MIDDLE" in "\n".join(terminal.snapshot()[0]))
                expected = expected.replace(prefix + "_MIDDLE", "KEPT_" + prefix + "_MIDDLE")
                assert not terminal.snapshot()[1], "queued editor SIGINT exited Ion after restoration"
                assert len(requests) == before, "editor cancellation admitted input"
            terminal.send(b"\r")
            terminal.wait_turn()
            assert len(requests) == before + 1 and user_text(requests[-1]) == expected, requests[-1]
            last_answer = "DONE_" + expected.splitlines()[0]
        terminal.finish()
        modes = json.loads(modes_file.read_text())
        assert modes["restored"], ("editor raw modes became Ion's exit baseline", mode, modes)
        history = terminal.tmux("capture-pane", "-p", "-N", "-S", "-", "-t", "ion:0.0")
        if mode == "inline":
            assert history.count("DONE_" + seed) == 1, "editor handoff duplicated committed history"
        assert "EDITOR_ORIGINAL" not in history, "unsent draft was published into native history"
        view = json.loads(subprocess.run([binary, "--session", terminal.database, "inspect"], env=env, capture_output=True, text=True, check=True, timeout=10).stdout)
        ended = [entry["data"]["reason"] for entry in view["entries"] if entry["kind"] == "turn_ended"]
        assert ended == ["completed"] * (len(outcomes) + 1), ended
        assert not report_errors, report_errors
        print(f"External editor {mode} ({reporting} surface reports): {', '.join(outcomes)}, draft/caret, viewport, caller TTY, exact prompts and native history: OK", flush=True)
    finally:
        # Unblock a helper even if a screen assertion failed while it owned input.
        (cwd / "release").touch()
        stop_reports.set()
        try:
            if reporter is not None:
                reporter.join(timeout=6)
        finally:
            terminal.close()
        assert reporter is None or not reporter.is_alive(), "fixture surface reporter did not stop"
