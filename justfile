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
