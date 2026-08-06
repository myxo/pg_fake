# CI-style checks

default: check

# Build all workspace crates
build:
    cargo build

# Run all tests
test:
    cargo test

# Run PostgreSQL 18 comparison benchmarks
bench:
    PG_FAKE_DATABASE_URL='postgresql://myxo@127.0.0.1:5432/postgres' cargo bench -p pg_fake_sqlx --bench workloads

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
