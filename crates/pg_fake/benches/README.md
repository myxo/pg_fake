# Benchmarks

The Criterion suite compares in-process `pg_fake` calls with the same SQL sent
through a `postgres` client connection to PostgreSQL 18. It covers a create/drop
table lifecycle, individual constrained explicit/defaulted inserts, updates,
deletes, explicit transactions at READ COMMITTED and REPEATABLE READ, row-lock
acquisition with `SELECT ... FOR UPDATE`, a 100-row full-table select, and a
100-row multi-key `ORDER BY` with explicit NULL placement, including an ordered
`LIMIT`/`OFFSET` paging workload. The insert workloads include primary-key,
not-null, default, and column- and table-level `CHECK` validation.

By default, it starts a PostgreSQL 18 Testcontainers container. It detects the
default Colima socket (`~/.colima/default/docker.sock`); set `DOCKER_HOST` for
another Docker socket or profile.

To benchmark an existing PostgreSQL 18 instance instead, set
`PG_FAKE_DATABASE_URL`:

```sh
PG_FAKE_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres \
  cargo bench --bench workloads
```

Otherwise run:

```sh
cargo bench --bench workloads
python3 crates/pg_fake/benches/report_speedups.py
```

Criterion writes latency and throughput reports under `target/criterion`. The
report script discovers every workload with results for both engines and prints
their average latencies together with the PostgreSQL-to-`pg_fake` speedup. The
expected result on a local Docker-backed PostgreSQL is at least 10x; this is not
a CI threshold because host and Docker configuration materially affect the
ratio.
