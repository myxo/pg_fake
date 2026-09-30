# E11: reuse session defaults

2026-09-30, baseline `c5adffb`, native release build on macOS arm64,
Cargo 1.98.1, default features (no execution tracing). Each process runs 10,000
operations per case on a fresh database. Fixture and custom-setting setup are
excluded; INSERT intentionally grows the table. No concurrent compilation or
benchmarking during measurements. These are native API timings, not SQLx or
PostgreSQL speedup claims. The probe includes no internal warmup.

Reproduce with `cargo build -p pg_fake --release --example settings_probe`, then
run `target/release/examples/settings_probe` three times. Run the same probe on
the baseline source and changed source. Values below are microseconds/operation.

| Case | Custom settings | Before runs | After runs | Mean change |
| --- | ---: | --- | --- | --- |
| INSERT | 0 | 24.331, 22.698, 22.790 | 16.366, 13.891, 14.068 | 23.273 → 14.775 (36.5% lower) |
| UPDATE | 0 | 31.418, 31.639, 31.612 | 24.672, 22.740, 22.813 | 31.556 → 23.408 (25.8% lower) |
| Transaction INSERT | 0 | 23.956, 24.363, 23.840 | 15.168, 15.192, 15.407 | 24.053 → 15.256 (36.6% lower) |
| Prepared read | 0 | 1.191, 1.182, 1.166 | 1.171, 1.158, 1.247 | 1.180 → 1.192 (1.0% higher) |
| INSERT | 32 | 22.428, 22.574, 22.611 | 13.942, 14.013, 14.085 | 22.538 → 14.013 (37.8% lower) |
| UPDATE | 32 | 31.368, 31.834, 31.480 | 22.617, 22.982, 22.732 | 31.561 → 22.777 (27.8% lower) |
| Transaction INSERT | 32 | 23.803, 24.831, 23.932 | 15.163, 15.402, 15.294 | 24.189 → 15.286 (36.8% lower) |
| Prepared read | 32 | 1.190, 1.350, 1.187 | 1.170, 1.165, 1.188 | 1.242 → 1.174 (5.5% lower) |

Retained: replace repeated default reconstruction with a shared immutable
session snapshot, including RESET and SET DEFAULT. This removes code and
improves all measured write cases above 20%. Prepared reads are a regression
sentinel, with changes near measurement noise. Unconditional GUC write-back
is unchanged; this result does not claim the prior bundled optimization.

Validation: `cargo test -p pg_fake --lib` (282 passed),
`cargo test -p pg_fake_sqlx --test settings` (6 passed, including PostgreSQL
differential tests), `cargo fmt --all -- --check`. Independent review found no
issue. Paired SQLx performance and the broader tier sweep remain for the final
plan audit.
