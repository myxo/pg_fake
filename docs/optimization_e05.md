# E05 — Prove partial-index eligibility from bound predicates

Status: accepted. Parent revision: `be94c5c5`.

## Hypothesis and predeclared gate

The membership queries with `deleted_at IS NULL` can use the existing partial
indexes when the query's bound conjuncts imply that predicate. Today access
selection excludes every partial index, so those queries scan unrelated rows.
Choose the specific eligible index, probe its complete key or bounded prefix,
and recheck the visible row and the entire query predicate. An unknown
implication falls back to an unfiltered index or scan.

The primary workload is the handoff's original partial-index schema, without
diagnostic indexes: prepared membership profile and group-member joins, each
returning one row with 100, 1,000, and 10,000 rows in each source table. Rotate
bound account/group keys across warm executions. At 1,000 rows, measure each
query in three independently warmed alternating parent/candidate pairs, using
at least 100 Criterion samples, 3 seconds warmup, and 5 seconds measurement.
Each pair must save at least 10% and 1.0 µs per query, with the 95% mean
confidence interval excluding no improvement.

Secondary cases separate predicate-only single-column access, unfiltered
composite access, partial complete-key access, and partial prefix access.
Include missing keys, soft-deleted rows, queries without the predicate, cold
prepare/execute, and a small 10-row fixture. Check result cardinality before
timing. Run Tier 1, directly affected Tier 2/3, native diagnostic, SQLx, and
concurrency controls on both revisions, then the full fake-only benchmark
catalog. Repeat each suspected regression above 5% in three pairs. Include
bounded inserts, updates, soft deletes, commit/rollback, snapshot fork, and
memory observations; do not move preparation or cleanup across timer boundaries.

Correctness coverage must compare rows, empty results, metadata, and errors
against PostgreSQL 18. Check reordered/nested conjuncts, parameters with
changed values, OR fallback, partial-index DDL/invalidation, savepoints,
old snapshots, stale index entries, and serializable reads. A visible version
must satisfy the selected index's own predicate before it can be returned.

## Measurements and decision

The implementation proves conjunctions of resolved `IS NULL` / `IS NOT NULL`
column tests and carries the selected index identity into storage. Supported
partial indexes retain entries only for qualifying row versions; the visible
version and the complete query predicate are rechecked. Other predicates use
the existing fallback. Native and PostgreSQL 18 differential tests vary a
nullable comparison parameter between NULL and non-NULL values, confirming
that it does not imply the partial-index predicate. Native coverage includes
savepoints, rollback, reclamation, serializable reads, schema invalidation, and
historical rows after `ALTER TABLE ADD COLUMN`. PostgreSQL 18 differential coverage compares reads,
changes, prepared parameters, and the column-addition case.

The final implementation also avoids constructing predicate conjuncts when no
partial index needs proof. During the first full candidate run,
`tier2_sqlx_migration_chain` exposed a panic: historical row versions predating
an added predicate column are shorter than the current schema. The index
matcher now excludes those versions. A native test and a PostgreSQL 18
differential sequence reproduce the schema evolution.

### Method and primary result

The machine ran Darwin 24.6.0 on an Apple M2 (arm64, 16 GiB RAM) with rustc
1.98.1. Both revision archives were built with `cargo build --offline --release`
and no custom `RUSTFLAGS` in the research runner. They used the same benchmark
source, dependencies, Cargo release profile, fixtures, SQL, and rotating
prepared parameters. Criterion measured one complete prepared query per
iteration, after fixture setup. The 1,000-row primary cases returned
one row. Each process had 100 samples, 3 s warmup, 5 s requested measurement,
and an independent warmup. Parent/candidate order alternated. Commands:

```sh
python3 docs/optimization_research/2026-10-12/prepare.py --e05 --sqlx
target/optimization-research/results/e05/<revision>-focused-binary --bench --noplot \
  --sample-size 100 --warm-up-time 3 --measurement-time 5 \
  '^partial_index_access/(membership_profile|group_members)/1000$' \
  --save-baseline e05-final-p<pair>-<revision>
```

For the final candidate the archived binary is named
`candidate-focused-binary-v3`. Criterion mean estimates, in µs:

| Pair order | Membership parent → candidate | Saved | Group members parent → candidate | Saved |
| --- | ---: | ---: | ---: | ---: |
| parent / candidate | 727.149 → 2.744 | 724.405 (99.62%) | 747.412 → 2.845 | 744.566 (99.62%) |
| candidate / parent | 706.261 → 2.762 | 703.499 (99.61%) | 720.073 → 2.845 | 717.228 (99.60%) |
| parent / candidate | 706.343 → 2.751 | 703.591 (99.61%) | 722.724 → 2.884 | 719.840 (99.60%) |

The smallest conservative gain, subtracting the candidate 95% upper mean bound
from the parent 95% lower mean bound, was 702.726 µs. Every pair exceeds both
practical thresholds and excludes no improvement. The 10-row and 10,000-row
membership cases also improved. At 1,000 rows, partial complete-key access was
49.422 → 1.444 µs, prefix access 41.283 → 1.372 µs, and cold
prepare-plus-execute 64.447 → 25.217 µs. An unfiltered lookup was
32.768 → 32.948 µs (0.55% slower).

### Regression, displaced work, and memory

The original fake-only full catalog completed 217/217 cases on both revisions,
including native diagnostics, SQLx, and concurrency. Both runs used the same
release binaries and CLI timing settings. The final candidate archive is
`candidate-full-binary-v3`; its comparison with the parent is saved in
`target/optimization-research/results/e05/full-comparison-v3.csv`. Nine cases
appeared above 5% in this single sequential full-suite comparison. Each was
repeated in three independently warmed, alternating parent/candidate pairs in
the full runner. None stayed above 5% in all three pairs. For example, the
indexed 100-row SELECT changed by +11.8%, +5.5%, then −5.2%, while sequential
concurrent reads changed by +143%, +2.2%, then −6.1%. The wide reversals and
large changes in the parent means made those full-context estimates unsuitable
as isolated regressions. Focused three-pair controls measured the indexed
SELECT at +5.7%, +1.3%, then −4.0%, and isolated sequential concurrent reads
at −1.1%, −3.3%, then +0.8%. The remaining join and scan suspects also failed
to repeat above 5%.

The existing `ON CONFLICT` benchmark accumulates version history across timed
iterations; it changed by +145%, +51%, then −55% in the full-context pairs.
Rollback-bounded replacements with rotating keys and cleanup inside the timer
changed by −0.4%, +0.4%, +0.4% for inserts and +0.1%, +0.8%, +1.2% for
conflict updates. Six original groups locally override Criterion to 20 samples
and 2 s measurement. Removing only those local overrides in identical
temporary runners allowed 100-sample, 3 s warmup, 5 s requested measurements.
Five had no >5% slowdown. `update_indexed_1k` gave +21% in one pass, but its
mutation-only timer excludes rollback and its three follow-up pairs varied
by +20%, +45%, then −37% with wide intervals. A bounded 1,000-row indexed
UPDATE including BEGIN and ROLLBACK measured −1.1%, 0.0%, and −1.8% in three
pairs. These controls support no repeatable slowdown above the 5% gate.

Six new partial-index maintenance controls keep the row count bounded. They
measure active/deleted INSERT rollback, INSERT commit and DELETE, soft-delete
rollback, soft-delete commit and restore, and database snapshot through first
write and disposal. The candidate changed these by −0.9%, −3.2%, −0.7%,
−0.2%, −0.5%, and −39.5%, respectively. The last case is a stress snapshot
after many transactions; it is not a fresh 100-row snapshot estimate. Cleanup
is inside each timed operation. A separate child-process `getrusage` run found
peak RSS of 316,768,256 bytes for the parent and 292,814,848 bytes for the
candidate (7.6% lower). There is no new cache. Raw samples, build logs,
timing logs, and memory observations are retained under ignored
`target/optimization-research/results/e05/` and `target/criterion/`.

The fake-only full run used:

```sh
target/optimization-research/results/e05/<revision>-full-binary --bench --noplot \
  --sample-size 100 --warm-up-time 3 --measurement-time 5 \
  --save-baseline e05-full-<revision>
```

The new maintenance and bounded controls were timed separately with the same
Criterion settings; all original full-catalog cases and the added controls
were measured on both revisions. `prepare.py --full-fake --use-cli-timing --sqlx`
builds the temporary runner used for the six local-override checks.

### Correctness and decision

The native partial-index lifecycle tests, existing index and ALTER TABLE tests,
and PostgreSQL 18 SQLx differential test passed. The full workspace test suite
passed with one test thread after allowing its connection to the configured
local PostgreSQL server; sandboxed runs were denied at the first connection and
poisoned the suite's shared test mutex. `cargo fmt --all -- --check`,
`cargo check --workspace --all-targets --offline`, and
`cargo clippy --workspace --all-targets --offline` passed. Clippy emitted only
existing warnings outside the E05 changes.

Decision: **accepted**. All three primary pairs pass, correctness checks pass,
write and memory controls show no displaced cost, and no supported control has
a repeatable slowdown above 5% after stable paired measurement. The benchmark
and source changes were reviewed before commit.
