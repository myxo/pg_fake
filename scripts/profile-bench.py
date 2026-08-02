#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import time
from pathlib import Path

if len(sys.argv) not in (2, 3):
    raise SystemExit("usage: profile-bench.py <benchmark-filter> [duration]")

filter = sys.argv[1]
duration = sys.argv[2] if len(sys.argv) == 3 else "10"
environment = os.environ.copy()
environment["CARGO_PROFILE_BENCH_DEBUG"] = "true"
build = subprocess.run(
    [
        "cargo",
        "bench",
        "--no-run",
        "-p",
        "pg_fake_sqlx",
        "--bench",
        "workloads",
        "--message-format=json",
    ],
    check=True,
    env=environment,
    stdout=subprocess.PIPE,
    text=True,
)
benchmark = next(
    message["executable"]
    for line in build.stdout.splitlines()
    if (message := json.loads(line))["reason"] == "compiler-artifact"
    and message["target"]["name"] == "workloads"
)
report = Path("target/pg_fake-benchmark.sample.txt")
flamegraph = Path("target/pg_fake-benchmark.svg")
process = subprocess.Popen([benchmark, filter, "--bench"])
time.sleep(1)
subprocess.run(
    [
        "sample",
        str(process.pid),
        duration,
        "-mayDie",
        "-fullPaths",
        "-file",
        str(report),
    ],
    check=True,
)
status = process.wait()
with flamegraph.open("w") as output:
    collapsed = subprocess.Popen(
        ["stackcollapse-sample.awk", str(report)], stdout=subprocess.PIPE
    )
    subprocess.run(
        ["flamegraph.pl", "--title", f"pg_fake: {filter}"],
        check=True,
        stdin=collapsed.stdout,
        stdout=output,
    )
    collapsed.stdout.close()
    if collapsed.wait() != 0:
        raise SystemExit("could not collapse sample data")
subprocess.run(["open", "-b", "com.apple.Safari", str(flamegraph)], check=True)
raise SystemExit(status)
