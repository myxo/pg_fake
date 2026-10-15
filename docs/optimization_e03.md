# E03 — Complete composite equality keys

Status: accepted. Parent revision: `fd97842`.

## Hypothesis and preregistered measurements

The executor recognizes equality constraints on individual columns, but does not
combine complete keys in index-column order. A prepared lookup on a table with
only an unfiltered composite index therefore scans unrelated rows. Replacing
the single-column access representation with ordered key vectors should let the
same storage lookup serve reads, parameterized joins, and mutation candidates.
The residual predicate and MVCC visibility check remain authoritative.

The primary workload is a prepared, one-row lookup using both columns of an
unfiltered unique `(group_id, account_id)` index at 100, 1,000, and 10,000 rows.
Keys rotate between operations. A prepared, one-row parameterized join from a
primary-key selected parent into that same complete composite key is a second
primary shape at the same sizes. The timer includes execution and result drop;
fixture insertion and preparation are outside it. Measure a cold prepare plus
execute control separately.

Controls include the handoff-shaped complete-key query with its original partial
index, that query with an added diagnostic single-column index, existing
single-column read/write cases, composite nonunique duplicates, an absent key,
and complete-key UPDATE/DELETE with rollback. The full fake-only benchmark
catalog, including native diagnostics and concurrency, is the regression gate.
The parent and candidate use identical benchmark source, fixtures, parameters,
build profile, and toolchain. Run three independently warmed paired Criterion
measurements at each 1,000-row primary shape, alternating P/C, C/P, P/C, with
100 samples, 3 seconds warmup, and 5 seconds measurement. Record mean latency,
95% intervals, raw samples, absolute and percentage change. Require at least
10% and 1 µs improvement in each pair and no repeatable control slowdown above
5%.

Correctness checks cover reversed conjuncts and index order, mixed types, NULL,
casts, nonunique duplicates, key changes, old snapshots, unique-prefix rejection,
join multiplicity, serializable phantom tracking, and mutation rollback. Use
PostgreSQL 18 only for differential correctness checks, never for benchmarks.

## Results

The parent source is an isolated archive of `fd97842`, with the exact same
new benchmark source and fake-only entry as the candidate. The source hashes
matched before timing. Both use release builds with `rustc 1.98.1` on macOS
15.8.1. The primary timer covers one prepared query and result handling;
fixture insertion and preparation are outside it. Each case returns one row.
Criterion used 100 samples, 3 seconds warmup, and 5 seconds measurement.
All twelve runs used fresh baseline names and ran in P/C, C/P, P/C pair order.
These measurements are from the initial candidate before its regression fix;
the final candidate must pass the same gates again.

| Case, 1,000 rows | Pair | Parent mean (µs) | Candidate mean (µs) | Saved (µs) | Faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| Complete unique lookup | 1 | 39.8925 | 1.4010 | 38.4915 | 96.488% |
| Complete unique lookup | 2 | 40.0953 | 1.4057 | 38.6896 | 96.494% |
| Complete unique lookup | 3 | 40.1595 | 1.4046 | 38.7549 | 96.502% |
| Parameterized join | 1 | 1542.9817 | 1.8232 | 1541.1585 | 99.882% |
| Parameterized join | 2 | 1538.0461 | 1.8092 | 1536.2369 | 99.882% |
| Parameterized join | 3 | 1548.3417 | 1.8097 | 1546.5320 | 99.883% |

All 95% mean confidence intervals are separated. For lookup, the smallest
parent lower bound is 39.8700 µs and the largest candidate upper bound is
1.4084 µs. For join, the smallest parent lower bound is 1536.6451 µs and
the largest candidate upper bound is 1.8260 µs. Raw per-sample operation
times remained separated, including outliers. All pairs exceed both required
practical thresholds.

The final candidate, using direct storage for a single evaluated key and
inline storage for composite keys, was remeasured in three new P/C, C/P, P/C
pairs at 1,000 rows with the same 100-sample, 3-second warmup, 5-second
measurement protocol:

| Case | Pair | Parent mean (µs) | Final candidate mean (µs) | Saved (µs) | Faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| Complete unique lookup | 1 | 39.8442 | 1.3968 | 38.4474 | 96.494% |
| Complete unique lookup | 2 | 39.8193 | 1.3904 | 38.4289 | 96.508% |
| Complete unique lookup | 3 | 40.0175 | 1.4114 | 38.6061 | 96.473% |
| Parameterized join | 1 | 1550.0202 | 1.7530 | 1548.2672 | 99.887% |
| Parameterized join | 2 | 1552.3559 | 1.7706 | 1550.5853 | 99.886% |
| Parameterized join | 3 | 1553.4969 | 1.7548 | 1551.7421 | 99.887% |

The smallest parent 95% lower bounds are 39.7987 µs for lookup and
1548.2616 µs for join; the largest candidate upper bounds are 1.4131 µs
and 1.7731 µs. The raw samples and intervals are stored under the unique
`final_p*` Criterion baselines. Every pair clears the absolute and relative
gain thresholds.

The short screening run found near-flat candidate latency from 100 to 10,000
rows: complete lookup was 1.391/1.436/1.420 µs at 100/1,000/10,000 rows,
while the parent was 5.218/40.064/391.900 µs. Candidate parameterized join
was 1.760/1.829/1.886 µs, while the parent was
184.750/1553.000/15487.000 µs. These short numbers are scaling diagnostics,
not acceptance baselines. The original partial-only lookup remained about
47.9 µs at 1,000 rows; adding a diagnostic single-column index made it about
1.4 µs. The latter is a control, not the proposed solution.

The initial full fake-only catalog completed 170 matching cases on each
revision. Thirteen candidate controls exceeded the 5% slowdown threshold in
that single pass. Three alternating paired repeats of all thirteen controls
showed the large SQLx lookup and join outliers were run-order variation. Two
sub-microsecond controls did repeat a slowdown over 5% in all three pairs:

| Control | Pair 1 | Pair 2 | Pair 3 |
| --- | ---: | ---: | ---: |
| Prepared reuse | +5.72% | +5.67% | +7.07% |
| One-version old-snapshot read | +5.67% | +6.35% | +7.34% |

The native unique-index control rose by +4.74/+6.15/+6.28% at 100 rows and
+4.46/+5.29/+5.07% at 10,000 rows. These corroborate an approximately
0.03–0.04 µs fixed overhead. The initial candidate allocated a `Vec<Value>`
for every indexed prepared execution, including one-key lookups. This is a
repeatable regression under the acceptance rule, so the initial candidate is
not acceptable. An inline `SmallVec` alone did not reduce the overhead in a
short screening run. The revised candidate stores one evaluated key directly
and uses `SmallVec` for composite keys, while sharing the same lookup path.
A short four-control screen lowered prepared reuse from about 0.570 to
0.527 µs and one-version old-snapshot read from about 0.600 to 0.557 µs;
the paired repeats then showed no repeatable control slowdown above 5%.
In three new alternating parent/final-candidate pairs of all thirteen
suspected controls, prepared reuse changed by +0.47%, -1.45%, -1.17%,
and the one-version old-snapshot read by -1.98%, +0.99%, -1.60%.
The largest single result in those pairs was +6.52% for a SQLx unique lookup,
which was -1.00% and -0.27% in its other two pairs. No control repeated a
slowdown above 5%.

The final candidate completed all 170 cases in a separate full fake-only pass.
Seven more cases exceeded 5% against the parent's full-pass baseline and were
repeated in three alternating parent/candidate pairs. Six did not reproduce a
slowdown above 5%: runtime temporal, core snapshot, membership join, UNION,
serializable read, and serializable write skew. `UNION ALL` at 100 rows changed
by +7.06%, +5.91%, and +4.72%. Three more isolated, ordinary-length pairs
gave +4.24%, +7.75%, and +3.99%. Its parent timings varied noticeably across
runs, so the near-threshold result needed a longer measurement. Three new
alternating pairs with 200 samples, 10 seconds warmup, and 20 seconds
measurement gave +4.32%, -0.98%, and +3.58%. The candidate's means were
372.077, 374.108, and 370.949 µs; the parent's were 356.653, 377.813, and
358.135 µs. The second parent run was unusually slow, but none of the longer
pairs exceeded 5%. The earlier results are treated as measurement variation,
not a repeatable regression above the gate. Both revisions ran identical
`VALUES` queries in the same fake-only runner. A trial using a heap vector for
multi-column evaluated keys did not improve this control and was discarded.

After removing redundant reference operators, the exact final source passed
three further primary pairs under the original 100-sample, 3-second warmup,
5-second measurement protocol:

| Case, 1,000 rows | Pair | Parent mean (µs) | Candidate mean (µs) | Saved (µs) | Faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| Complete unique lookup | 1 | 39.9275 | 1.4028 | 38.5247 | 96.487% |
| Complete unique lookup | 2 | 40.2354 | 1.3932 | 38.8422 | 96.537% |
| Complete unique lookup | 3 | 40.0043 | 1.3859 | 38.6184 | 96.536% |
| Parameterized join | 1 | 1542.5156 | 1.7772 | 1540.7384 | 99.885% |
| Parameterized join | 2 | 1547.9931 | 1.7736 | 1546.2195 | 99.885% |
| Parameterized join | 3 | 1544.5675 | 1.7594 | 1542.8081 | 99.886% |

Every pair's 95% confidence intervals are separated and clear both primary
thresholds. In the full pass, cold prepare plus execute fell from 65.088 to
24.565 µs. Complete-key UPDATE and DELETE with rollback outside the timer
fell from 87.325 to 26.601 µs and 76.296 to 15.488 µs. The missing-key
lookup fell from 39.793 to 1.155 µs, and the duplicate nonunique lookup from
78.350 to 1.781 µs. The partial-only lookup remained 47.949 to 47.953 µs;
the diagnostic single-column-index control remained 1.355 to 1.355 µs.

The algorithm replaces inspection of all 1,000 row versions per complete-key
lookup with one ordered composite index probe and at most one matching row
candidate. A separate five-second macOS CPU sample after benchmark warmup
showed the parent's prepared table scan dominating its main thread, while the
candidate spent its indexed portion in `find_unique_visible_version`. The
sampled process footprints were 11.4 MiB parent and 11.6 MiB candidate, with
11.9 MiB peaks for both. This is process memory, not per-query allocation
traffic. The implementation adds no retained cache or index; its key buffers
are temporary. Index maintenance, transaction commit/rollback, and snapshot
fork behavior use the existing storage structures.

The final candidate passed five new native composite-access tests, including
reverse physical index order, duplicate parent rows in a prepared indexed
join, old unique and nonunique versions, rollback, and serializable phantom
tracking. Its PostgreSQL 18 differential test passed for complete-key reads,
joins, mutations, NULLs, casts, metadata, and errors. The full workspace
test suite, `cargo fmt --all -- --check`, `cargo check --workspace --offline`,
and `cargo clippy --workspace --all-targets --offline` passed. Clippy emitted
existing warnings and five redundant-borrow warnings, which were removed
after the full fake-only pass. The removed borrows were compiler-dereferenced
on the same index calls; the primary timings were repeated on the exact final
source. No PostgreSQL benchmark was run.

Raw Criterion baselines, logs, and CPU samples are retained under ignored
`target/criterion/` and `target/optimization-research/results/e03/`. The
full runner was built with `prepare.py --full-fake --sqlx`; focused runners
used `--e03`, `--e03-regression`, and `--e03-final-regression`, each with
`--sqlx`. Timed commands used `--bench --noplot --sample-size 100
--warm-up-time 3 --measurement-time 5` and unique `--save-baseline` names,
except the explicitly described longer `UNION ALL` repeats.

## Decision

Accept E03. The complete-key lookup and parameterized join exceed the 10%
and 1 µs gates in every final paired run, with separated confidence intervals.
The full 170-case fake-only catalog and focused repeats found no repeatable
slowdown above 5%; the near-threshold `UNION ALL` result was resolved with
longer paired measurements. Correctness, memory/profile, workspace checks,
and independent subagent review are complete. The reviewer found no remaining
problem after the reverse-index and duplicate-parent prepared-join cases were
added. No PostgreSQL benchmark was run.
