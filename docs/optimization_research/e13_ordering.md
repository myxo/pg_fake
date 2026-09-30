# E13: prepared ordering and ordered pages

Baseline is `c5adffb` plus the separately measured E11 and E7 changes.
The baseline source snapshots for the four modified files are saved locally
as `/tmp/e13-{prepared,query-mod,ordering,select}-before.rs`. The retained
experiment is committed as `971f0cd`.

Both variants use `cargo build -p pg_fake -p pg_fake_sqlx --release --examples`,
macOS arm64, Cargo 1.98.1, default features, no execution tracing. Run each of
these executables three times with no compilation or other benchmark running:

- `target/release/examples/ordering_probe`: native autocommit, 10/100/1,000
  rows, three projected columns, descending price/ascending unique ID; 2,000
  iterations each for one-shot and prepared reuse. Page is LIMIT 5 OFFSET 3.
- `target/release/examples/ordering_wide_probe`: native prepared pages, 1,000
  or 10,000 rows, two projected columns and zero/eight unselected 256-byte text
  columns; 100 iterations after a warm execution.
- `target/release/examples/ordering_adapter_probe`: SQLx persistent caching,
  parameterized predicate over 100 rows with 50 matches, ordered read/page,
  5,000 iterations after warmup. All three output columns are decoded.

Each case owns a fresh database; fixture setup is outside timing. Prepare and
first execution are separate diagnostics printed by the native probe. These
are same-API A/B measurements, not comparisons with historical PostgreSQL
latencies.

Implementation reuses generic order resolution/comparison for keys naming
projected direct columns. Numeric positions and normalized aliases are supported.
Other ordering expressions, dynamic row counts, LIMIT 0, unordered limits,
and computed projections with finite limits/nonzero offsets retain fallback.
Pages reuse the existing bounded heap, retaining at most OFFSET + LIMIT rows,
with projection before dropping OFFSET rows to preserve existing evaluation
behavior. The heap now accepts a row comparator so both generic SelectRow and
prepared source rows share the same algorithm.

## Baseline timings

All values are microseconds per operation.

| Case | Three runs | Mean |
| --- | --- | ---: |
| rows=10 page=false prepared=false | 33.266, 24.398, 24.197 | 27.287 |
| rows=10 page=false prepared=true | 15.473, 15.209, 15.091 | 15.258 |
| rows=10 page=true prepared=false | 27.093, 27.347, 27.263 | 27.234 |
| rows=10 page=true prepared=true | 16.603, 16.778, 16.669 | 16.683 |
| rows=100 page=false prepared=false | 62.990, 63.164, 63.011 | 63.055 |
| rows=100 page=false prepared=true | 53.466, 53.835, 53.765 | 53.689 |
| rows=100 page=true prepared=false | 52.907, 52.973, 52.835 | 52.905 |
| rows=100 page=true prepared=true | 42.069, 42.339, 42.038 | 42.149 |
| rows=1000 page=false prepared=false | 466.468, 474.193, 469.771 | 470.144 |
| rows=1000 page=false prepared=true | 457.085, 458.592, 462.244 | 459.307 |
| rows=1000 page=true prepared=false | 293.101, 294.057, 298.181 | 295.113 |
| rows=1000 page=true prepared=true | 281.007, 280.735, 279.701 | 280.481 |
| rows=1000 extra_columns=0 | 227.016, 229.425, 228.079 | 228.173 |
| rows=1000 extra_columns=8 | 636.387, 647.245, 653.475 | 645.702 |
| rows=10000 extra_columns=0 | 2128.472, 2144.480, 2136.750 | 2136.567 |
| rows=10000 extra_columns=8 | 7548.924, 7509.040, 7474.975 | 7510.980 |
| SQLx rows=100 page=false persistent=true | 145.149, 143.144, 143.456 | 143.916 |
| SQLx rows=100 page=true persistent=true | 136.510, 137.078, 137.531 | 137.040 |

## Validation and known baseline mismatch

284 core tests, 21 view tests, five SQLx runtime differential tests, 15 SQLx
driver tests, and the new direct-column ordering/page PostgreSQL test pass.
Independent review found no remaining issue after fixing alias normalization,
raw-placeholder fallback, and projection/offset ordering. Formatting passes.

The following existing generic behavior does not match PostgreSQL and is not
claimed fixed by E13:

```sql
CREATE TABLE ordered_pages (id integer PRIMARY KEY, price integer);
INSERT INTO ordered_pages VALUES (1,20), (2,NULL), (3,10), (4,20);
SELECT id, price, 100 / (id - 1)
FROM ordered_pages
ORDER BY price ASC NULLS FIRST, id DESC
LIMIT (2) OFFSET 1;
```

PostgreSQL raises `22012`; pg_fake's generic executor returns `(3,10,50)` and
`(4,20,33)`. Parenthesized LIMIT forces the generic path in this experiment.
The exact reproducer is retained as the explicitly ignored test
`compare_generic_projection_error_before_ordered_limit` in
`crates/pg_fake_sqlx/tests/prepared_ordering_differential.rs`. An explicit run
with `-- --ignored` reproduced the mismatch on 2026-09-30. Computed pages are
excluded from this optimization; generic-equivalence is not PostgreSQL parity.

## Rejected first candidate

Retaining all matching source rows improved small fixtures but regressed the
10,000-row, eight-extra-column page: 9,143.234 / 8,929.947 / 8,909.921 µs
(mean 8,994.367 µs) versus the 7,510.980 µs baseline, 19.7% slower. This version
was rejected. The bounded-heap refinement passed independent review and the
core/view tests. The measurements below accept the bounded refinement.

## Accepted bounded-heap timings

Three sequential release runs; microseconds per operation.

| Case | Three runs | Mean | Reduction |
| --- | --- | ---: | ---: |
| rows=10 page=false prepared=false | 20.690, 12.813, 12.665 | 15.389 | 43.6% |
| rows=10 page=false prepared=true | 3.748, 3.001, 3.041 | 3.263 | 78.6% |
| rows=10 page=true prepared=false | 14.894, 13.845, 14.031 | 14.257 | 47.7% |
| rows=10 page=true prepared=true | 2.710, 2.736, 2.762 | 2.736 | 83.6% |
| rows=100 page=false prepared=false | 31.798, 32.141, 32.345 | 32.095 | 49.1% |
| rows=100 page=false prepared=true | 21.469, 21.753, 22.144 | 21.789 | 59.4% |
| rows=100 page=true prepared=false | 20.916, 21.229, 21.130 | 21.092 | 60.1% |
| rows=100 page=true prepared=true | 9.643, 9.762, 9.775 | 9.727 | 76.9% |
| rows=1000 page=false prepared=false | 243.783, 238.014, 239.369 | 240.389 | 48.9% |
| rows=1000 page=false prepared=true | 228.023, 227.684, 228.169 | 227.959 | 50.4% |
| rows=1000 page=true prepared=false | 79.239, 80.348, 80.245 | 79.944 | 72.9% |
| rows=1000 page=true prepared=true | 67.796, 68.800, 68.737 | 68.444 | 75.6% |
| rows=1000 extra_columns=0 | 48.935, 46.442, 46.191 | 47.189 | 79.3% |
| rows=1000 extra_columns=8 | 298.453, 275.033, 270.697 | 281.394 | 56.4% |
| rows=10000 extra_columns=0 | 475.405, 433.238, 434.321 | 447.655 | 79.0% |
| rows=10000 extra_columns=8 | 4080.550, 3926.477, 4135.779 | 4047.602 | 46.1% |
| SQLx rows=100 page=false persistent=true | 36.450, 36.680, 36.713 | 36.614 | 74.6% |
| SQLx rows=100 page=true persistent=true | 25.025, 24.982, 25.022 | 25.010 | 81.8% |

All measured cases improve, including the previously regressing wide fixture.
The retained implementation is reviewed, measured and committed as `971f0cd`.

## Prepare and first-execution diagnostics

Means of three single observations, in microseconds. These are noisy cold-path
diagnostics, not stable speedup estimates; fixture initialization precedes each
observation. All individual observations are shown to expose outliers.

| Case | Baseline prepare / first runs | Bounded prepare / first runs |
| --- | --- | --- |
| rows=10 page=false | 1398.459 / 933.667; 103.167 / 66.459; 103.875 / 66.625 (means 535.167 / 355.584) | 1560.750 / 193.958; 108.250 / 13.792; 106.792 / 13.625 (means 591.931 / 73.792) |
| rows=10 page=true | 115.083 / 62.667; 40.875 / 25.333; 41.625 / 24.375 (means 65.861 / 37.458) | 92.333 / 4.791; 42.583 / 4.250; 44.084 / 4.666 (means 59.667 / 4.569) |
| rows=100 page=false | 29.583 / 96.458; 25.791 / 58.500; 25.542 / 58.791 (means 26.972 / 71.250) | 27.708 / 60.875; 25.792 / 27.292; 41.375 / 27.875 (means 31.625 / 38.681) |
| rows=100 page=true | 35.000 / 55.375; 29.958 / 44.667; 34.791 / 46.500 (means 33.250 / 48.847) | 27.875 / 11.167; 30.292 / 11.333; 74.417 / 14.125 (means 44.195 / 12.208) |
| rows=1000 page=false | 68.875 / 472.333; 40.416 / 469.000; 34.834 / 460.709 (means 48.042 / 467.347) | 42.375 / 215.667; 50.333 / 210.834; 75.083 / 221.625 (means 55.930 / 216.042) |
| rows=1000 page=true | 71.334 / 285.958; 60.875 / 291.708; 54.750 / 283.208 (means 62.320 / 286.958) | 70.791 / 72.042; 47.750 / 71.375; 74.833 / 71.000 (means 64.458 / 71.472) |
