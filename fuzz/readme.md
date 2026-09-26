# Fuzzing

Install the required tools once:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz
```

Set `PG_FAKE_DATABASE_URL` to a PostgreSQL 18 database and run the generated SQL
differential fuzzer:

```sh
PG_FAKE_DATABASE_URL=postgresql://user@127.0.0.1:5432/postgres \
  just fuzz-generated-sql
```

The corpus is retained in `fuzz/corpus/generated_sql_matches_postgres/` and
failures are written to `fuzz/artifacts/generated_sql_matches_postgres/`.

To reproduce and minimize a property failure, copy the reported
`CHAOS_THEORY_REPLAY` value into the regular property test:

```sh
PG_FAKE_DATABASE_URL=postgresql://user@127.0.0.1:5432/postgres \
CHAOS_THEORY_REPLAY=... \
  cargo test -p pg_fake_sqlx --test property_tests generated_sql_matches_postgres_long
```

# Squirrel + AFL++ differential fuzzing

A second fuzzer uses [Squirrel](https://github.com/s3team/Squirrel) as a custom
mutator for AFL++, feeding mutated SQL scripts to a differential target that
executes each statement against both PostgreSQL 18 and pg_fake and aborts on
any divergence (result rows, column metadata, affected-row counts, SQLSTATE).
Everything runs in one Docker image (AFL++, Squirrel, PostgreSQL 18 and the
instrumented target); Docker is required because Squirrel/AFL++ need Linux.

Build the image once (and after changing Rust sources):

```sh
just fuzz-squirrel-build
```

Run a bounded campaign (default 300s; crashes and corpus land in
`fuzz/artifacts/squirrel/default/`):

```sh
just fuzz-squirrel 600
```

Replay a crash to print the per-statement PostgreSQL vs pg_fake diff (exits 1
on mismatch):

```sh
just fuzz-squirrel-repro fuzz/artifacts/squirrel/default/crashes/<crash-file>
```

## Determinism

AFL requires identical coverage for identical inputs. The target achieves this
(~99% reported stability) via: a deferred forkserver (`__afl_manual_init`,
enabled by `--cfg fuzzing`) after a one-time `HashMap` seed warm-up and after
establishing the PostgreSQL connection; per-run state reset through
`ROLLBACK; BEGIN` + `DISCARD ALL`; a fixed `set_random_seed` for pg_fake; and
selective sancov instrumentation (`fuzz/squirrel/rustc_wrapper.sh`) limited to
`pg_fake`, `pg_fake_sqlx`, `sqlparser` and the target itself — dependency
I/O loops (tokio/mio/sqlx) are timing-nondeterministic and would otherwise
pollute the coverage map. Residual instability mostly comes from the
PostgreSQL server under load; crashes should always be confirmed with
`just fuzz-squirrel-repro`.

## Seed constraints

Squirrel's PostgreSQL grammar is a subset of real PostgreSQL. A seed that it
cannot parse yields zero mutations, so seeds in `fuzz/squirrel/seeds/` must:

- be self-contained: every script starts with the `CREATE TABLE` statements it
  later references (Squirrel derives its scope library from the script itself);
- stay short (Squirrel rejects mutation results with more than 8 statements);
- avoid constructs outside the grammar. Known unsupported: `RETURNING`,
  `DELETE`, aggregate calls like `count(*)`/`sum(...)`.

`fuzz/squirrel/mutator_probe.c` checks seed compatibility without running AFL:
it dlopens the Squirrel mutator and prints how many mutations a seed yields.

```sh
docker run --rm --entrypoint bash -v "$PWD:/repo" pg-fake-squirrel -c '
  gcc -o /tmp/probe /repo/fuzz/squirrel/mutator_probe.c -ldl &&
  SQUIRREL_CONFIG=/opt/squirrel-config.yml /tmp/probe /repo/fuzz/squirrel/seeds/basic.sql'
```
