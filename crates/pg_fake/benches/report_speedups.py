#!/usr/bin/env python3
import json
from pathlib import Path

workloads = (
    "create_table",
    "insert_row",
    "update_row",
    "transaction_insert",
    "select_100_rows",
)
root = Path("target/criterion")

for workload in workloads:
    fake = json.loads((root / workload / "pg_fake" / "new" / "estimates.json").read_text())
    postgres = json.loads(
        (root / workload / "postgres_18" / "new" / "estimates.json").read_text()
    )
    fake_mean = fake["mean"]["point_estimate"]
    postgres_mean = postgres["mean"]["point_estimate"]
    print(f"{workload:16} {postgres_mean / fake_mean:8.2f}x faster")
