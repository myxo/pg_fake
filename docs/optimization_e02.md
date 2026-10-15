# E02 — Selective outer joins through indexed probes

**Decision:** Accepted. Parent revision:
`7b2c05964ea9f6fbb349ab5cf5d31ac171fbb469`.

## Hypothesis and change

A selective predicate on the preserved side of a LEFT JOIN can limit the left
input before joining. When the right join column has an index, the executor can
fetch matching right rows instead of scanning and hashing the whole relation.
The candidate passes the WHERE expression to the streamed path for a single
LEFT JOIN, pushes only filters confined to the preserved side, and skips right
WHERE pushdown before null extension. Unique and nonunique indexes can provide
right rows. Nonunique probes retain the relation-level serializable read. RIGHT,
FULL, and more complex outer joins keep the prior fallback.

## Method and primary results

Apple M2, macOS 15.8.1 (24H32), `rustc 1.98.1`, release build, identical
benchmark source in both revisions. The 1,000-row primary used three
independently warmed parent/candidate pairs in P/C, C/P, P/C order. Criterion
used 100 samples, 3 s warmup, 5 s measurement, and fresh baseline names. The
timer covers one prepared query and result handling, with rotating UUID keys.
Fixture creation and statement preparation are outside the timer. Both primary
queries return one row. PostgreSQL was used only for correctness, never for
latency comparison.

| Query | Pair | Parent mean (µs) | Candidate mean (µs) | Saved (µs) | Faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| `left_roles` | 1 | 1158.017 | 27.643 | 1130.374 | 97.613% |
| `left_roles` | 2 | 1165.206 | 27.832 | 1137.374 | 97.611% |
| `left_roles` | 3 | 1154.417 | 28.050 | 1126.367 | 97.570% |
| `left_group` | 1 | 1704.255 | 41.082 | 1663.173 | 97.589% |
| `left_group` | 2 | 1700.106 | 40.861 | 1659.244 | 97.597% |
| `left_group` | 3 | 1703.847 | 41.584 | 1662.263 | 97.559% |

The 95% mean confidence intervals are separated in every pair; the smallest
parent lower bound is 1,152.094 µs for `left_roles` and 1,697.810 µs for
`left_group`, above the largest candidate upper bounds of 28.105 and
41.642 µs respectively. Every pair exceeds the 10% and 1 µs thresholds.

| Query | Rows | Parent mean (µs) | Candidate mean (µs) | Faster |
| --- | ---: | ---: | ---: | ---: |
| `left_roles` | 10 | 37.371 | 27.721 | 25.82% |
| `left_roles` | 100 | 138.639 | 27.682 | 80.03% |
| `left_roles` | 500 | 609.484 | 27.776 | 95.44% |
| `left_roles` | 10,000 | 12,330.967 | 30.354 | 99.75% |
| `left_group` | 10 | 54.678 | 40.463 | 26.00% |
| `left_group` | 100 | 212.251 | 40.456 | 80.94% |
| `left_group` | 500 | 892.308 | 40.571 | 95.45% |
| `left_group` | 10,000 | 17,754.787 | 43.307 | 99.76% |

The 10,000-row fixture was inserted in 500-row batches inside one setup
transaction in both revisions. This prevents fixture preparation from dominating
the benchmark run; it does not change the timed query or stored rows. Candidate
latency stays nearly flat as unrelated rows increase, matching the indexed
candidate path in the source. The candidate retains only query-local rows and
probe keys; it adds no persistent cache or deferred cleanup work.

Temporary counters in an ignored diagnostic build counted source rows and
right probes at 1,000 rows. They were removed from the deliverable source.

| Case | Left rows | Right index probes | Right rows scanned | Right candidates |
| --- | ---: | ---: | ---: | ---: |
| `left_roles` | 1 | 1 | 0 | 1 |
| `left_group` | 1 | 1 | 0 | 1 |
| `left_roles_unindexed` | 1 | 0 | 999 | 1 |
| `left_roles_full_output` | 1,000 | 0 | 1,000 | 1,000 |

The indexed path does not visit unrelated right rows. The full-output and
unindexed cases use their existing scan paths. These counts agree with the
near-flat 100-to-10,000-row candidate timings. The diagnostic source and log
are under ignored `target/optimization-research/e02-diagnostic/`.

## Targeted controls

At 1,000 rows, `left_roles_many` changed from 1174.937 to 29.334 µs and
returned two rows; `left_roles_zero` changed from 1177.368 to 27.299 µs and
returned one null-extended row; an absent left row changed from 1172.169 to
24.950 µs and returned none. The unindexed right join changed from 1174.910 to
71.039 µs, and cold prepare plus execute from 1226.685 to 70.560 µs. The
full-output LEFT JOIN changed from 674.776 to 660.672 µs, a 2.09% improvement.
No targeted control slowed by more than 5%.

## Full catalog and regression repeats

The fake-only full catalog covered 157 cases in each revision: 141 existing
cases and 16 new outer-join cases. The parent initially used an older full
entry list and ran its 141 existing cases; the 16 new cases were then run with
the updated full entry and the same benchmark code and settings. The candidate
ran all 157 in one pass. Three pre-existing groups override Criterion's
100-sample/5-second settings with 20 samples and 2 seconds; their definitions
are identical between revisions.

Ten existing controls were more than 5% slower in the single catalog pass.
They were repeated in three independently warmed P/C, C/P, P/C pairs. Eight
did not repeat a slowdown above 5%; the largest original outlier,
`tier2_join_profile_3k`, changed by -1.62%, -0.40%, and +2.77% in those pairs
(positive means slower). The remaining two had mixed results:

| Control | Three broad repeat pairs | Three further focused pairs |
| --- | --- | --- |
| 1,000-row heap scan | +6.33%, +6.15%, -0.35% | -0.83%, -0.49%, -0.81% |
| Sequential concurrent reads | +2.42%, +5.65%, +6.76% | +0.85%, +2.29%, +0.58% |

The further pairs used the same fake-only workload functions and alternated
P/C, C/P, P/C. Neither control reproduced a slowdown above 5% there. Their
absolute times changed with the narrower workload filter, so the broad-pass
differences are treated as measurement variability, not a repeatable
regression. No control showed a persistent slowdown above the 5% gate.

## Post-change profile and memory

A 5-second macOS `sample` attached after `READY` to the warmed 1,000-row
`left_roles` handoff probe. Of roughly 3,730 main-thread samples, 470 were
under `visit_streamed_join_rows` and 202 under the indexed hash-join chain.
The profile shows the full-right scan has been displaced by a bounded index
probe; the remaining time is spread across session, query, expression, and
result work. The process footprint was 27.1 MiB (28.2 MiB peak). There is no
paired memory measurement, and the optimization adds no retained cache.

## Validation and artifacts

- Three native selective-outer-join tests passed, including null extension,
  old snapshots, and a serializable write-skew schedule with a nonunique right
  probe.
- PostgreSQL 18 differential tests passed for LEFT, RIGHT, FULL, nullable
  keys, zero/many matches, right-side `IS NULL`, ON filters, OR, prepared
  parameters, locking reads, join chains, and serializable schedules. No
  PostgreSQL latency benchmark was run.
- Workspace tests passed against the disposable local PostgreSQL 18 instance
  with two unrelated CET timezone tests skipped because its timezone database
  lacks that entry. `cargo fmt --all -- --check`, `cargo check --workspace
  --offline`, and `cargo clippy --workspace --all-targets --offline` passed.
  Clippy reported existing warnings and one unused shared differential helper
  warning in the new test target.

Raw Criterion samples, logs, and profiles are in ignored
`target/criterion/` and `target/optimization-research/results/e02/`. The
fake-only runner comes from
`python3 docs/optimization_research/2026-10-12/prepare.py --full-fake --sqlx`;
the focused runners use `--e02 --sqlx` and `--e02-regression --sqlx`.
All timing commands use the resulting
`target/release/research-workloads` with `--bench --noplot --sample-size 100
--warm-up-time 3 --measurement-time 5` and unique `--save-baseline` names.
