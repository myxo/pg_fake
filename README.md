# pg_fake

An in-memory, embeddable fake of PostgreSQL for use as a test double in
automated tests.

## Project philosophy

I love end-to-end property tests. This is the best way to test your project.
But it's challenging to make it work fast and reliably. Especially if
you have a network-only multi-process beast database like postgres.

So I wanted to create a fake double that:
- works exactly like postgres (in most common cases)
- faster than postgres
- deterministic

So my idea: if we just rewrite pg without filesystem and network layers, this
would be a win.

> [!WARNING]
> This is Work In Progres library

See the [source map](docs/source_map.md) for module responsibilities and the
path a statement takes through the engine.

## Usage

TODO

Run the SQLx adapter example:

```sh
cargo run -p pg_fake_sqlx --bin sqlx_example
```

Enable the adapter's optional `time` feature to bind and decode
`time::OffsetDateTime` as PostgreSQL `timestamptz`:

```toml
pg_fake_sqlx = { version = "0.1", features = ["time"] }
```

Transactions support nested `SAVEPOINT`, `ROLLBACK TO SAVEPOINT`, and
`RELEASE SAVEPOINT`, including recovery after an error. SQLx nested
transactions use the same savepoint state, including rollback on drop.

```sql
BEGIN;
CREATE TABLE items (id INTEGER PRIMARY KEY);
INSERT INTO items VALUES (1);
SAVEPOINT retry;
INSERT INTO items VALUES (2);
ROLLBACK TO SAVEPOINT retry;
RELEASE SAVEPOINT retry;
COMMIT;
SELECT * FROM items; -- returns 1
```

## Fixture snapshots

`db.snapshot()` returns an independent database containing committed rows and
catalog objects. New sessions start with database defaults. In-flight writes,
temporary objects, session settings, and locks are excluded. Sequence allocation,
mock time, and seeded random state are copied and evolve independently in each
fork. Taking a snapshot briefly locks the source database while copying it.

## Session settings

`SET`, `SET SESSION`, `SHOW`, `RESET`, `SET ... TO DEFAULT`, and `RESET ALL`
share a typed registry. Semantic settings are `TimeZone`, `lock_timeout`,
`statement_timeout`, `search_path`, and `default_transaction_isolation`.
`application_name` and UTF-8 `client_encoding` are supported, including
`SET NAMES`, `SET SCHEMA`, and `TIME ZONE` aliases. Fresh sessions use the
configured database lock timeout, UTC, and `"$user", public` as their search path.
`SET LOCAL` applies to every registered setting and restores values across
commit, rollback, and savepoints. `current_setting` and `set_config` use the
same validation and support PostgreSQL custom names containing a namespace dot.
`transaction_isolation` reports the active transaction level.

```sql
SET SESSION lock_timeout = '1.5s';
SHOW lock_timeout;
SET TIME ZONE 'Europe/Paris';
SET application_name = 'test-worker';
SELECT set_config('application_name', 'local-worker', true);
SELECT current_setting('application_name');
SHOW transaction_isolation;
RESET ALL;
```

Known planner settings are validated and tracked without changing execution;
strict mode rejects them. Unknown names are rejected rather than accepted by
prefix. READ COMMITTED and REPEATABLE READ defaults are implemented; other
isolation levels and non-UTF-8 encodings remain explicit unsupported features.
Time zones support named IANA zones and numeric offsets; arbitrary POSIX zone
rules and interval-valued settings remain outside this registry's current surface.

## Command-line interface

Run a SQL file against a fresh in-memory database:

```sh
cargo run -p pg_fake_cli -- path/to/script.sql
```

Without a file argument, it starts an interactive shell. Finish SQL statements
with `;`; use `\q` or EOF to exit:

```sh
cargo run -p pg_fake_cli
```

## Benchmarks

The Criterion suite compares `pg_fake` with PostgreSQL 18. See
[`crates/pg_fake_benchmarks/results/report.md`](crates/pg_fake_benchmarks/results/report.md)
for current numbers.
Run `cargo x bench tier1_` for essential operations, `tier2_` for important
application features, or `tier3_` for rare workloads and diagnostics. See the
[benchmark guide](crates/pg_fake_benchmarks/README.md) for filtering and recording.

## Testing

The only way to be sure we are compatible with postgres is to make differential tests
against it. Basically, just apply sql to both systems and see that they return the same result.

We run the PostgreSQL regression tests and differential property tests with custom generators.

SQLx tests use `PG_FAKE_DATABASE_URL` from the environment or `.env` when set;
otherwise they start PostgreSQL 18 containers. The configured database must be
dedicated to testing. Property tests run concurrently in separate databases;
the configured PostgreSQL role must have `CREATEDB` permission. Each suite drops
its database when it finishes, including when a test panics.

Configure Docker before launching tests. For Colima's default socket:

```sh
DOCKER_HOST="unix://${HOME}/.colima/default/docker.sock" cargo test -p pg_fake_sqlx
```

Run property tests ending in `_long` with an extended budget of 10,000
iterations or 600 seconds per test:

```sh
CHAOS_THEORY_CHECK_ITERS=10000 CHAOS_THEORY_CHECK_TIME=600s cargo test -p pg_fake_sqlx --features time --test property_tests _long
```
