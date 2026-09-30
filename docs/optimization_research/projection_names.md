# Borrow queries while restoring projection names — 2026-09-30

Follow-up to E8's remaining nested-view copying cost. The output-name helper
accepted a statement, so every generic query execution cloned its complete AST
into a temporary boxed statement before reading its projection. It now borrows
the existing query. Preparation performs the same query-statement check at its
caller. Naming rules, execution paths and SQL support are unchanged.

This is a small shared simplification, not predicate pushdown, another prepared
plan or a cache. Independent source/harness review found no correctness issue.
The candidate is retained; correctness validation and tier benchmarks pass.
Independent final evidence review is clean, including all 70 tier estimates,
confidence intervals, ratios, counts, test totals and profile samples.

## Measurement scope

The fresh `view_projection_probe` runs 10/100/1,000-row equivalent fixtures
through native unprepared, native prepared and SQLx cached-query APIs.
Each has two nested projection/filter views and an outer equality returning
one row. Setup, preparation and warmup are excluded from 1,000 timed executions;
complete results are black-boxed and dropped, without additional SQLx typed
decoding. SQLx gets a separate equivalent database. Three independent process
runs are retained per source version. Preparation is printed separately.

The exact registered `nested_filtered_view_100_rows` workload additionally
compares the original long-name fixture through both SQLx backends. Its fresh
filtered history differs from the recovery full sweep, so only its paired
before/after run is used for a source-change claim.

Reproduction:

```sh
cargo build -p pg_fake_sqlx --release --example view_projection_probe
for run in 1 2 3; do target/release/examples/view_projection_probe; done
cargo bench -p pg_fake_benchmarks --bench workloads -- nested_filtered_view_100_rows \
  --warm-up-time 0.3 --measurement-time 1 --sample-size 20 \
  --save-baseline projection_before
```

Repeat on the candidate with baseline `projection_after`. No compilation,
tests, other benchmarks or sampling overlap the timed measurements. One failed
candidate compile overlapped the baseline runner's remaining filtered-fixture
cleanup/setup after both measured cases had finished; it did not overlap their
measurement or analysis. Environment is the same as the recovery audit.

Logs: `/tmp/projection-{before,after}.log` and
`/tmp/projection-criterion-{before,after}.log`. Criterion means/CIs:
`target/criterion/**/projection_{before,after}/estimates.json`.
Pre-candidate source snapshots for a separate commit:
`/tmp/projection-{query,projection,session}-before.rs`.

## Three-run probe results

Microseconds per execution; reduction is 1 − after / before. Negative means
slower. The 1,000-row unprepared result includes a 1,338.360 µs after run;
the other two after runs were 1,244.991 and 1,251.313 µs. It is retained in
the mean, not discarded.

| Case | Before runs | After runs | Before mean | After mean | Reduction |
| --- | --- | --- | ---: | ---: | ---: |
| rows=10 prepared=false | 349.195, 331.568, 333.438 | 335.925, 318.560, 319.272 | 338.067 | 324.586 | 4.0% |
| rows=10 prepared=true | 287.297, 289.763, 284.340 | 273.313, 271.897, 272.406 | 287.133 | 272.539 | 5.1% |
| rows=10 sqlx | 314.133, 312.932, 315.248 | 298.588, 302.290, 301.473 | 314.104 | 300.784 | 4.2% |
| rows=100 prepared=false | 418.731, 412.331, 412.759 | 402.805, 399.951, 403.813 | 414.607 | 402.190 | 3.0% |
| rows=100 prepared=true | 376.219, 369.614, 367.061 | 359.280, 354.977, 361.719 | 370.965 | 358.659 | 3.3% |
| rows=100 sqlx | 399.287, 394.346, 397.855 | 384.439, 379.704, 384.460 | 397.163 | 382.868 | 3.6% |
| rows=1000 prepared=false | 1269.827, 1250.161, 1237.250 | 1244.991, 1338.360, 1251.313 | 1252.413 | 1278.221 | -2.1% |
| rows=1000 prepared=true | 1223.305, 1196.155, 1202.348 | 1213.008, 1188.097, 1205.898 | 1207.269 | 1202.334 | 0.4% |
| rows=1000 sqlx | 1240.718, 1222.078, 1232.638 | 1229.927, 1210.703, 1232.657 | 1231.811 | 1224.429 | 0.6% |

## Exact SQLx workload

Criterion means and 95% confidence intervals, microseconds:

| Backend | Before mean [95% CI] | After mean [95% CI] |
| --- | ---: | ---: |
| pg_fake | 384.681 [384.017, 385.513] | 366.427 [365.964, 366.890] |
| postgres_18 | 35.822 [35.684, 35.973] | 35.711 [35.641, 35.789] |

The pg_fake mean is 4.7% lower, saving 18.254 µs.
Retain the change under the plan's smaller-gain exception: it removes an
unnecessary full-query clone and temporary statement wrapper with no new cache,
algorithm or specialized execution path. This does not fix the nested-view
performance gap: pg_fake still takes 10.26× PostgreSQL time.
Do not claim a universal gain; the 1,000-row probe is effectively small/noisy
and its unprepared mean is 2.1% higher.

## Validation

285 core library tests, four prepared-parameter tests and all 21 view tests pass.
SQLx runtime differential (5), ordering differential (1) and driver (15) tests
pass; the same existing generic projection/pagination error mismatch remains
explicitly ignored. Formatting passes. Logs:
`/tmp/projection-core-tests.log`, `/tmp/projection-sqlx-tests.log`.

The affected tier 3 plus tier 1 rerun uses `projection_tiers13` and command
`cargo bench -p pg_fake_benchmarks --bench workloads -- 'tier[13]_'
--warm-up-time 0.3 --measurement-time 1 --sample-size 20
--save-baseline projection_tiers13`. The [complete results](projection_tiers13.md) account for 70 estimates:
31 paired SQLx cases and eight unpaired diagnostics. Tier 1 is faster on 8/10,
tier 3 on 8/21; neither tier has a PostgreSQL/10 result. Different accumulated
history makes comparisons against the earlier full/tier-1-only sweeps unsuitable
for causal speedup claims.


## Refreshed profile

Rebuilt the release `workload_profile` example on the retained source and
sampled its prepared native `view` case for two seconds at a requested 1 ms
interval, after one second of warmup. No compilation, tests or benchmarks ran
during sampling. The 1,421 main-thread samples still show copying/recursion/
allocation costs: memmove 188 self samples, remaining_stack 138, nanov2_free 56,
malloc_zone_malloc 43. These counts are not before/after latency estimates.
This small removal does not address the other AST copies and visitors.
Files: `/tmp/projection-view-{run.log,profile.txt,sample.log}`.
