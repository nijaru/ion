"""File-input smoke using current emulator publication, not historical PTY text."""

import re
import sqlite3
import subprocess
import tempfile
import time
from contextlib import closing
from pathlib import Path


class FileTerminal:
    def __init__(self, binary, cwd, env, mode):
        # Unix socket paths must stay short even when the typed workspace is long.
        self.ipc = tempfile.TemporaryDirectory(prefix="ion-file-screen-", dir="/tmp")
        self.env = env
        self.socket = str(Path(self.ipc.name) / "tmux.sock")
        self.database = Path(self.ipc.name) / "session.sqlite"
        config = Path(self.ipc.name) / "tmux.conf"
        config.write_text("set -g status off\nset -g remain-on-exit on\nset -g default-shell /bin/sh\n")
        self.turns = 0
        try:
            # Multiple argv execute Ion directly, without a launcher shell or
            # pipe-pane consumer. The retained pane owns its real exit status.
            self.tmux("-f", str(config), "new-session", "-d", "-s", "ion", "-x", "100", "-y", "30", "-c", str(cwd), "--", str(binary), "--cwd", str(cwd), "--tui-mode", mode, "--session", str(self.database), "chat")
        except BaseException:
            self.close()
            raise

    def tmux(self, *args, check=True):
        result = subprocess.run(["tmux", "-S", self.socket, *args], env=self.env, text=True, capture_output=True, timeout=5, check=check)
        return result.stdout

    def snapshot(self):
        lines = self.tmux("capture-pane", "-p", "-N", "-t", "ion:0.0", ";", "display-message", "-p", "-t", "ion:0.0", "#{pane_dead},#{pane_dead_status},#{cursor_x},#{cursor_y}").splitlines()
        dead, status, column, row = lines.pop().split(",")
        return lines, bool(int(dead)), status, int(column), int(row)

    def send(self, data):
        data = data.encode() if isinstance(data, str) else data
        self.tmux("send-keys", "-t", "ion:0.0", "-H", *[f"{byte:02x}" for byte in data])

    def wait(self, predicate):
        end = time.monotonic() + 10
        while not predicate():
            lines, dead, status, column, row = self.snapshot()
            # An exit can become visible between the predicate and snapshot.
            assert (not dead and time.monotonic() < end) or predicate(), ("pane exited" if dead else "screen condition timed out", status, column, row, "\n".join(lines))
            time.sleep(0.01)

    def editor_matches(self, draft, cursor=None, completion=None):
        lines, dead, _, column, row = self.snapshot()
        if dead:
            return False
        start = next((i for i in range(row, -1, -1) if lines[i].startswith("› ")), None)
        if start is None:
            return False
        end = start + 1
        while end < len(lines) and lines[end].startswith("  "):
            end += 1
        # Erased cells may remain as spaces in tmux's grid. Match the visible
        # row text while assigning only the expected draft's spaces at wraps.
        pattern = "".join(f"({re.escape(line[2:].rstrip(' '))} *)" for line in lines[start:end])
        match = re.fullmatch(pattern, draft)
        if match is None:
            return False
        content = match.groups()
        if cursor is not None:
            before = "".join(content[:row - start])
            if row >= end or not len(before) <= cursor <= len(before) + len(content[row - start]):
                return False
            # Let the same emulator measure Unicode cells. The option's value
            # is literal data, never interpolated into a format or shell command.
            width = int(self.tmux("set-option", "-g", "@ion-file-text", draft[len(before):cursor], ";", "display-message", "-p", "#{w:@ion-file-text}"))
            if column != 2 + width:
                return False
        menu = lines[:start]
        selectable = any("Files · Up / Down select · Tab / Enter complete · Esc close" in line for line in menu) and any(line.startswith("› ") and line[2:].strip() not in ("", "…") for line in menu)
        if completion is True:
            return selectable
        if completion is False:
            return not any("Files ·" in line for line in menu)
        return True

    def resize(self, rows, columns):
        self.tmux("resize-window", "-t", "ion:0", "-x", str(columns), "-y", str(rows))

    def wait_turn(self):
        self.turns += 1

        def settled():
            with closing(sqlite3.connect(f"file:{self.database}?mode=ro", uri=True)) as db:
                completed = db.execute("SELECT count(*) FROM entries WHERE json_extract(body,'$.kind')='turn_ended' AND json_extract(body,'$.data.reason')='completed'").fetchone()[0]
            # A provisional answer is not settlement; require Core's committed
            # fact and the current, cleared idle composer, not an old debug line.
            return completed == self.turns and self.editor_matches("", 0, False) and "Working" not in "\n".join(self.snapshot()[0])

        self.wait(settled)

    def finish(self):
        self.send(b"\x03")
        self.wait(lambda: self.snapshot()[1])
        assert self.snapshot()[2] == "0", self.snapshot()

    def close(self):
        try:
            self.tmux("kill-server", check=False)
        finally:
            self.ipc.cleanup()


def exercise_file_input(binary, work, env, mode, requests, user_text):
    # The independent model request remains the prompt oracle. Completion may
    # consume Enter only after this intended draft/cursor has a selectable row.
    cwd = work / (mode + "-files")
    (cwd / "src").mkdir(parents=True)
    (cwd / "src/main.rs").write_text("fn main() {}\n")
    (cwd / "plain.rs").write_text("fixture\n")
    home_paths = Path(env["HOME"]) / f"paths-{mode}"
    home_paths.mkdir(parents=True)
    (home_paths / "main.rs").write_text("home fixture\n")
    (cwd / "space dir").mkdir()
    (cwd / "space dir/café🦀.rs").write_text("fixture\n")
    terminal = FileTerminal(binary, cwd, env, mode)
    try:
        terminal.wait(lambda: terminal.editor_matches("", 0, False))
        cases = [
            ("nick@example.com", None, "nick@example.com"),
            ("Read @m next", "dismiss", "Read @ma next"),
            ("Read @smrs next", "accept", "Read @src/main.rs next"),
            ("Read @ma", "paste", "Read @src/main.rs "),
            ("Read @ma next", "undo", "Read @ma next"),
            ("Read pl next", "bare", "Read plain.rs next"),
            ("Read ./src/ma next", "accept", "Read ./src/main.rs next"),
            (f"Read {cwd}/src/ma next", "accept", f"Read {cwd}/src/main.rs next"),
            (f"@{cwd}/src/ma next", "accept", f"@{cwd}/src/main.rs next"),
            (f'"{cwd}/src/ma next', "accept", f'"{cwd}/src/main.rs" next'),
            (f"Read ~/paths-{mode}/ma next", "accept", f"Read ~/paths-{mode}/main.rs next"),
            ('Read "space dir/ca next', "accept", 'Read "space dir/café🦀.rs" next'),
            ('Read @"space d next', "directory", 'Read @"space dir/café🦀.rs" next'),
        ]
        if mode == "fullscreen":
            cases.append(("Read @ma next", "clipped", "Read @ma next"))
        for prompt, action, expected in cases:
            before = len(requests)

            def wait_edit(draft, cursor=None, completion=None):
                terminal.wait(lambda: len(requests) != before or terminal.editor_matches(draft, cursor, completion))
                assert len(requests) == before, ("completion/dismissal sent a prompt", prompt, action, user_text(requests[-1]))

            terminal.send(b"\x1b[200~" + prompt.encode() + b"\x1b[201~" if action == "paste" else prompt)
            if action:
                cursor = len(prompt) if action == "paste" else len(prompt) - len(" next")
                if action != "paste":
                    terminal.send(b"\x1b[D" * len(" next"))
                menu_draft = prompt
                if action == "bare":
                    terminal.send(b"\t")
                    terminal.send("a")  # Forced completion survives ordinary typing.
                    menu_draft = prompt[:cursor] + "a" + prompt[cursor:]
                    cursor += 1
                if prompt.startswith("@"):
                    # A selectable earlier prefix is not this intended cursor.
                    terminal.send(b"\x1b[D")
                    wait_edit(prompt, cursor - 1, True)
                    assert not terminal.editor_matches(prompt, cursor, True), "wrong cursor granted completion readiness"
                    assert not terminal.editor_matches(prompt.replace("/ma next", "/max next"), cursor - 1, True), "wrong draft granted completion readiness"
                    terminal.send(b"\x1b[C")
                wait_edit(menu_draft, cursor, True)
                if action == "clipped":
                    # Do not wait for a redraw after resize. Old visible rows
                    # cannot authorize Enter in the now-clipped viewport.
                    terminal.resize(30, 2)
                    terminal.send(b"\r")
                elif action == "dismiss":
                    terminal.send("a")
                    terminal.send(b"\x1b")
                    wait_edit(expected, completion=False)
                else:
                    terminal.send(b"\t" if action == "directory" else b"\r")
                    if action == "directory":
                        wait_edit('Read @"space dir/" next', len('Read @"space dir/'), True)
                        terminal.send(b"\t")
                    wait_edit("Read @src/main.rs next" if action == "undo" else expected, completion=False)
                    if action == "undo":
                        terminal.send(b"\x1f\x1b")
                        wait_edit(expected, completion=False)
            else:
                wait_edit(prompt, len(prompt), False)
            if action != "clipped":
                assert len(requests) == before, ("completion/dismissal sent a prompt", prompt, action, user_text(requests[-1]))
                terminal.send(b"\r")
            terminal.wait(lambda: len(requests) == before + 1)
            assert user_text(requests[-1]) == expected, user_text(requests[-1])
            if action == "clipped":
                terminal.resize(30, 100)
            terminal.wait_turn()
        terminal.finish()
    finally:
        terminal.close()
    print(f"File input {mode}: literal email, fuzzy references, bare/relative/absolute/home paths, dismiss/accept/undo and quoted directories" + ("; clipped menu during resize" if mode == "fullscreen" else "") + ": OK", flush=True)
