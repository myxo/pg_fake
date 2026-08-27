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
  cargo test -p pg_fake_sqlx --test property_tests generated_sql_matches_postgres
```
