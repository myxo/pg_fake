# O2 — SQLx statement cache experiment

Baseline: `a7ea35d1b27658cc51a29a2f9922a24e920d79e0` (O1 complete). The
baseline and candidate used the same release-with-debug-symbols build settings,
fixture and one-worker SQLx probe. Each insert run executed 64 warmups followed
by 32,768 distinct persistent literal inserts. Times include SQLx dispatch and
result destruction. The three independent means were:

| Revision | Run 1 | Run 2 | Run 3 | Median |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 41.294 us | 41.323 us | 41.418 us | 41.323 us |
| O2 | 38.419 us | 38.280 us | 38.176 us | 38.280 us |

The matched median falls by **7.36%**. Raw output:
[baseline](o2-insert-baseline-matched.log), [O2](o2-insert-accepted.log).
On the same 32,768-insert workload, peak RSS falls from 666,419,200 bytes
(635.55 MiB) to 36,405,248 bytes (34.72 MiB), a 94.54% reduction:
[baseline](o2-memory-32k-baseline-matched.log),
[O2](o2-memory-32k-accepted.log). The 100,032-insert O2 run peaks at
85,098,496 bytes (81.16 MiB): [raw result](o2-memory-100k-accepted.log).
RSS includes the database, executable and allocator state, so it is diagnostic
rather than the cache's byte-accounting assertion.

The [full Tier 1 baseline](o2-full-baseline-paired.log) and
[full O2 run](o2-full-after-paired.log) used 20 Criterion samples, 1-second
warmup and 2-second measurement for every pg_fake and PostgreSQL case. Seven
pg_fake means improved in that pair. The update mean was distorted by severe
tail samples, and one indexed lookup baseline was unusually fast. Longer
adjacent comparisons resolved those cases:

| Tier 1 case | Baseline | O2 | Reduction | Setting |
| --- | ---: | ---: | ---: | --- |
| Indexed point lookup | 8.576 us | 8.457 us | 1.39% | 30 samples, 2s warmup, 5s measurement |
| Many-match join | 154.47 us | 154.25 us | 0.14% | 30 samples, 2s warmup, 5s measurement |
| Ordered paging | 30.619 us | 30.701 us | -0.27% | 30 samples, 2s warmup, 5s measurement; p=0.30, no detected change |
| Update, raw-sample median | 42.899 us | 42.438 us | 1.08% | 50 samples, 2s warmup, 10s measurement |
| Update, 10% trimmed mean | 42.940 us | 42.538 us | 0.94% | Same raw samples; Criterion mean 43.223→44.282 us, p=0.45 |

The update O2 sample has a 93.98-us maximum; its baseline maximum is 50.18 us.
Neither Criterion run detects a mean change. Raw focused results are the
`*accepted-focused.log`, `*targeted-estimate.log` and `o2-update-*samples.*`
files in this directory. No confirmed Tier 1 slowdown remains.

Validation: cache accounting and LRU tests include 100,000 distinct SQL keys,
replanning, typed keys, `.persistent(false)`, explicit statements, and large
default/CHECK/view definitions retained after DROP. The workspace test suite
passes with `PG_FAKE_DATABASE_URL` pointing to a local `C`-collation PostgreSQL
database and the pre-existing `reports_phase2_regression_progress` corpus
threshold test skipped. That test reports 820 statements against its 850
threshold on both O2 and the isolated baseline. Formatting and scoped workspace
Clippy pass; strict Clippy still reports the four pre-existing engine warnings
documented in the optimization plan.
