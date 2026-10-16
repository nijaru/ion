#!/usr/bin/env python3
"""Built SDK: foreground join and process loss after a real synced child effect."""
import json
import os
from pathlib import Path
import queue
import sqlite3
import subprocess
import tempfile
import threading


def facts(path):
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
        header = json.loads(db.execute("SELECT header FROM session WHERE id=1").fetchone()[0])
        entries = [json.loads(row[0]) for row in db.execute("SELECT body FROM entries ORDER BY seq")]
    return header, entries


def run(binary, state, env, *mode):
    result = subprocess.run([str(binary), str(state), *mode], env=env,
                            capture_output=True, text=True, timeout=20)
    assert result.returncode == 0, result.stdout + result.stderr


def main():
    repo = Path(__file__).resolve().parents[1]
    target = Path(os.environ.get("CARGO_TARGET_DIR", repo / "target"))
    binary = Path(os.environ.get("ION_SMOKE_DELEGATION_BIN", target / "debug/examples/delegation"))
    assert binary.is_file(), f"build the delegation example first: {binary}"
    with tempfile.TemporaryDirectory(prefix="ion-delegation-smoke-") as temporary:
        root = Path(temporary)
        home = root / "home"
        home.mkdir()
        env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"))
        run(binary, root / "normal", env)
        for mode in ("--resume", "--new"):
            state = root / mode.removeprefix("--")
            child = subprocess.Popen([str(binary), str(state), "--pause-after-effect"], env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            marker = queue.Queue(maxsize=1)
            reader = threading.Thread(target=lambda: marker.put(child.stdout.readline()))
            reader.start()
            try:
                line = marker.get(timeout=15)
                assert line.strip() == "CHILD_EFFECT_SYNCED", line
            finally:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=5)
                reader.join(timeout=5)
                assert not reader.is_alive(), "owned output reader did not join"
                child.stdout.close()
            assert child.returncode != 0, "fixture settled before process loss"
            parent_header, entries = facts(state / "parent.sqlite")
            records = [entry["data"]["record"] for entry in entries if entry["kind"] == "delegation_admitted"]
            assert len(records) == 1 and not any(entry["kind"] == "delegation_settled" for entry in entries)
            record = records[0]
            child_path = Path(record["path"])
            child_header, child_entries = facts(child_path)
            assert child_header["id"] == record["child"]
            assert record["site"]["session"] == parent_header["id"]
            assert child_entries[0] == {"kind": "child_session_of", "data": {"record": record}}
            assert any(entry["kind"] == "assistant" for entry in child_entries)
            assert not any(entry["kind"] in ("tool_result", "turn_ended") for entry in child_entries)
            run(binary, state, env, mode)
            after_header, after = facts(state / "parent.sqlite")
            assert after_header["id"] == parent_header["id"]
            assert [entry["data"]["record"] for entry in after if entry["kind"] == "delegation_admitted"] == records
            index = next(index for index, entry in enumerate(after) if entry["kind"] == "delegation_settled")
            assert after[index]["data"] == {"child": record["child"], "outcome": {"kind": "unknown"}}
            assert after[index + 1]["kind"] == "tool_result", "relationship must close before its outer tool"
            assert facts(child_path) == (child_header, child_entries), "parent recovery rewrote or replayed child work"
            assert list((state / "children").glob("*.sqlite")) == [child_path], "recovery allocated another child"
            assert (state / "effects").read_bytes() == b"child effect\n"
        print("delegation smoke passed: joined foreground work; both recovery policies preserve unknown child effects without replay")


if __name__ == "__main__":
    main()
