# Phase 3 release audit

The audit target is PostgreSQL 18 with `C` collation. The corpus runner creates
and removes separate databases for upstream scripts and for each conformance
case, so transaction state and session settings cannot leak between cases.

## Conformance and upstream corpus

| Gate | Phase 2 baseline | Phase 3 audit |
| --- | ---: | ---: |
| Phase 2 conformance cases | 32 | 32 / 32 |
| Phase 3 conformance cases | — | 96 / 96 |
| Matching upstream statements | 463 | 850 |
| Skipped upstream scripts | 141 | 141 |

The audit compares row multiplicity and values as a multiset unless the query
has top-level `ORDER BY`. With `ORDER BY`, it checks key order and permits rows
with equal keys to exchange places. For nonempty results it compares column
names and PostgreSQL type names, as well as affected-row counts and SQLSTATE.
Focused differential tests cover prepared metadata, transaction outcomes, and
controlled concurrent outcomes that the upstream scripts cannot schedule.

Every first upstream blocker is pinned in
[`SKIPPED.txt`](../crates/pg_fake_sqlx/tests/postgres_regress/SKIPPED.txt)
and classified in
[`SKIPPED_CATEGORIES.tsv`](../crates/pg_fake_sqlx/tests/postgres_regress/SKIPPED_CATEGORIES.tsv).
The 141 blockers divide into 5 fixture or runner cases, 15 parser limitations,
and 121 later or out of scope SQL forms and fidelity boundaries. Fixture cases
include psql variable substitution, inline COPY data, and non-UTF-8 source
encoding. Parser cases include valid statements that the pinned `sqlparser`
AST cannot represent. The later group includes broader type input and utility
functions, catalog and server administration, full COPY support, additional
DDL forms, planner-dependent expression suppression, and window expressions
used directly in final ordering. The [feature registry](phase3_coverage.md)
states the supported boundary for these families.

## Integration gates

The [Phase 3 integration test](../crates/pg_fake_sqlx/tests/phase3_integration.rs)
runs a shared workflow through PostgreSQL 18, SQLx pg_fake, and the native API.
It combines a view, JSONB, UUID arrays, `timestamptz`, a CTE upsert, a moving
window aggregate, session-local settings, savepoint recovery, row locks, and
SERIALIZABLE sessions. It also checks prepared column metadata and typed SQLx
decoding. The separate Task 30 application workload remains the priority
application gate; its result is reported separately from the synthetic and
upstream corpus counts.

| Surface | Focused SQLx and native coverage |
| --- | --- |
| CTEs, set operations, conflicts, views, and transactional DDL | [manifest](../crates/pg_fake_sqlx/tests/postgres_regress/phase3_manifest.rs), [migration chains](../crates/pg_fake_sqlx/tests/migration_chains.rs), [transactional DDL](../crates/pg_fake_sqlx/tests/transactional_ddl.rs) |
| JSONB, arrays, temporal values, and prepared codecs | [JSON differential](../crates/pg_fake_sqlx/tests/json_differential.rs), [array differential](../crates/pg_fake_sqlx/tests/array_differential.rs), [time codecs](../crates/pg_fake_sqlx/tests/time_codec_differential.rs), [integration workflow](../crates/pg_fake_sqlx/tests/phase3_integration.rs) |
| Windows, settings, savepoints, and locks | [property suite](../crates/pg_fake_sqlx/tests/property_tests.rs), [settings](../crates/pg_fake_sqlx/tests/settings.rs), [savepoints](../crates/pg_fake_sqlx/tests/savepoints.rs), [row locks](../crates/pg_fake_sqlx/tests/row_lock_differential.rs) |
| Advisory keys and SERIALIZABLE concurrent histories | [text hashes](../crates/pg_fake_sqlx/tests/text_hash_differential.rs), [SERIALIZABLE differential](../crates/pg_fake_sqlx/tests/serializable_differential.rs), [native transaction tests](../crates/pg_fake/src/session/tests.rs) |

The full benchmark catalog includes paired PostgreSQL 18 and pg_fake workloads
for set operations, recursive CTEs, conflicting inserts, windows, JSONB,
arrays, `OffsetDateTime`, text-derived advisory keys, views, savepoints,
transactional DDL, row locking, and SSI. The
[recorded benchmark report](../crates/pg_fake_benchmarks/results/report.md)
uses an Apple M2 and was recorded on 2026-09-22. Representative comparisons
from that report follow; “slower” means pg_fake takes more time.

| Workload | Comparison with PostgreSQL 18 |
| --- | ---: |
| `UNION ALL` | 4.99× slower |
| Recursive numeric CTE | 33.54× slower |
| `ON CONFLICT DO UPDATE` | 1.55× slower |
| Moving window aggregate | 4.01× slower |
| JSONB containment | 6.35× slower |
| Array containment | 2.92× slower |
| `OffsetDateTime` bind/store/fetch | 1.68× slower |
| Text-derived advisory lock | 468.54× slower |
| Nested filtered view | 13.84× slower |
| Nested savepoint release | 1.58× faster |
| Transactional DDL create/rollback | 23.63× faster |
| `SKIP LOCKED` queue | 1525.94× slower |

The Task 39 isolated SSI measurements, recorded separately from that report,
measured an uncontended SERIALIZABLE read 31.8× faster and a contended
write-skew workload 11.8× faster than PostgreSQL 18. Most workload comparisons
above miss the project's order-of-magnitude speed goal. The benchmark baseline
has not been rewritten to conceal those misses.

## Final validation

The final 10,000-iteration long property gate passed all 11 suites in 748.64
seconds. The all-feature workspace regression passed 593 tests across 48
reported suites. Two `CET`-dependent settings cases ran separately against the
configured PostgreSQL server because the disposable PostgreSQL 18 container
does not provide that timezone name. All six settings tests passed there. Two
repeated runs of the Phase 3 integration, row-lock differential, and
SERIALIZABLE differential tests passed. Formatting and strict all-target,
all-feature workspace Clippy passed. The strengthened corpus and conformance
audit passed 850 matching statements, 141 classified skips, 32/32 Phase 2
cases, and 96/96 Phase 3 cases; DML `RETURNING` rows and each Phase 2 case's
isolated database are included in those checks.
The Task 30 SQLx application-workload gate passed its three tests separately
within the workspace run.

A short paired `UNION ALL` smoke run on the disposable PostgreSQL 18 server
measured 436.66 µs for pg_fake and 550.33 µs for PostgreSQL, about 1.26 times
faster for pg_fake in this local sample. This smoke measurement does not replace
the recorded baseline above or change the documented speed misses. Independent
review found no remaining actionable issue after the audit-gate fixes.
