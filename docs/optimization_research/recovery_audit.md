# Optimization recovery audit — 2026-09-30

The recovery continues the user's instruction to measure retained changes and
keep experiment-sized commits. Automatic approval review initially blocked Git
writes; the user's subsequent explicit authorization resolved that block. The
measured experiments now have separate commits. Historical September 29 bundled
results are not proof of the current implementation.

## Environment

- macOS 15.7.9 (24G830), arm64; Cargo 1.98.1 (797e8a9bc, 2026-08-05).
- Native/probe builds use release optimization and default features, with no
  execution tracing. Probe records describe API, cache state, fixtures, decoding,
  iterations and before/after build commands individually.
- The configured comparison server reports PostgreSQL 18.6 (Homebrew), aarch64,
  Apple clang 16.0.0. Connection transport is TCP.
- Server settings: `work_mem=4MB`, `shared_buffers=128MB`, `jit=on`,
  `max_parallel_workers_per_gather=2`, `fsync=off`,
  `synchronous_commit=off`, `TimeZone=UTC`.
- Read-only reproduction: `cargo run -p pg_fake_sqlx --release --example
  postgres_environment`. The example reads `PG_FAKE_DATABASE_URL` via dotenv and
  reports settings without printing credentials. It ran successfully against
  the local server. The measurements describe this development configuration.

## Recovery experiments

| Experiment | Current evidence | Commit status |
| --- | --- | --- |
| E11 shared defaults | [Three-run native comparison and settings validation](e11_defaults.md); retained and reviewed | `8310db2` |
| E7 scalar temporal binding | [Native comparison and PostgreSQL validation](e7_temporal.md); retained and reviewed | `e569753` |
| E13 ordered reads/pages | [18-case native/SQLx comparison](e13_ordering.md); bounded candidate retained and reviewed; unbounded candidate rejected | `971f0cd` |
| E13 bound DML companions | [Existing inline/bound API measurements](e13_dml_bindings.md); probe reviewed; no executor change | `c072977` |
| E14 window templates | [22-case native/SQLx comparison](e14_windows.md); retained and reviewed | `64c30e9` |
| High-cardinality grouping | [Fresh profile and candidate comparisons](grouping.md); normalization/threshold candidate retained with disclosed small-group cost; tests/source review pass | `5247ad1` |

E13's optional SQLx immutable cache-sharing change is not restored. Its
historical 7.3% gain required extra ownership/invalidation machinery and missed
the plan's preferred threshold; no fresh cache-sharing speedup is claimed.
E11 retains the smaller measured defaults-sharing change, without restoring the
historical conditional write-back mechanism. No new transparent core plan cache
has been added.

## Final validation

The final source passes 285 core library tests. The PostgreSQL grouping-key test
and both migration-transform differential tests pass, including hash-transition,
window-owner/volatility and bpchar-representative regressions.

A final combined SQLx validation run passed 39 tests:

| Suite | Passed | Ignored |
| --- | ---: | ---: |
| migration_chains | 12 | 0 |
| settings | 6 | 0 |
| runtime_expression_differential | 5 | 0 |
| prepared_ordering_differential | 1 | 1 |
| sqlx_driver | 15 | 0 |

The ignored test explicitly records the existing generic computed-projection
pagination error mismatch documented in [E13](e13_ordering.md); it is not claimed
fixed. Migration tests cover schema evolution, typed round trips, reversible
migrations, catalog semantics, trigger/reconciliation workflows, failed migration
rollback, repaired migration retries, and lock-timeout rollback.

## Final benchmark audit

The full Criterion sweep completed with all tiers and both backends:

```sh
cargo bench -p pg_fake_benchmarks --bench workloads -- \
  --warm-up-time 0.3 --measurement-time 1 --sample-size 20 \
  --save-baseline recovery_full_20260930
```

Individual groups that explicitly override sample count/measurement duration
keep those overrides. The suite retains its original shared long-session
fixture/history behavior; it is distinct from the fresh per-case experiment
probes. Both backends fetch and retain results through SQLx; the dedicated
native/adapter probes additionally describe their decoding behavior.
No compilation, tests, other benchmarks or profiling run concurrently.

The [complete table](recovery_full_benchmarks.md) accounts for 161 cases:
71 paired SQLx workloads and 19 unpaired/native diagnostics. pg_fake is faster
on 35/71 pairs, and 2/71 meet PostgreSQL/10 (create table and transactional DDL
create/rollback). The worst pair is nested filtered views: 393.170 versus
35.362 µs, 11.12× slower. UNION, UNION ALL, JSONB containment and derived/scalar
subqueries are 6.67×, 6.20×, 6.16× and 5.37× slower respectively.

The [separate filtered tier-1 sweep](recovery_tier1_benchmarks.md) completed:
8/10 faster, 0/10 at target. Both joins remain slower (2.48× and 2.37×).
Full and filtered runs have different accumulated history and remain separate.
These are current-source cross-backend comparisons, not causal source speedups.

The [13 fresh family profiles](recovery_profiles.md) identify remaining work.
Formatting and the release profile-example build pass. The recovery measurement
audit is finished; separate experiment commits are recorded below. The overall optimization
target is not achieved, and historical before/after claims for unchanged
experiments have not been independently re-established by this audit.

Independent final review verified all 161 full-sweep and 20 filtered tier-1
estimates and confidence intervals against saved Criterion JSON, plus ratios,
summary counts, profile totals and the profile harness. No substantive issues
remain. Subsequent explicit user authorization permitted the separate commits.

## Subsequent E8 simplification

After this full-sweep audit, a
[borrowed-query projection-name change](projection_names.md) measured the exact
nested-view workload at 384.681 → 366.427 µs (4.7%). This is a new paired
experiment; the full-sweep and 13-profile tables above describe the source
before it. The follow-up has fresh correctness validation and a separate
affected-tier rerun. Committed as `287ca65`. The original full-sweep
numbers must not be relabeled as measurements of this later source.
