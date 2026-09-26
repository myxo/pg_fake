# CI-style checks

default: check

# Build all workspace crates
build:
    cargo build

# Start the pg_fake SQL REPL
repl:
    cargo run -p pg_fake_cli

# Run all tests
test:
    cargo test

# Run PostgreSQL 18 comparison benchmarks
bench:
    cargo bench -p pg_fake_benchmarks --bench workloads

# Fuzz generated SQL against PostgreSQL 18
fuzz-generated-sql:
    cargo +nightly fuzz run generated_sql_matches_postgres

# Build the squirrel + AFL++ differential fuzzing image
fuzz-squirrel-build:
    docker build -f fuzz/squirrel/Dockerfile -t pg-fake-squirrel .

# Run a bounded squirrel + AFL++ campaign against PostgreSQL 18
fuzz-squirrel seconds='300':
    mkdir -p fuzz/artifacts/squirrel
    docker run --rm -e AFL_RUN_SECONDS={{seconds}} \
      -v {{justfile_directory()}}/fuzz/artifacts/squirrel:/out \
      -v {{justfile_directory()}}/fuzz/squirrel/seeds:/opt/seeds:ro pg-fake-squirrel

# Replay a squirrel crash against PostgreSQL and pg_fake and print the diff
fuzz-squirrel-repro crash:
    docker run --rm --entrypoint bash \
      -v {{justfile_directory()}}:/repo pg-fake-squirrel -c '\
      su postgres -s /bin/bash -c "/usr/lib/postgresql/18/bin/pg_ctl \
        -D /var/lib/postgresql/data -l /tmp/pg.log -w -t 60 start" >/dev/null && \
      PG_FAKE_SQUIRREL_POSTGRES_URL="postgresql://postgres@localhost/postgres?host=/var/run/postgresql" \
      /opt/fuzz-bin/squirrel_matches_postgres "$(cat /repo/{{crash}})"'

# Record benchmark results as the committed baseline
bench-record:
    cargo x bench record

# Record one pg_fake benchmark and open its flame graph
profile-bench filter duration='10':
    scripts/profile-bench.py {{filter}} {{duration}}

# Run clippy on all workspace crates
lint:
    cargo clippy --all-targets -- -D warnings

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Apply formatting
fmt:
    cargo fmt --all

# Full CI check: fmt, clippy, test
check: fmt-check lint test

# Run the complete Task 30 application-workload conformance gate
task30-gate:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace --all-features -- --skip _long
    CHAOS_THEORY_CHECK_ITERS=10000 CHAOS_THEORY_CHECK_TIME=600s cargo test -p pg_fake_sqlx --features time --test property_tests _long
