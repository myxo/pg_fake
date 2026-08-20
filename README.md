# pg_fake

An in-memory, embeddable fake of PostgreSQL for use as a test double in
automated tests.

## Command-line CLI

Run a SQL file against a fresh in-memory database:

```sh
cargo run -p pg_fake_cli -- path/to/script.sql
```

Without a file argument, it starts an interactive shell. Finish SQL statements
with `;`; use `\q` or EOF to exit:

```sh
cargo run -p pg_fake_cli
```

## Phase 2 SQL support

Phase 2 covers joins and correlated subqueries, aggregates with `GROUP BY` /
`HAVING`, `DISTINCT`, query-producing DML, sequences, serial and identity
columns, foreign keys, UUID, and date/time, timestamp, and interval values.
These features work through both the native API and SQLx.

The remaining major gaps are Phase 3 features: CTEs, `ON CONFLICT`, window
functions, views, JSON/JSONB, arrays, savepoints, general session GUCs,
serializable isolation, and transactional DDL. `OVERRIDING SYSTEM VALUE` /
`OVERRIDING USER VALUE` and identity declarations with multiple sequence
options remain deferred until `sqlparser-rs` exposes them.

## Lock timeout

Row-lock waits time out after one second by default. Configure the database
default with the builder, or change an individual session with SQL:

```rust
let db = Db::builder()
    .lock_timeout(Duration::from_millis(250))
    .build();
let mut session = db.session();
session.execute("SET lock_timeout = '100ms'")?;
```

Deadlocks are detected from the transaction wait-for graph as soon as a new
row-lock wait closes a cycle. The transaction with the highest XID in that
cycle (the newest transaction) is chosen deterministically as the victim. Its
blocked statement returns `40P01`, and the transaction remains failed while
retaining its locks until the caller issues `ROLLBACK`.

## Parameters and prepared statements

Use `$1` placeholders with `query` or the non-breaking `execute_params` method.
Call `prepare` to parse and analyze a single statement once, then reuse the
owned statement with different typed values:

```rust
let insert = session.prepare("INSERT INTO items VALUES ($1, $2)")?;
session.execute_prepared(
    &insert,
    &[Value::Int4(1), Value::Text("first".into())],
)?;

let select = session.prepare("SELECT * FROM items WHERE id = $1")?;
let rows = session.query_prepared(&select, &[Value::Int4(1)])?;
```

One-shot parameterized calls and prepared statements accept exactly one SQL
statement. Placeholder types are inferred from their Phase-1 expression and
column contexts; supplied values are checked using PostgreSQL implicit-cast
rules. The highest placeholder number determines the argument count, so a
statement using only `$2` still requires two values.

A zero timeout waits indefinitely, matching PostgreSQL.

## Multi-statement execution

`execute` accepts PostgreSQL simple-query batches and returns one public
`StatementResult` per statement:

```rust
let results = session.execute(
    "CREATE TABLE items (id INTEGER); \
     INSERT INTO items VALUES (1), (2); \
     SELECT * FROM items ORDER BY id",
)?;
```

Each result is either `StatementResult::Affected(u64)` or
`StatementResult::Query(QueryResult)`. PostgreSQL transaction boundaries are
preserved: without explicit transaction control the whole batch is one
implicit transaction, execution stops at the first error, and preceding
changes are rolled back. Parameterized and prepared calls remain
single-statement operations.

## Benchmarks

The Criterion suite compares `pg_fake` with PostgreSQL 18. See
[`crates/pg_fake_benchmarks/README.md`](crates/pg_fake_benchmarks/README.md) for
Docker/database configuration, commands, reports, and speedup interpretation.

## Differential tests

The differential suite compares `pg_fake` with PostgreSQL 18 and starts it
through Testcontainers by default:

```sh
cargo test --tests
```

The default Colima socket (`~/.colima/default/docker.sock`) is detected
automatically. For another Docker socket or Colima profile, set `DOCKER_HOST`.

Set `PG_FAKE_DATABASE_URL` in the workspace-root `.env` file to use an existing
PostgreSQL 18 database instead:

```sh
PG_FAKE_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres
```

An exported environment variable takes precedence. For a one-off run:

```sh
PG_FAKE_DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres \
  cargo test --tests
```

The property test runs `pg_fake` through its SQLx driver, generates stateful
sequences of valid SQL, and compares every statement with PostgreSQL.
`chaos_theory` prints `CHAOS_THEORY_REPLAY` when it
finds a failing sequence. Generated tables are dropped during success and
failure cleanup. The intentional-error examples leave uniquely named tables in
the configured target database, so a database dedicated to differential testing
is recommended.
