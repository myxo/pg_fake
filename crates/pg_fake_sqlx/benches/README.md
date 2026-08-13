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

PostgreSQL workloads use ordinary tables with stable names inside the
`pgfake_benchmark` schema. The suite takes a database-scoped advisory lock,
recreates that schema before benchmarking, and drops it afterward.

To benchmark an existing PostgreSQL 18 instance instead, set
`PG_FAKE_DATABASE_URL`. The connected role must be able to create and drop the
`pgfake_benchmark` schema, which is reserved for this suite:

```sh
PG_FAKE_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres \
  cargo x bench
```

Otherwise run:

```sh
cargo x bench
```

The command runs Criterion and prints every benchmark's average latency, then
prints the relative timing for every comparison declared by the shared benchmark
catalog. Criterion writes latency and throughput reports under `target/criterion`.
