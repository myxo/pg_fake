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

## Usage

TODO

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

## Testing

The only way to be sure we are compatible with postgres is to make differential tests
against it. Basically, just apply sql to both systems and see that they return the same result.

Right now we run the postgres regression test and property tests with custom generators.
When we have more implemented sql features, we will start proper fuzzing.
