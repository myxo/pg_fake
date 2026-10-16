# E08 — Replace pairwise duplicate and membership searches with SQL equality keys

Status: accepted. Parent revision: `1ebd3de4`.

## Hypothesis and predeclared gate

For types with a proven not-distinct hash key, DISTINCT and set operations can
retain first-seen row order while replacing pairwise row searches with a hash
membership or multiplicity lookup. Types without such a key keep the current
SQL comparison path. The same row-key construction must serve both consumers.

The co-primary operations are a complete prepared high-cardinality `SELECT
DISTINCT id` and table `UNION` query, each over 1,000 integer rows per source
table. Measure each also at 100 and 10,000 rows. For each 1,000-row operation,
three independently warmed alternating parent/candidate pairs must improve by
at least 10% and save at least 1.0 microsecond per complete query. Each pair
uses at least 100 Criterion samples, 3 seconds warmup, 5 seconds measurement,
and a 95% confidence interval excluding no improvement. Fixture setup is
outside steady-state timing; result disposal is inside. Record output
cardinality and raw samples.

Secondary cases include `INTERSECT`, `INTERSECT ALL`, `EXCEPT`, `EXCEPT ALL`,
ten-key and all-equal `DISTINCT`, empty/tiny inputs, `UNION ALL`, recursive
`UNION`, aggregate `DISTINCT`, and `DISTINCT ON`. Measure each operator
separately; no result from one operator stands in for another. Run all Tier 1,
directly affected Tier 2/3, native diagnostics, concurrency and full fake-only
catalog controls on both revisions before acceptance. Repeat suspected
slowdowns above 5% in three alternating pairs. Include cold prepare/execute
and memory observations.

The 10,000-row DISTINCT and UNION scaling diagnostics use a local 20-sample,
1-second warmup, 2-second measurement override because the parent has
quadratic work at that size. The 1,000-row acceptance cases retain the 100-sample
settings above. Other secondary cases run at 100 and 1,000 rows.

Correctness must compare rows, metadata and errors against PostgreSQL 18.
Cover NULLs, cross-type coercion, bpchar, numeric normalization, NaN and signed
zero, arrays, JSONB, collisions, first-row selection after ordering, and ALL
multiplicity. Unsupported key types must exercise the comparison fallback.
The hash table must never determine output visitation order.

## Measurements and decision

The candidate uses a shared row key for supported value types. The first 16
distinct outputs use SQL comparisons; once distinct cardinality grows beyond
that small-input region, a hash set retains first-seen rows. Rows containing a
type without a proven key continue through the comparison path. INTERSECT and
EXCEPT use the same keys with right-side multiplicity counts while visiting
left rows in their existing order. NULL occupies a distinct key slot.

An initial always-hash candidate cleared the two primary gates but slowed
100-row all-equal DISTINCT by 20.1%, ten-key DISTINCT by 15.5%, and DISTINCT ON
by 7.7% in a screening pair. The adaptive candidate brought those changes to
+0.8%, +0.5%, and −0.2%, respectively, in one follow-up pair. An intermediate
version also slowed 1,000-row numeric DISTINCT by 8.8% because it retained an
index mask in the comparison fallback. The final candidate retains selected
rows directly, and the numeric case changed by +2.3% in its first follow-up
pair. The complete repeatability and full-catalog results are recorded below.

The final candidate's three independently warmed primary pairs used alternating
parent/candidate order, 100 Criterion samples, 3 seconds warmup, and at least
5 seconds requested measurement. The fixture had 1,000 rows in each of two
integer tables. DISTINCT returned 1,000 rows and UNION returned 1,500. The
timer covered one complete prepared query and result disposal. Criterion mean
estimates, in microseconds:

| Pair order | DISTINCT parent → candidate | Saved | UNION parent → candidate | Saved |
| --- | ---: | ---: | ---: | ---: |
| parent / candidate | 1694.129 → 257.459 | 1436.670 (84.8%) | 4706.999 → 530.809 | 4176.190 (88.7%) |
| candidate / parent | 1687.171 → 257.745 | 1429.426 (84.7%) | 4685.650 → 527.162 | 4158.488 (88.7%) |
| parent / candidate | 1690.951 → 254.570 | 1436.382 (84.9%) | 4690.707 → 531.381 | 4159.326 (88.7%) |

The smallest conservative gain (parent 95% lower mean bound minus candidate
95% upper bound) was 1427.662 µs for DISTINCT and 4153.245 µs for UNION.
Every pair clears both practical thresholds and excludes no improvement.
These runs used the same locked dependencies, release profile, SQL and fixtures
on an Apple M2 running Darwin 24.6.0 with rustc 1.98.1. The focused binaries
were built with `python3 docs/optimization_research/2026-10-12/prepare.py
--e08 --sqlx` in each revision before timing. The six timed commands, in run
order, were:

```sh
while read -r revision baseline; do
  if [ "$revision" = parent ]; then binary=parent-final-focused-binary;
  else binary=candidate-v4-focused-binary; fi
  target/optimization-research/results/e08/$binary --bench --noplot \
    --sample-size 100 --warm-up-time 3 --measurement-time 5 \
    'equality_membership/(distinct_high|union)/1000$' \
    --save-baseline "$baseline"
done <<'RUNS'
parent e08-v4-p1-parent
candidate e08-v4-p1-candidate-v4
candidate e08-v4-p2-candidate-v4
parent e08-v4-p2-parent
parent e08-v4-p3-parent
candidate e08-v4-p3-candidate-v4
RUNS
```

The parent binary was built before engine edits; the final candidate binary is
archived as `candidate-v4-focused-binary`. Raw samples and build/timing logs
are retained under ignored `target/criterion/` and
`target/optimization-research/results/e08/`. The 10,000-row scaling diagnostics
used 20 samples with the local override: DISTINCT 184275.521 → 2604.413 µs
(98.6%), UNION 433429.135 → 5776.095 µs (98.7%). With preparation inside
the timer, the 1,000-row DISTINCT and UNION queries changed by −83.7% and
−88.3%, respectively. The added PostgreSQL 18 differential test passes,
including >16-key hash paths, numeric fallback, bpchar and JSONB. Two
256-case PostgreSQL property tests for set operations and general SQL pass.

The full fake-only suite completed on both revisions: 279 matched cases, one
benchmark process at a time, 100 samples, 3 seconds warmup and 5 seconds
requested measurement except documented local overrides. The full-suite
commands, run sequentially, were:

```sh
target/optimization-research/results/e08/parent-final-full-binary --bench --noplot --sample-size 100 --warm-up-time 3 --measurement-time 5 --save-baseline e08-full-parent
target/optimization-research/results/e08/candidate-final2-full-binary --bench --noplot --sample-size 100 --warm-up-time 3 --measurement-time 5 --save-baseline e08-full-candidate
```

The primary and directly affected full-suite means were:

| Query, rows | Parent → candidate (µs) | Saved | Faster |
| --- | ---: | ---: | ---: |
| DISTINCT, 1,000 | 1696.748 → 273.293 | 1423.455 | 83.9% |
| UNION, 1,000 per table | 4707.165 → 562.139 | 4145.026 | 88.1% |
| INTERSECT, 1,000 per table | 4878.574 → 700.592 | 4177.982 | 85.6% |
| INTERSECT ALL, 1,000 per table | 2000.593 → 493.902 | 1506.691 | 75.3% |
| EXCEPT, 1,000 per table | 4903.283 → 705.182 | 4198.101 | 85.6% |
| EXCEPT ALL, 1,000 per table | 2015.423 → 492.446 | 1522.976 | 75.6% |
| DISTINCT, cold prepare/execute | 1711.592 → 278.342 | 1433.250 | 83.7% |
| UNION, cold prepare/execute | 4785.133 → 596.482 | 4188.651 | 87.5% |
| DISTINCT, 10,000 | 183333.027 → 2614.475 | 180718.552 | 98.6% |
| UNION, 10,000 per table | 432604.967 → 5798.505 | 426806.462 | 98.7% |

Ten full-suite controls were more than 5% slower in the first parent/candidate
run. Three additional 13-case paired runs alternated candidate/parent,
parent/candidate, candidate/parent, using the same 100-sample settings. The
percentage is candidate latency above its paired parent:

| Control | Full run | Repeat 1 | Repeat 2 | Repeat 3 |
| --- | ---: | ---: | ---: | ---: |
| join profile, 1,000 | 37.1% | 1.4% | −0.6% | −0.6% |
| INSERT with defaults | 16.2% | −0.1% | 0.2% | −2.0% |
| adapter SELECT, SQLx | 10.0% | −2.2% | −2.1% | −1.8% |
| direct single-column lookup, 10,000 | 9.7% | 0.2% | 0.3% | −0.3% |
| many-match inner join | 9.4% | 1.3% | −0.6% | −1.3% |
| INSERT RETURNING | 9.3% | 0.2% | 0.1% | 0.1% |
| group-prefix lookup, 10,000 | 7.9% | −0.5% | −0.2% | −1.4% |
| adapter SELECT, core | 6.8% | −12.7% | −2.7% | −2.4% |
| sequential concurrent reads | 6.4% | 8.8% | 5.9% | 2.4% |
| snapshot, 1,000 | 5.3% | −1.2% | −5.4% | −0.6% |

The affected small-input controls remained below 5% in all three repeats:
ten-key DISTINCT at 100 and 1,000 rows and numeric DISTINCT at 100 rows.
Sequential concurrent reads had conflicting short repeats, so it was measured
again in three alternating pairs with 200 samples, 5 seconds warmup and 10
seconds requested measurement. A parallel-read case was measured alongside it:

| Pair order | Sequential parent → candidate (µs) | Change | Parallel change |
| --- | ---: | ---: | ---: |
| candidate / parent | 11.565 → 11.428 | −1.2% | −0.3% |
| parent / candidate | 11.399 → 11.612 | +1.9% | −1.2% |
| candidate / parent | 11.615 → 11.413 | −1.7% | −0.1% |

The longer runs do not reproduce a >5% slowdown. The PostgreSQL 18
differential test also passes after adding >16-key bpchar, mixed-integer and
DISTINCT ON cases.

Peak resident memory was measured with `/usr/bin/time -l` and a separate
fixed one-query probe using the same fixtures and prepared SQL. Criterion's
process-level RSS is confounded by its different iteration counts for a
183 ms parent query and a 2.6 ms candidate query, so the fixed probe is used
for the comparison:

| Fixture/query | Parent peak RSS | Candidate peak RSS |
| --- | ---: | ---: |
| 1,000 rows, setup only | 10.49 MB | 10.06 MB |
| 1,000 rows, one DISTINCT | 12.04 MB | 12.44 MB |
| 10,000 rows, setup only | 14.43 MB | 14.96 MB |
| 10,000 rows, one DISTINCT | 21.77 MB | 26.62 MB |
| 10,000 rows per table, one UNION | 22.40 MB | 25.77 MB |

The hash keys add transient memory proportional to distinct row count. The
set is scoped to one query and is dropped with the query's temporary rows;
there is no persistent cache. Cold prepare/execute and 10,000-row scaling
above show that work was not merely moved outside the steady-state timer.

Separate from all timing runs, the archived focused binaries were profiled for
9 seconds after fixture setup and Criterion warmup, with macOS `sample` attached
for 4 seconds at 1 ms intervals. The parent main-thread sample has 3,013
query-iteration stacks; 2,023 fall in the pairwise `remove_duplicate_rows` /
`compare_values` paths. The candidate has 2,515 query-iteration stacks; 1,014
fall in `retain_first_unique_rows` paths, with key construction visible beneath
them. The counts are within-process attribution, not comparable operation
counts because the candidate completes more queries during sampling. They
confirm that pairwise comparisons were replaced by key construction and hash
membership. The separate profile commands were:

```sh
target/optimization-research/results/e08/parent-final-focused-binary --bench --noplot --profile-time 9 '^equality_membership/distinct_high/1000$'
target/optimization-research/results/e08/candidate-v4-focused-binary --bench --noplot --profile-time 9 '^equality_membership/distinct_high/1000$'
```

For each process, run `sample PID 4 1 -mayDie -file REPORT` after its
`Profiling for` line appears. The reports and stdout logs are under ignored
`target/optimization-research/results/e08/profile-*.sample.txt` and
`profile-*.log`. The forced-collision unit test uses one constant hash for
different keys and verifies both distinct membership and multiplicity counts.
The PostgreSQL differential test now also compares prepared column names, type
names, and OIDs for every successful E08 query, including the empty result.
`cargo fmt --all -- --check`, `git diff --check`, workspace Clippy and
`cargo test --workspace` pass on the final code and expanded tests. The
workspace suite was rerun with local PostgreSQL connection access after its
first sandboxed run was denied socket access. The existing Clippy warnings
are recorded in the ignored local log. Final subagent review found no engine
correctness defect or repeatable performance regression. E08 clears the
predeclared primary and control gates and is accepted.
