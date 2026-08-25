# Benchmarks

The Criterion suite compares `pg_fake` with PostgreSQL 18 through SQLx. It
covers a create/drop table lifecycle, individual constrained
explicit/defaulted inserts, updates, deletes, explicit transactions at READ
COMMITTED and REPEATABLE READ, row-lock
acquisition with `SELECT ... FOR UPDATE`, a 100-row full-table select, a simple
filtered heap select, the same filtered select over a primary key, and a
100-row multi-key `ORDER BY` with explicit NULL placement, including an ordered
`LIMIT`/`OFFSET` paging workload. The insert workloads include primary-key,
not-null, default, and column- and table-level `CHECK` validation. They also
cover a sequence-backed identity insert with `RETURNING`, and a UUID key lookup
that applies timestamp-with-time-zone and interval arithmetic.

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

The point-lookup comparison isolates native core index execution across table
sizes, while the paired SQLx filtered selects compare otherwise identical heap
and primary-key queries against PostgreSQL. The MVCC benchmark retains old
versions intentionally; the regular update benchmark deletes its fixture row
after each timed update.

By default, it starts a PostgreSQL 18 Testcontainers container. It detects the
default Colima socket (`~/.colima/default/docker.sock`); set `DOCKER_HOST` for
another Docker socket or profile.

PostgreSQL workloads use ordinary tables with stable names inside the
`pgfake_benchmark` schema. The suite takes a database-scoped advisory lock,
recreates that schema before benchmarking, and drops it afterward.

To benchmark an existing PostgreSQL 18 instance instead, set
`PG_FAKE_DATABASE_URL` in the workspace-root `.env` file. The connected role
must be able to create and drop the `pgfake_benchmark` schema, which is reserved
for this suite:

```sh
PG_FAKE_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres
```

An exported environment variable takes precedence. For a one-off run:

```sh
PG_FAKE_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres \
  cargo x bench
```

Otherwise run:

```sh
cargo x bench
```

The command restores the baseline committed under `results/`, runs Criterion,
and prints Criterion's statistical changes. It also prints the current CPU and
system information, every benchmark's average latency, and the relative timing
for every comparison declared by the shared benchmark catalog. It does not
modify the committed results.

After making an intentional performance change, replace the committed baseline:

```sh
cargo x bench record
```

This stores Criterion's compact raw JSON baseline, `environment.json`, and a
readable `report.md` under `results/`. Transient comparison data remains under
`target/criterion`; the wrapper disables HTML and plot generation.

Direct `cargo bench -p pg_fake_benchmarks --bench workloads` remains an ordinary
Criterion run and does not read or write the committed results.
