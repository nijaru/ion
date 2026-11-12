"""External-editor handoff through the built client and an actual terminal grid."""

import json
import shlex
import subprocess
import sys

from smoke_input_files import FileTerminal


def exercise_external_editor(binary, work, env, mode, requests, user_text):
    cwd = work / (mode + "-editor")
    cwd.mkdir()
    helper = cwd / "editor.py"
    helper.write_text('''import json, pathlib, sys, time
root = pathlib.Path(__file__).parent
config = json.loads((root / "config.json").read_text())
draft = pathlib.Path(sys.argv[1])
(root / "received").write_text(draft.read_text())
print(config["marker"], flush=True)
while not (root / "release").exists():
    time.sleep(0.01)
draft.write_text(config["edited"] + "\\n")
sys.exit(7 if config["fail"] else 0)
''')
    terminal = FileTerminal(binary, cwd, dict(env, VISUAL=shlex.join([sys.executable, str(helper)])), mode)
    seed = "EDITOR_BASE_" + mode
    try:
        terminal.wait(lambda: terminal.editor_matches("", 0, False))
        terminal.send(seed + "\r")
        terminal.wait_turn()
        for outcome in ("success", "failure", "interrupt"):
            fail = outcome != "success"
            prefix = "EDITOR_RETAIN_" + outcome if fail else "EDITOR_ORIGINAL"
            original = prefix + "_TOP_🦀\n" + prefix + "_MIDDLE\n" + prefix + "_BOTTOM"
            edited = "EDITOR_CHANGED_TOP_🦀\nEDITOR_CHANGED_BOTTOM"
            marker = "EDITOR_OUTPUT_" + outcome.upper()
            (cwd / "config.json").write_text(json.dumps({"fail": fail, "marker": marker, "edited": edited}))
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
            assert "\n".join(lines).count("DONE_" + seed) == 1, "editor handoff duplicated the conversation"
            alternate = terminal.tmux("display-message", "-p", "-t", "ion:0.0", "#{alternate_on}").strip()
            assert alternate == ("1" if mode == "fullscreen" else "0"), "editor changed the selected viewport"
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
        terminal.finish()
        history = terminal.tmux("capture-pane", "-p", "-N", "-S", "-", "-t", "ion:0.0")
        if mode == "inline":
            assert history.count("DONE_" + seed) == 1, "editor handoff duplicated committed history"
        assert "EDITOR_ORIGINAL" not in history, "unsent draft was published into native history"
        view = json.loads(subprocess.run([binary, "--session", terminal.database, "inspect"], env=env, capture_output=True, text=True, check=True, timeout=10).stdout)
        ended = [entry["data"]["reason"] for entry in view["entries"] if entry["kind"] == "turn_ended"]
        assert ended == ["completed"] * 4, ended
        print(f"External editor {mode}: clean handoff, success/failure/SIGINT draft and caret custody, exact prompts and native history: OK", flush=True)
    finally:
        # Unblock a helper even if a screen assertion failed while it owned input.
        (cwd / "release").touch()
        terminal.close()
