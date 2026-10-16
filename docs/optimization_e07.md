# E07 — Borrow source rows until ownership is required

Status: rejected. Parent revision: `bcd2ddf8`.

## Hypothesis and predeclared gate

The table-factor scan copies every visible row into a scratch vector even when
the source row already fills the bound scope. Passing that immutable stored row
directly to the existing query consumer should avoid cloning unused wide values
in grouping and filtered scans. A consumer that retains a row must still copy
it before the callback returns. Multi-source rows, lock waits, mutations, and
other layouts that require owned slots keep their current ownership boundary.

The co-primary operations are complete prepared grouped queries and filtered
scans over 1,000 wide rows with large unused text and JSONB payloads. Measure
the same SQL on narrow and wide fixtures at 100 and 1,000 rows; a 100-row query
is one operation. Each 1,000-row co-primary case must improve by at least 10%
and 1.0 microsecond in three independently warmed alternating parent/candidate
pairs, with separate 95% confidence intervals excluding no improvement. Use
at least 100 Criterion samples, 3 seconds warmup, and 5 seconds measurement.
Fixture setup is outside steady-state timing; result disposal is inside.

Controls include scans and projections returning the payload, indexed reads,
small inputs, joins, all aggregate forms, windows, CTEs, cold prepare/execute,
and every Tier 1 and directly affected Tier 2/3 benchmark. Run the full fake-only
catalog on both revisions before acceptance. Repeat suspected regressions above
5% in three paired runs. Compare actual rows, metadata and errors with PostgreSQL
18 for representative queries, including empty results and NULL payloads. Check
row provenance, SSI reads, old snapshots, joined slots, group representatives,
projection order, waits, and mutations. Inspect memory and allocation volume;
do not accept a design that merely defers copying all values or grows retained
memory without bound.

## Measurements and decision

The candidate passed the stored row directly to the query consumer when its
length matched the entire bound scope. CTE rows used the same rule. Rows with
joined slots or older, shorter versions continued through the scratch vector.
The change compiled with `cargo check --workspace --all-targets --offline` and
passed `cargo fmt --all -- --check` before timing.

The screening run used an Apple M2, arm64, Darwin 24.6.0, and rustc 1.98.1.
Both parent and candidate were built in release mode from the same dependencies
and benchmark source. The isolated fake-only runner measured complete prepared
queries, including result disposal, after fixture setup. The 1,000-row grouped
query returned 10 rows; the filtered query returned 100. Each process used 100
samples, 3 seconds warmup, and at least 5 seconds requested measurement. The
Criterion **mean** estimates and 95% intervals, in microseconds, were:

| Primary operation | Parent | Candidate | Saved |
| --- | ---: | ---: | ---: |
| Grouped, 1,000 wide rows | 1201.468 [1194.844, 1207.559] | 850.622 [846.273, 854.927] | 350.846 (29.2%) |
| Filtered, 1,000 wide rows | 42.034 [41.906, 42.191] | 41.236 [41.163, 41.313] | 0.798 (1.9%) |

The timing commands were:

```sh
python3 docs/optimization_research/2026-10-12/prepare.py --e07 --sqlx
target/optimization-research/results/e07/<revision>-focused-binary --bench --noplot \
  --sample-size 100 --warm-up-time 3 --measurement-time 5 \
  'borrowed_source_rows/(grouped|filtered)/1000/wide' \
  --save-baseline e07-<revision>-screen
```

`<revision>` was `parent` or `candidate`. The parent source was an archive of
`bcd2ddf8` with identical benchmark source copied in. Both runner binaries and
Criterion raw samples are retained in ignored `target/optimization-research/`
and `target/criterion/`. The two screening runs were sequential, not three
alternating pairs. The filtered primary missed both practical thresholds by a
wide margin, so repeating the pairs, profiling, and running the full catalog
could not justify accepting this candidate. No PostgreSQL 18 differential test
or memory gate was claimed for the rejected implementation.

Decision: **rejected**. All E07 engine changes were removed. The new benchmark
cases remain as controls for later work; they include narrow and wide fixtures
at 100 and 1,000 rows plus payload-returning projections.
