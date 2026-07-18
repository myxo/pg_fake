# CI-style checks

default: check

# Build all workspace crates
build:
    cargo build

# Run all tests
test:
    cargo test

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
