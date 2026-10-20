#!/usr/bin/env python3
"""Built SDK consumer: lose an observation, then explicitly continue the same Turn."""
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading


def main():
    repo = Path(__file__).resolve().parents[1]
    target = Path(os.environ.get("CARGO_TARGET_DIR", repo / "target"))
    binary = Path(os.environ.get("ION_SMOKE_RESTRICTED_BIN", target / "debug/examples/restricted"))
    assert binary.is_file(), f"build the restricted example first: {binary}"
    with tempfile.TemporaryDirectory(prefix="ion-recovery-smoke-") as temporary:
        root = Path(temporary)
        state = root / "state"
        home = root / "home"
        home.mkdir()
        env = {key: os.environ[key] for key in ("PATH", "SHELL", "TMPDIR") if key in os.environ}
        env.update(HOME=str(home), ION_HOME=str(home / "ion"), XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"))
        first = subprocess.Popen([str(binary), str(state), "--pause-after-effect"], env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        marker = queue.Queue(maxsize=1)
        reader = threading.Thread(target=lambda: marker.put(first.stdout.readline()))
        reader.start()
        try:
            line = marker.get(timeout=15)
            assert line.strip() == "COUNTER_EFFECT", line
        finally:
            if first.poll() is None:
                first.kill()
            first.wait(timeout=5)
            reader.join(timeout=5)
            assert not reader.is_alive(), "owned output reader did not join"
            first.stdout.close()
        assert first.returncode != 0, "fixture unexpectedly settled before process loss"
        # The executable checks the original key/Turn, unknown outcome, one model
        # request and zero tool dispatches; its ordinary reopen/redelivery path also runs.
        resumed = subprocess.run([str(binary), str(state), "--resume"], env=env,
                                 capture_output=True, text=True, timeout=20)
        assert resumed.returncode == 0, resumed.stdout + resumed.stderr
        assert "COUNTER_EFFECT" not in resumed.stdout, resumed.stdout
        print("recovery smoke passed: original Turn continued; lost observation stayed unknown")


if __name__ == "__main__":
    main()
