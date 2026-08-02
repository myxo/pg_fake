#!/usr/bin/env python3
import json
from pathlib import Path

root = Path("target/criterion")


def format_time(nanoseconds):
    if nanoseconds < 1_000:
        return f"{nanoseconds:.2f} ns"
    if nanoseconds < 1_000_000:
        return f"{nanoseconds / 1_000:.2f} us"
    if nanoseconds < 1_000_000_000:
        return f"{nanoseconds / 1_000_000:.2f} ms"
    return f"{nanoseconds / 1_000_000_000:.2f} s"


workloads = sorted(
    path.name
    for path in root.iterdir()
    if (path / "pg_fake" / "new" / "estimates.json").is_file()
    and (path / "postgres_18" / "new" / "estimates.json").is_file()
)

print(f"{'workload':35} {'pg_fake avg':>14} {'postgres avg':>14} {'speedup':>10}")
for workload in workloads:
    fake = json.loads((root / workload / "pg_fake" / "new" / "estimates.json").read_text())
    postgres = json.loads(
        (root / workload / "postgres_18" / "new" / "estimates.json").read_text()
    )
    fake_mean = fake["mean"]["point_estimate"]
    postgres_mean = postgres["mean"]["point_estimate"]
    print(
        f"{workload:35} {format_time(fake_mean):>14} "
        f"{format_time(postgres_mean):>14} {postgres_mean / fake_mean:9.2f}x"
    )
