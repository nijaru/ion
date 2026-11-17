"""Queued intent stays visible and withdrawable without stopping the current Turn."""

import json
import sqlite3
from contextlib import closing

from smoke_input_files import FileTerminal


def exercise_queue_controls(binary, work, env, requests, release):
    for mode in ("inline", "fullscreen"):
        cwd = work / f"queue-controls-{mode}"
        cwd.mkdir()
        before = len(requests)
        release.clear()
        terminal = FileTerminal(binary, cwd, env, mode)
        try:
            def screen():
                return "\n".join(terminal.snapshot()[0])

            terminal.wait(lambda: terminal.editor_matches("", 0, False))
            terminal.send("WAIT_QUEUE\r")
            terminal.wait(lambda: len(requests) == before + 1 and "Working" in screen())
            steering, follow_up, draft = "STEER_QUEUE_🦀", "FOLLOW_QUEUE_雪", "DRAFT_QUEUE_é"
            terminal.send(steering + "\x01\r")
            terminal.wait(lambda: f"Steering: {steering}" in screen())
            terminal.send(follow_up + "\x01")
            terminal.send(b"\x1b[13;3u")
            terminal.wait(lambda: f"Follow-up: {follow_up}" in screen()
                          and f"Steering: {steering}" in screen())
            assert len(requests) == before + 1, "queued input bypassed the held request"
            terminal.send(draft + "\x01")
            terminal.wait(lambda: terminal.editor_matches(draft, 0, False))
            terminal.send(b"\x1b[1;3A")
            terminal.wait(lambda: "Draft returned" in screen()
                          and "2 recovered draft(s)" in screen()
                          and "Steering:" not in screen() and "Follow-up:" not in screen())
            # Each source keeps its own caret. No concatenation, resubmission or
            # dropped newer composer is permitted, regardless of return order.
            seen = set()
            for _ in range(3):
                terminal.wait(lambda: any(terminal.editor_matches(text, 0, False)
                                         for text in (steering, follow_up, draft) if text not in seen))
                shown = next(text for text in (steering, follow_up, draft)
                             if text not in seen and terminal.editor_matches(text, 0, False))
                seen.add(shown)
                if len(seen) < 3:
                    terminal.send(b"\x1b[1;3A")
            assert seen == {steering, follow_up, draft}
            assert "Working" in screen(), "withdrawal stopped the active Turn"
            assert len(requests) == before + 1, "withdrawal submitted a restored draft"
            release.set()

            def ended():
                with closing(sqlite3.connect(f"file:{terminal.database}?mode=ro", uri=True)) as db:
                    return db.execute("SELECT count(*) FROM entries WHERE json_extract(body,'$.kind')='turn_ended'").fetchone()[0] == 1

            terminal.wait(lambda: ended() and "Working" not in screen())
            with closing(sqlite3.connect(f"file:{terminal.database}?mode=ro", uri=True)) as db:
                entries = [json.loads(body) for body, in db.execute("SELECT body FROM entries ORDER BY seq")]
            assert not any(entry["kind"] == "steering" for entry in entries), entries
            assert sum(entry["kind"] == "turn_started" for entry in entries) == 1, entries
            assert len(requests) == before + 1, "cleared follow-up started after settlement"
            terminal.send(b"\x03\x03")
            terminal.wait(lambda: terminal.editor_matches("", 0, False))
            terminal.finish()
        finally:
            release.set()
            terminal.close()
        print(f"Queue {mode}: visible steering/follow-up, edit-all custody, exact carets, no stop/replay: OK", flush=True)
