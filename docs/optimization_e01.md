# E01 — Indexed candidates in guarded mutations

**Decision:** Accepted after clean subagent re-review. Parent revision:
`201b63dd293c785fd69058e8cce042049476b92c`.

## Hypothesis and change

An indexed equality inside nested `AND` can select the only possible mutation
row before evaluating the full guard. The same constraint extractor now serves
table scans, mutation targets, and row-lock discovery. Lock discovery evaluates
the complete predicate on the candidate before locking it. Lock acquisition
retries the selection after a concurrent row change. OR and unsafe equalities
keep the scan path. Constant
boolean operands are validated even when the indexed key is absent, so
an error such as `1 / 0` is not hidden by the lookup.

## Method

Apple M2, macOS 15.8.1 (24H32), `rustc 1.98.1`, release profile, unchanged
dependencies and fixture SQL. Criterion used 100 samples, 3 s warmup, 5 s
measurement, with independently warmed parent/candidate pairs in P/C, C/P,
P/C order. Each operation used a prepared mutation against a 1,000-row table
with one matching UUID key; keys rotated. The timed region contains the mutation
and result handling. `BEGIN` and `ROLLBACK` are outside it. No PostgreSQL
latency appears in the comparison.

| Operation | Pair | Parent mean (µs) | Candidate mean (µs) | Saved (µs) | Faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| Guarded UPDATE | 1 | 3693.324 | 36.055 | 3657.269 | 99.024% |
| Guarded UPDATE | 2 | 3625.249 | 36.096 | 3589.153 | 99.004% |
| Guarded UPDATE | 3 | 3706.168 | 35.920 | 3670.248 | 99.031% |
| Guarded DELETE | 1 | 3670.477 | 20.958 | 3649.519 | 99.429% |
| Guarded DELETE | 2 | 3627.513 | 20.889 | 3606.624 | 99.424% |
| Guarded DELETE | 3 | 3619.705 | 20.850 | 3598.855 | 99.424% |

The 95% mean confidence intervals are separated in every pair. Every pair
exceeds both the 10% and 1 µs acceptance thresholds. In the full catalog's
separate scale check, guarded UPDATE changed from 394.388 to 35.706 µs at 100
rows and from 35,990.511 to 36.004 µs at 10,000 rows. Guarded DELETE changed
from 379.588 to 20.754 µs and from 36,037.975 to 20.905 µs at those sizes.
The flat candidate times support the intended bounded candidate discovery.

The same prepared UPDATE and DELETE were also timed with `BEGIN` and `ROLLBACK`
inside the timer. Three parent/candidate pairs at 1,000 rows gave:

| Operation | Pair | Parent mean (µs) | Candidate mean (µs) | Saved (µs) | Faster |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full UPDATE transaction | 1 | 3522.038 | 40.073 | 3481.965 | 98.862% |
| Full UPDATE transaction | 2 | 3528.952 | 39.434 | 3489.518 | 98.883% |
| Full UPDATE transaction | 3 | 3491.495 | 39.219 | 3452.276 | 98.877% |
| Full DELETE transaction | 1 | 3533.548 | 24.226 | 3509.322 | 99.314% |
| Full DELETE transaction | 2 | 3510.412 | 24.047 | 3486.365 | 99.315% |
| Full DELETE transaction | 3 | 3474.748 | 23.915 | 3450.834 | 99.312% |

Their 95% mean confidence intervals are also separated in every pair.

The full pg_fake catalog compared 135 matching native and SQLx cases. Two
serializable cases omitted from its first fake-only entry were then run through
a dedicated fake-only entry on both revisions: serializable read was 17.960 to
17.574 µs, and serializable write skew was 129.363 to 129.311 µs. Together these
cover all 137 pg_fake cases in the production catalog. Four
controls appeared more than 5% slower in that single pass: indexed DELETE at
1,000 rows, nonunique lookup at 1,000 rows, single-row UPDATE, and unique lookup
at 100 rows. Three alternating parent/candidate repeats of those exact cases
did not reproduce a slowdown above 5% across pairs. Their candidate changes
were respectively `-2.36%, -48.04%, -0.49%`; `+3.54%, +12.81%, -5.79%`;
`+0.64%, -0.72%, +0.04%`; and `+3.13%, +12.19%, -0.57%` (positive means slower).
The second indexed-DELETE parent estimate was noisy at 39.855 µs; the other
parent estimates were 21.118 and 20.825 µs. Bare-key UPDATE and DELETE at
1,000 rows changed from 19.695 to 19.889 µs and from 10.177 to 10.408 µs in
the full pass. False-guard and missing-key cases changed from 3651.671 to
21.824 µs and from 3651.179 to 15.934 µs.

The change adds no retained cache or redundant index. Constraint traversal uses
an ephemeral stack, and only the candidate row receives residual evaluation;
the work is not moved to commit, rollback, or fork disposal. A 5 s post-change
sample placed 1,792 of 3,695 main-thread samples under row-lock acquisition,
including 593 under required-row-lock collection. The earlier baseline profile
placed 97.9% under lock acquisition, though its sampling boundary also included
rollback, so these percentages are attribution hints rather than comparable
latencies. The sampled process footprint was 20.6 MiB; there is no paired
memory measurement.

## Correctness and checks

- Native `pg_fake` tests passed, including index, locking, transaction, and
  serializable tests. The new indexed-mutation native tests passed.
- The focused PostgreSQL 18 differential test passed for UPDATE/DELETE,
  RETURNING, nested/reordered guards, missing keys, contradictions, NULL, OR,
  parameters, division-by-zero, and invalid casts.
- Workspace tests passed against a disposable local PostgreSQL 18 server with
  two unrelated `SET timezone = 'CET'` tests skipped because that server lacks
  the CET timezone entry. A full run without those skips failed only at those
  two cases. No PostgreSQL benchmarks were run after the request to stop them.
- `cargo fmt --all -- --check`, `cargo check --workspace --offline`, and
  `cargo clippy --workspace --all-targets --offline` passed. Clippy emitted four
  warnings in untouched files; treating warnings as errors fails on those
  existing warnings.

Raw Criterion samples and logs are in ignored `target/criterion/` and
`target/optimization-research/results/e01/`. The fake-only runner is built with
`python3 docs/optimization_research/2026-10-12/prepare.py --full-fake --sqlx`;
the focused runner uses `--e01 --sqlx` and regression repeats use
`--regression --sqlx`. The serializable controls use `--serializable --sqlx`.
Timing commands use the resulting
`target/release/research-workloads` binary with `--bench --noplot --sample-size
100 --warm-up-time 3 --measurement-time 5` and unique `--save-baseline` names.
