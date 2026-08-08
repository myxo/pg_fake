#!/usr/bin/env python3
import json
import math
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


def mean(*parts):
    path = root.joinpath(*(str(part) for part in parts), "new", "estimates.json")
    if not path.is_file():
        return None
    estimates = json.loads(path.read_text())
    return estimates["mean"]["point_estimate"]


def relative_change(baseline, candidate):
    ratio = candidate / baseline
    if ratio > 1:
        return f"{ratio:.2f}x slower"
    if ratio < 1:
        return f"{1 / ratio:.2f}x faster"
    return "same"


def print_table(headers, rows):
    widths = [len(header) for header in headers]
    for row in rows:
        for index, value in enumerate(row):
            widths[index] = max(widths[index], len(value))

    def formatted(row):
        cells = [row[0].ljust(widths[0])]
        cells.extend(
            value.rjust(widths[index]) for index, value in enumerate(row[1:], 1)
        )
        return "  ".join(cells)

    print(formatted(headers))
    print(formatted(tuple("-" * width for width in widths)))
    for row in rows:
        print(formatted(row))


def paired_postgres_results():
    rows = []
    speedups = []
    for path in sorted(root.iterdir()):
        fake = mean(path.name, "pg_fake")
        postgres = mean(path.name, "postgres_18")
        if fake is None or postgres is None:
            continue
        speedup = postgres / fake
        speedups.append(speedup)
        rows.append(
            (
                path.name,
                format_time(fake),
                format_time(postgres),
                f"{speedup:.2f}x",
            )
        )
    return rows, speedups


def internal_comparisons():
    comparisons = [
        (
            "SQLx adapter",
            ("adapter_overhead_select_100_rows", "core"),
            "core",
            ("adapter_overhead_select_100_rows", "sqlx"),
            "sqlx",
        ),
        (
            "Prepared reuse",
            ("core_parsed_vs_prepared_point_select", "parse_and_analyze"),
            "parse/analyze",
            ("core_parsed_vs_prepared_point_select", "prepared_reuse"),
            "prepared",
        ),
        (
            "Parallel reads",
            ("concurrent_uncontended_reads", "sequential"),
            "sequential",
            ("concurrent_uncontended_reads", "parallel"),
            "parallel",
        ),
    ]
    rows = []
    for label, baseline_path, baseline_name, candidate_path, candidate_name in comparisons:
        baseline = mean(*baseline_path)
        candidate = mean(*candidate_path)
        if baseline is None or candidate is None:
            continue
        rows.append(
            (
                label,
                f"{baseline_name}: {format_time(baseline)}",
                f"{candidate_name}: {format_time(candidate)}",
                relative_change(baseline, candidate),
            )
        )
    return rows


def scaling_results(group, values):
    measurements = [
        (value, measurement)
        for value in values
        if (measurement := mean(group, value)) is not None
    ]
    if not measurements:
        return []
    baseline = measurements[0][1]
    return [
        (f"{value:,}", format_time(measurement), relative_change(baseline, measurement))
        for value, measurement in measurements
    ]


def index_results():
    rows = []
    for size in [100, 10_000]:
        heap = mean("point_lookup_index_vs_scan", "heap_scan", size)
        indexed = mean("point_lookup_index_vs_scan", "unique_index", size)
        if heap is None or indexed is None:
            continue
        rows.append(
            (
                f"{size:,}",
                format_time(heap),
                format_time(indexed),
                relative_change(heap, indexed),
            )
        )
    return rows


if not root.is_dir():
    raise SystemExit(
        "target/criterion does not exist; run "
        "`cargo bench -p pg_fake_sqlx --bench workloads` first"
    )

print("pg_fake Criterion report")
print("========================")

postgres_rows, postgres_speedups = paired_postgres_results()
if postgres_rows:
    print("\nPostgreSQL comparison (higher speedup is better)\n")
    print_table(
        ("workload", "pg_fake avg", "postgres avg", "speedup"), postgres_rows
    )
    geometric_mean = math.exp(
        sum(math.log(speedup) for speedup in postgres_speedups)
        / len(postgres_speedups)
    )
    print(f"\nGeometric mean speedup: {geometric_mean:.2f}x")

comparison_rows = internal_comparisons()
if comparison_rows:
    print("\nInternal comparisons (candidate relative to baseline)\n")
    print_table(("comparison", "baseline", "candidate", "result"), comparison_rows)

transaction_rows = scaling_results(
    "transaction_history_point_select", [1, 100, 10_000, 100_000]
)
if transaction_rows:
    print("\nTransaction-history scaling (relative to the first result)\n")
    print_table(("completed txns", "average", "change"), transaction_rows)

mvcc_rows = scaling_results("mvcc_old_snapshot_read", [1, 100, 10_000])
if mvcc_rows:
    print("\nMVCC old-snapshot scaling (relative to one retained version)\n")
    print_table(("updates", "average", "change"), mvcc_rows)

index_rows = index_results()
if index_rows:
    print("\nUnique-index predicate versus heap predicate\n")
    print_table(("rows", "heap scan", "unique index", "index result"), index_rows)

contention = mean("concurrent_same_row_contention", "wait_then_rollback")
if contention is not None:
    print("\nSame-row contention\n")
    print_table(
        ("workload", "average"),
        [("wait then rollback", format_time(contention))],
    )

if not any(
    [
        postgres_rows,
        comparison_rows,
        transaction_rows,
        mvcc_rows,
        index_rows,
        contention is not None,
    ]
):
    raise SystemExit("no Criterion estimates were found under target/criterion")
