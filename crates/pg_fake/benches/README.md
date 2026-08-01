# Benchmarks

The Criterion suite compares in-process `pg_fake` calls with the same SQL sent
through a `postgres` client connection to PostgreSQL 18. It covers a create/drop
table lifecycle, individual inserts, updates, deletes, explicit transactions,
a 100-row full-table select, and a 100-row multi-key `ORDER BY` with explicit
NULL placement, including an ordered `LIMIT`/`OFFSET` paging workload.

By default, it starts a PostgreSQL 18 Testcontainers container. It detects the
default Colima socket (`~/.colima/default/docker.sock`); set `DOCKER_HOST` for
another Docker socket or profile.

To benchmark an existing PostgreSQL 18 instance instead, set
`PG_FAKE_BENCHMARK_DATABASE_URL`:

```sh
PG_FAKE_BENCHMARK_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres \
  cargo bench --bench workloads
```

Otherwise run:

```sh
cargo bench --bench workloads
python3 crates/pg_fake/benches/report_speedups.py
```

Criterion writes latency and throughput reports under `target/criterion`. The
report script reads their mean latencies and prints the PostgreSQL-to-`pg_fake`
speedup for every workload. The expected result on a local Docker-backed
PostgreSQL is at least 10x; this is not a CI threshold because host and Docker
configuration materially affect the ratio.
