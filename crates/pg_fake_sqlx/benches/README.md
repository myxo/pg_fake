# Benchmarks

The Criterion suite compares `pg_fake` through its in-process SQLx driver with
the same SQL sent through a `postgres` client connection to PostgreSQL 18. It covers a create/drop
table lifecycle, individual constrained explicit/defaulted inserts, updates,
deletes, explicit transactions at READ COMMITTED and REPEATABLE READ, row-lock
acquisition with `SELECT ... FOR UPDATE`, a 100-row full-table select, and a
100-row multi-key `ORDER BY` with explicit NULL placement, including an ordered
`LIMIT`/`OFFSET` paging workload. The insert workloads include primary-key,
not-null, default, and column- and table-level `CHECK` validation.

Diagnostic groups isolate costs within `pg_fake`:

- `adapter_overhead_select_100_rows` compares the native core API with SQLx;
- `core_parsed_vs_prepared_point_select` compares one-shot parsing and analysis
  with prepared reuse;
- `transaction_history_point_select` measures lookup after 1, 100, 10,000, and
  100,000 completed transactions;
- `mvcc_old_snapshot_read` measures reads through version chains retained by a
  long-lived repeatable-read snapshot;
- `point_lookup_index_vs_scan` compares a primary-key predicate with an
  equivalent heap predicate at 100 and 10,000 rows;
- `concurrent_uncontended_reads` compares sequential and parallel sessions, and
  `concurrent_same_row_contention` exercises a blocking same-row update.

The point-lookup comparison is intentionally useful before index-assisted query
execution exists: similar timings expose the missing optimization, while future
changes can demonstrate the expected separation. The MVCC benchmark retains old
versions intentionally; the regular update benchmark constructs a fresh fake
database per timed update so its latency does not drift with an ever-growing
version chain.

By default, it starts a PostgreSQL 18 Testcontainers container. It detects the
default Colima socket (`~/.colima/default/docker.sock`); set `DOCKER_HOST` for
another Docker socket or profile.

To benchmark an existing PostgreSQL 18 instance instead, set
`PG_FAKE_DATABASE_URL`:

```sh
PG_FAKE_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres \
  cargo bench -p pg_fake_sqlx --bench workloads
```

Otherwise run:

```sh
cargo bench -p pg_fake_sqlx --bench workloads
python3 crates/pg_fake_sqlx/benches/report_speedups.py
```

Criterion writes latency and throughput reports under `target/criterion`. The
report script discovers every workload with paired `pg_fake` and PostgreSQL
results and prints their average latencies together with the
PostgreSQL-to-`pg_fake` speedup. It also prints readable tables for adapter and
prepared-statement overhead, transaction-history and MVCC scaling, indexed
predicates, and concurrent sessions. The expected result on a local
Docker-backed PostgreSQL is at least 10x; this is not a CI threshold because
host and Docker configuration materially affect the ratio.
