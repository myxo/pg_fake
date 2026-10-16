# E06 — Compute structural expression properties once

Status: rejected. Parent revision: `561ac28d`.

## Hypothesis and predeclared gate

Repeated visits of unchanged expression trees to discover subqueries and
advisory calls account for a material part of VALUES UNION ALL and pattern
query latency. Compute properties in one traversal at statement execution
scope, then reuse them for stable expression occurrences. Expressions created
by view expansion or rewriting require their own analysis or the existing
safe traversal. Preserve stack-depth checks in the actual evaluator.

Primary workloads are the existing native VALUES UNION ALL and pattern
queries at 100 and 1,000 rows, one complete query per operation. Predeclare
the 1,000-row VALUES UNION ALL and 1,000-row pattern cases as co-primary:
each must improve by at least 10% and 1.0 microsecond in three independently
warmed alternating parent/candidate pairs, with separate 95% mean intervals
excluding no improvement. Use at least 100 samples, 3 seconds warmup, and
5 seconds requested measurement, with the same SQL, fixture, parameters, and
timer boundaries on both revisions. Record cardinality and node-walk counts.

Controls include cold prepare and execution, 100-row and tiny queries, indexed
reads, DML, scalar and correlated subqueries, nested views, recursive CTEs,
deep/wide expression trees, advisory waits, volatility, and cancellation.
Run the original full fake-only benchmark catalog plus directly affected
native and SQLx controls on both revisions. Repeat suspected regressions above
5% in three pairs. Reject if another supported workload is repeatably slower
by more than 5%, correctness diverges from PostgreSQL 18, or memory grows
without bound. Preserve lazy CASE errors and exactly-once volatile effects
through row-lock waits and resumed statements.

## Measurements and decision

The screening run used Apple M2, arm64, 16 GiB RAM, Darwin 24.6.0, and
rustc 1.98.1. All binaries used the same benchmark source, dependencies,
fixtures, SQL and Cargo release build (`cargo build --offline --release`, no
custom runner `RUSTFLAGS`). Criterion timed a complete prepared query, including
result disposal, after fixture setup. VALUES UNION ALL returned 200 or 2,000
rows; the pattern query returned 100 or 1,000 rows. Each run used 100 samples,
3 s warmup and at least 5 s requested measurement. Mean estimates and 95%
intervals, in microseconds:

The build and timing commands were:

```sh
python3 docs/optimization_research/2026-10-12/prepare.py --full-fake --sqlx
cp target/release/research-workloads target/optimization-research/results/e06/<revision>-binary
target/optimization-research/results/e06/<revision>-binary --bench --noplot \
  --sample-size 100 --warm-up-time 3 --measurement-time 5 \
  'expression_properties/' --save-baseline e06-<revision>-preflight
```

`<revision>` was `parent` or `candidate-v3` for the table below. The earlier
candidate used `candidate-v1`; the view-discovery probe used `candidate-v4`
with the filter `expression_properties/patterns/1000`. Build and benchmark
stdout/stderr were redirected into matching files under the E06 results
directory. The archived parent binary came from `561ac28d` with the new
benchmark source; candidate engine edits were removed after rejection.

| Case | Parent | Candidate with prepared structural properties | Gain |
| --- | ---: | ---: | ---: |
| VALUES UNION ALL, 100 | 403.422 [403.056, 403.787] | 209.692 [208.977, 210.570] | 193.730 (48.0%) |
| VALUES UNION ALL, 1,000 | 3952.051 [3949.485, 3954.689] | 2066.733 [2060.166, 2074.055] | 1885.318 (47.7%) |
| Patterns, 100 | 210.008 [209.638, 210.494] | 204.565 [204.279, 204.890] | 5.443 (2.6%) |
| Patterns, 1,000 | 1572.489 [1570.687, 1574.254] | 1569.645 [1567.049, 1572.371] | 2.844 (0.2%) |

An earlier execution-scoped candidate measured 381.858 / 3890.359 µs for
VALUES and 209.475 / 1577.649 µs for patterns at 100 / 1,000 rows. A final
probe that skipped view discovery when the prepared statement's catalog
identity still matched measured 1583.335 [1581.817, 1584.898] µs for the
1,000-row pattern case, slower than the parent. The parent and candidate
screening runs were sequential, not three alternating pairs. That is sufficient
to reject this candidate because the 1,000-row pattern result was about 150 µs
short of the 10% threshold in every formulation; it is not acceptance evidence.

The candidate built one structural property map per prepared statement and
used it for subquery, advisory, volatile, CTE, locking and view-discovery
decisions, with traversal fallback for rewritten expressions. The VALUES gain
shows that repeated whole-query analysis was costly. The pattern workload's
dominant remaining expression work needs a separate design; broadening this
experiment into typed expression evaluation would overlap E11. The candidate
did not advance to full-suite, memory or PostgreSQL differential gates because
its co-primary performance gate failed. Node-walk counters were not collected
because no candidate survived that screening gate. Raw Criterion samples,
build logs and binary archives are retained in ignored
`target/optimization-research/results/e06/` and `target/criterion/`.

Decision: **rejected**. The engine changes are removed. The two benchmark
cases remain as controls for future expression experiments.
