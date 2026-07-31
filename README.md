# pg_fake

An in-memory, embeddable fake of PostgreSQL for use as a test double in
automated tests.

## Benchmarks

The Criterion suite compares `pg_fake` with PostgreSQL 18 for create/drop,
insert, update, delete, transaction, and select workloads. See
[`crates/pg_fake/benches/README.md`](crates/pg_fake/benches/README.md) for
Docker/database configuration, commands, reports, and speedup interpretation.

## Differential tests

The differential suite compares `pg_fake` with PostgreSQL 18 and starts it
through Testcontainers by default:

```sh
cargo test --test e2e_unit_tests
```

The default Colima socket (`~/.colima/default/docker.sock`) is detected
automatically. For another Docker socket or Colima profile, set `DOCKER_HOST`.

Set `PG_FAKE_TEST_DATABASE_URL` to use an existing PostgreSQL 18 database
instead:

```sh
PG_FAKE_TEST_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres \
  cargo test --test e2e_unit_tests
```

The tests create uniquely named tables and leave them in the configured target
database. A database dedicated to differential testing is recommended.

