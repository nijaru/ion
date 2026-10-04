"""Measure warm local Ion process startup without provider/network work."""

import os
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = Path(os.environ.get("ION_SMOKE_BIN", root / "target/debug/ion"))

with tempfile.TemporaryDirectory(prefix="ion-startup-measure-") as temporary:
    work = Path(temporary)
    env = {
        **os.environ,
        "XDG_CONFIG_HOME": str(work / "config"),
        "XDG_STATE_HOME": str(work / "state"),
    }

    def sample():
        started = time.perf_counter_ns()
        result = subprocess.run(
            [binary, "models"],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            check=False,
        )
        elapsed_us = (time.perf_counter_ns() - started) // 1000
        if result.returncode != 0:
            raise RuntimeError(result.stderr.decode(errors="replace"))
        return elapsed_us

    for _ in range(5):
        sample()
    samples = sorted(sample() for _ in range(30))

    def percentile(percent):
        index = min(len(samples) - 1, len(samples) * percent // 100)
        return samples[index]

    print(
        "startup_models: "
        f"min={samples[0]}us "
        f"p50={statistics.median(samples):.0f}us "
        f"p95={percentile(95)}us "
        f"max={samples[-1]}us"
    )
