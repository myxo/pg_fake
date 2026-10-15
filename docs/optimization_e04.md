# E04 — Bounded composite-index equality prefixes

Status: accepted. Parent revision: `680f49a`.

## Hypothesis and measurement plan

An equality on the leading columns of an unfiltered composite index currently
falls back to a table scan unless another complete index matches. The ordered
`BTreeMap<IndexKey, BTreeSet<RowId>>` can seek to the normalized prefix and
enumerate only adjacent keys with that prefix. The visible row must still be
rechecked against the full predicate and its current index key. Row IDs from
historical keys must be deduplicated before visibility checks.

Add a multirow prefix mode to E03's equality-access descriptor. The primary
read workload selects by the leading key of an unfiltered two-column index,
with a fixed 10 matches as unrelated rows grow through 100, 1,000, and 10,000.
Measure both `(account_id, group_id)` and `(group_id, account_id)` index
orders, plus a membership-style join chain. Vary the matching prefix to 10,
100, and 1,000 rows and include a missing prefix. Compare a direct
single-column index, a complete-key E03 lookup, the handoff's partial-only
index fallback, and full scans. Return cardinality and fixture size are
checked before timing.

Use the isolated parent archive and the same fake-only benchmark source in
both revisions. Benchmark only pg_fake, with three independently warmed
P/C, C/P, P/C pairs at the 1,000-row primary shape, 100 Criterion samples,
3 seconds warmup, and 5 seconds measurement. Require a 95% interval excluding
no improvement and at least 10% plus 1 µs saved in every pair. Run all Tier 1
controls, affected Tier 2/3 cases, and the full fake-only catalog before
acceptance. Repeat any apparent >5% slowdown in three pairs. PostgreSQL 18 is
used only for differential correctness tests, never benchmarks.

Correctness checks cover prefix NULLs, multiple matches, stale entries after
key changes, old snapshots, rollback, ordering, missing prefixes, nonunique
and unique composite indexes, and serializable phantom tracking. The index
must remain one physical map: include write, rollback, and memory controls.

## Results

The parent is an isolated archive of `680f49a` with identical E04 benchmark
source and a fake-only entry point. Both full runners list the same 186 cases.
They use release builds on Apple M2, macOS 15.8.1, with `rustc 1.98.1`.
Preparation, fixture insertion, and result-cardinality checks are outside the
timer; each primary prefix read returns 10 rows. Keys rotate across prepared
executions. The three primary P/C, C/P, P/C pairs used 100 samples, 3 seconds
warmup, and 5 seconds measurement, with fresh Criterion baselines:

The acceptance runs used `python3 target/optimization-research/results/e04/run_primary_final.py`,
`python3 target/optimization-research/results/e04/run_full.py`, and
`python3 target/optimization-research/results/e04/repeat_final_suspects.py`.
The primary runner invoked each release binary with `--bench --noplot
--sample-size 100 --warm-up-time 3 --measurement-time 5
'^composite_prefix/(leading_account|leading_group|membership_chain)/1000$'
--save-baseline <pair-and-revision>`; the full and suspect runners used the
same settings with their recorded filters. All commands and logs remain in the
ignored E04 results directory.

| Case, 1,000 rows | Pair | Parent mean (µs) | Candidate mean (µs) | Saved (µs) | Faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| Leading account | 1 | 32.825 | 3.589 | 29.236 | 89.07% |
| Leading account | 2 | 32.950 | 3.599 | 29.351 | 89.08% |
| Leading account | 3 | 33.115 | 3.559 | 29.556 | 89.25% |
| Leading group | 1 | 32.984 | 3.571 | 29.413 | 89.17% |
| Leading group | 2 | 33.087 | 3.606 | 29.481 | 89.10% |
| Leading group | 3 | 33.030 | 3.570 | 29.460 | 89.19% |
| Membership chain | 1 | 670.880 | 9.357 | 661.523 | 98.61% |
| Membership chain | 2 | 669.540 | 9.402 | 660.138 | 98.60% |
| Membership chain | 3 | 673.700 | 9.512 | 664.188 | 98.59% |

The 95% mean confidence intervals are separated in every pair, and each
clears both practical thresholds. A 20-sample screening pass showed the
10-match leading-account case scaling from 5.281 to 307.246 µs on the parent
across 100 to 10,000 rows, versus 3.849 to 4.010 µs on the candidate. The
10,000-row membership chain changed from 6,210.669 to 10.280 µs. The direct
single-column control changed from 3.704 to 3.692 µs at 10,000 rows, and
the 1,000-row complete-key control from 1.339 to 1.345 µs. These short runs
show scaling and controls. Diagnostic counters in a temporary build showed a
10-match query over 10,000 rows visiting 11 adjacent index keys and collecting
10 RowIds; the counters and diagnostic test were removed afterward.

Four native prefix tests pass for unique and nonunique indexes, two-column
prefixes of a three-column index, missing/NULL keys, changed keys and old
snapshots, row deduplication, rollback, and serializable phantom tracking.
A focused PostgreSQL 18 differential test passes for rows, empty results,
parameters, old snapshots, changed keys, and rollback. PostgreSQL was used
only for correctness testing, never timing. Both revisions completed all 186
fake-only benchmark cases. Fourteen cases exceeded 5% against the older parent
full-suite baseline; each was rerun in three alternating parent/candidate
pairs. None exceeded 5% in all three pairs. The initially repeatable join and
heap-scan suspects were also cleared in separate three-pair checks. The full
logs, raw samples, scripts, and Criterion estimates are preserved under
`target/optimization-research/results/e04/` and `target/criterion/`.

`cargo test --workspace --offline`, `cargo fmt --all -- --check`,
`cargo check --workspace --offline`, and
`cargo clippy --workspace --all-targets --offline` passed. Clippy emitted
existing unrelated warnings.

The resource check used the final parent and candidate fake-only binaries on
the 10,000-row leading-account case, with 10 samples, 1 second warmup and
1 second measurement. Fixture setup was included in process memory. Parent
process peak RSS was 82.1 MB; candidate peak RSS was 76.7 MB. CPU user time
was 7.89 seconds for the parent and 8.41 seconds for the candidate across the
whole process, including fixture creation and Criterion calibration; those
totals are not per-query latency measurements. A separate 20-sample,
3-second warmup and 5-second measurement run reported 15.68 and 14.34 user
seconds respectively; fixed benchmark duration and setup dominate these
totals. `samply record --save-only` could not collect stacks in this sandbox
(`Unknown(1100)`), so the work-count diagnostic and process resource figures
provide the available CPU and memory evidence. No CPU profiler or compiler ran
alongside the acceptance timings.

The bounded index walk, primary gains, regression pairs, correctness tests,
and resource results satisfy the acceptance gate. The code retains one index
map per index; NULL suffixes enter that map under their non-NULL prefix while
full-key uniqueness checks continue to ignore them.
