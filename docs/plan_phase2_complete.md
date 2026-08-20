# pg_fake — Phase 2 Implementation Plan

Phase 2 delivers the scope from `spec.md` §9: joins, subqueries, aggregation,
`DISTINCT`, sequences and `SERIAL`, DML `RETURNING`, foreign keys, UUID, and
temporal types. A few small adjacent features are included where they are
prerequisites for that scope or cheaply unlock useful PostgreSQL regression
coverage. Phase 3 features remain out of scope.

The plan is a linear sequence of small tasks. Each task has a **Goal**, a
testable **Definition of Done (DoD)**, and **Notes** describing dependencies and
scope boundaries. The completed Phase 1 plan is archived in
`plan_phase1_complete.md`.

## Conventions

- "Differential test" means running the same SQL against PostgreSQL 18 and
  `pg_fake`, comparing result values, row multiplicity, ordered results when
  requested, column metadata, and `SQLSTATE`.
- The embedded PostgreSQL regression corpus is a prioritization and conformance
  source, not a requirement to implement features outside Phase 2. At the Phase
  2 baseline, 248 statements compare successfully and all 141 scripts have a
  recorded blocker.
- Task 1 makes Phase 2 progress visible independently of blockers caused by
  missing upstream fixtures or Phase 3/later features. Upstream SQL files remain
  unmodified.
- Every feature task must extend the property-based differential generator when
  its behavior can be generated meaningfully. Otherwise, the completion handoff
  must explain why and provide focused differential coverage.
- Every feature task must add or update a representative benchmark when it can
  materially affect common query or mutation workloads. Otherwise, the
  completion handoff must explain why.
- Before a task can be marked complete, the property suite must pass at least
  10,000 `chaos_theory` iterations with
  `CHAOS_THEORY_CHECK_ITERS=10000 CHAOS_THEORY_CHECK_TIME=600s cargo test -p pg_fake_sqlx --test property_tests`.
- After implementing a task, update its progress but do not mark it complete
  until the user approves the result, as required by `AGENTS.md`.
- Features not listed here retain the unsupported-feature behavior from
  `spec.md` §10. In particular, an optimization-only statement may be tolerated
  only when ignoring it cannot change Tier-A behavior; strict mode must still
  reject it.

## Phase 2 regression focus

The primary upstream source files for this phase are:

- query processing: `join.sql`, `subselect.sql`, `aggregates.sql`,
  `select_distinct.sql`, `select_having.sql`, and `select_implicit.sql`;
- DML and generated values: `returning.sql`, `sequence.sql`, and `identity.sql`;
- referential integrity: `foreign_key.sql`;
- types: `uuid.sql`, `date.sql`, `time.sql`, `timestamp.sql`,
  `timestamptz.sql`, and `interval.sql`.

Passing an entire source file is not a per-task requirement because those files
also exercise out-of-phase features. Each task instead promotes relevant
statements into the Phase 2 conformance set and records newly exposed blockers.

---

## Milestone A — Conformance baseline and query foundations

### Task 1 — Phase 2 regression manifest and progress reporting [COMPLETE]

**Goal:** Make the embedded PostgreSQL corpus an actionable Phase 2 scorecard
without treating unrelated later-phase blockers as failures.

**DoD:**

- A checked-in manifest maps Phase 2 features to relevant statements or focused
  cases derived from the upstream files listed above, including the source file
  and statement location.
- The regression runner reports the full-corpus passed-statement count, skipped
  scripts, Phase 2 conformance cases passed, and the first blocker for each
  Phase 2 feature.
- Previously passing statements and Phase 2 cases cannot regress silently; the
  test fails if either set shrinks without an explicit manifest review.
- Required setup data is supplied by local harness fixtures rather than by
  editing the copied upstream SQL.
- The initial report records the current baseline of 248 matching statements
  and 141 skipped scripts.

**Notes:** This task changes test infrastructure only. It does not make the full
upstream corpus a Phase 2 exit criterion.

### Task 2 — Small compatibility wins [COMPLETE]

**Goal:** Add low-cost PostgreSQL behavior that supports Phase 2 queries and
allows the regression runner to reach more meaningful statements.

**DoD:**

- Constant `SELECT` without `FROM` supports expressions, aliases, ordering, and
  `LIMIT`/`OFFSET` where meaningful.
- Standalone `VALUES` queries support multiple rows, common-type resolution,
  default `columnN` metadata, ordering, and limiting.
- `INSERT INTO t DEFAULT VALUES` works and uses the existing default and
  constraint paths.
- `ANALYZE` and planner-only `SET`/`RESET` variables may be tolerated as no-ops
  in normal mode, but strict mode rejects them. Variables that affect supported
  observable semantics, including `TimeZone`, are not silently ignored.
- Differential cases cover results, metadata, errors, strict-mode behavior, and
  at least one newly reached upstream blocker.

**Notes:** These are explicit small extensions, not permission to accept
arbitrary unsupported syntax. `TRUNCATE`, temporary-table semantics, and broad
DDL expansion remain out of scope.

### Task 3 — Bound relation scopes, aliases, and qualified columns [COMPLETE]

**Goal:** Replace the single-table expression context with a bound row schema
that later joins, grouping, and subqueries can share.

**DoD:**

- The analyzer produces bound column slots carrying name, type, source relation,
  qualifier, and output metadata; the executor no longer resolves ordinary
  query columns directly against one `TableSchema`.
- Table aliases, column aliases, qualified references, `table.*`, and projection
  aliases work for a single table.
- Unqualified names resolve only when unique; missing and ambiguous references
  return PostgreSQL-compatible `SQLSTATE` values.
- Quoted identifier case rules and alias visibility match PostgreSQL for the
  supported clauses.
- Prepared-statement parameter inference uses the same bound scopes rather than
  a separate approximation.
- Differential and property tests cover aliasing, qualification, wildcard
  expansion, ambiguity, metadata, and parameter inference.

**Notes:** This is the shared structural prerequisite for the remaining query
milestones. It should introduce one coherent bound representation rather than
parallel special cases in the executor.

---

## Milestone B — Joins and subqueries

### Task 4 — Multiple sources, `CROSS JOIN`, and `INNER JOIN` [COMPLETE]

**Goal:** Execute inner relational combinations using the bound row model.

**DoD:**

- Comma-separated `FROM` items, `CROSS JOIN`, and `INNER JOIN ... ON` produce
  the correct Cartesian or matched rows.
- Join predicates use three-valued logic; only true matches are retained.
- `JOIN ... USING` and `NATURAL JOIN` expose merged key columns in PostgreSQL's
  output order while retaining correct qualified-name visibility.
- Nested joins and aliases over joined relations bind correctly.
- Differential tests cover duplicate matches, NULL keys, ambiguity, wildcard
  shape, metadata, and joins combined with filtering, ordering, and limiting.
- The benchmark suite includes a small selective inner join and a many-match
  join.

**Notes:** A tree-walking nested-loop implementation is sufficient. Physical
join algorithms and planner behavior are Tier C.

### Task 5 — `LEFT`, `RIGHT`, and `FULL OUTER JOIN` [COMPLETE]

**Goal:** Complete the Phase 2 join family with PostgreSQL null-extension
semantics.

**DoD:**

- Left, right, and full outer joins emit every matched row and exactly one
  null-extended row for each unmatched input row.
- `ON`, `USING`, and natural outer joins have the correct output columns,
  merged-key values, and qualified references.
- Predicates in `ON` and `WHERE` are applied at the correct stage.
- Nested inner/outer joins preserve their explicit association and alias scope.
- Differential/property cases cover empty sides, duplicate matches, NULL join
  keys, non-equi predicates, and post-join filtering.

**Notes:** Join reordering is unnecessary; execute the parsed join tree.

### Task 6 — Derived tables and uncorrelated scalar subqueries [COMPLETE]

**Goal:** Allow queries to consume the result of another query as a relation or
scalar value.

**DoD:**

- `FROM (<query>) AS alias [(columns...)]` materializes a derived relation with
  correct names and types.
- Scalar subqueries in expressions return NULL for zero rows, the value for one
  row, and `21000` for more than one row; more than one output column is rejected
  with PostgreSQL's error category.
- Nested derived tables and scalar subqueries work in projection, predicates,
  ordering, and DML expressions where expressions are already accepted.
- An uncorrelated subquery is evaluated once per statement rather than once per
  outer row.
- Differential cases cover empty, singleton, and multi-row results, NULLs,
  aliases, metadata, and error codes.

**Notes:** Correlation and subquery predicates land in the following tasks.

### Task 7 — `EXISTS`, `IN`, `NOT IN`, `ANY`, and `ALL` [COMPLETE]

**Goal:** Implement PostgreSQL's row-membership and quantified-subquery
semantics.

**DoD:**

- `EXISTS`/`NOT EXISTS`, scalar `IN`/`NOT IN`, and comparison with `ANY`/`SOME`
  and `ALL` work for uncorrelated subqueries.
- Empty-set and NULL behavior follows PostgreSQL three-valued logic, especially
  `NOT IN` with a NULL-producing subquery.
- Row-value membership supports multiple columns with arity/type validation.
- Subquery output types participate in the central coercion rules.
- Differential/property tests cover empty sets, matches, misses, NULLs on both
  sides, duplicates, row values, and incompatible types.

### Task 8 — Correlated subqueries [COMPLETE]

**Goal:** Resolve and execute subqueries that reference enclosing query rows.

**DoD:**

- Scalar, `EXISTS`, membership, and quantified subqueries can reference columns
  from one or more outer scopes.
- Nearest-scope name resolution, qualification, ambiguity, and shadowing match
  PostgreSQL.
- Correlated subqueries work in projection, `WHERE`, and join predicates.
- Execution does not leak a bound outer row between evaluations or sessions.
- Differential/property cases cover nested correlation, shadowed aliases,
  NULLs, and cardinality errors.

**Notes:** `LATERAL` table sources are not required in Phase 2.

**Completed:**

- Correlated scalar, `EXISTS`, membership, and quantified subqueries resolve
  enclosing rows at evaluation time, including nested outer scopes and join
  predicates.
- Scope-depth binding enforces nearest-scope lookup, qualified-name shadowing,
  ambiguity errors, and per-row scalar cardinality checks without retaining
  outer rows between evaluations.
- Native, PostgreSQL differential, generated differential, and benchmark
  coverage now includes correlation, nested scopes, alias shadowing, NULLs,
  cardinality errors, and a 100-row correlated `EXISTS` workload.
- The Phase 2 manifest promotes `correlated_exists` to `MustPass`; the current
  report records 413 matching corpus statements, 141 skipped scripts, and 22
  passing Phase 2 conformance cases with no correlated-subquery blocker.
- `cargo test --workspace`, `cargo check --workspace --all-targets`, and the
  10,000-iteration property suite pass.

---

## Milestone C — Aggregation and duplicate elimination

### Task 9 — Aggregate execution and core aggregate functions [COMPLETE]

**Goal:** Add the aggregate execution stage and the common PostgreSQL aggregate
set.

**DoD:**

- Global aggregation works with no `GROUP BY`, producing one group even for an
  empty input.
- `count(*)`, `count(expr)`, `sum`, `avg`, `min`, `max`, `bool_and`, and
  `bool_or` support applicable Phase 1 and already-landed Phase 2 types.
- Aggregate NULL handling, empty-input results, accumulator types, numeric
  overflow, and result OIDs match PostgreSQL.
- Multiple aggregates and expressions composed from aggregate results work in
  one projection.
- Nested aggregates and illegal aggregate placement return PostgreSQL-compatible
  errors.
- Differential/property tests cover empty/all-NULL/mixed inputs, overflow,
  multiple aggregates, metadata, and ordering over aggregate results.

**Notes:** Statistical, ordered-set, hypothetical-set, and user-defined
aggregates are outside Phase 2.

**Completed:**

- Global aggregation now reduces filtered source rows into one result row,
  including empty inputs and aggregate queries without `FROM`.
- `count(*)`, `count(expr)`, `sum`, `avg`, `min`, `max`, `bool_and`, and
  `bool_or` implement PostgreSQL-compatible signatures, NULL behavior,
  accumulator/result types, metadata OIDs, and numeric/float/interval overflow
  paths for the supported types.
- Aggregate calls compose with scalar expressions, ordering, aliases, limits,
  and scalar subqueries in both directions. Bare columns, nested aggregates,
  aggregates in `WHERE`, and row locking on aggregate queries return the
  PostgreSQL error category.
- Focused native and PostgreSQL differential coverage includes mixed and
  all-NULL inputs, empty filters, overflow, metadata, illegal placement, and
  correlated scalar subqueries. The generated differential suite now emits
  aggregate workloads, and the benchmark registry includes a 100-row global
  aggregate scan.
- The Phase 2 manifest promotes the aggregate case to `MustPass`; the current
  report records 417 matching corpus statements, 141 skipped scripts, and 23
  passing Phase 2 conformance cases with no aggregate blocker.
- `cargo test --workspace`, `cargo check --workspace --all-targets`, and the
  10,000-iteration property suite pass.

### Task 10 — `GROUP BY`, aggregate modifiers, and `HAVING` [COMPLETE]

**Goal:** Partition rows into groups and filter aggregate results.

**DoD:**

- `GROUP BY` accepts columns, expressions, and select-list ordinals with
  PostgreSQL equality and NULL grouping semantics.
- Selected and ordered expressions obey PostgreSQL's grouped-column rules;
  invalid ungrouped references return `42803`.
- `HAVING` executes after grouping and supports aggregate and non-aggregate
  predicates, including the no-`GROUP BY` degenerate case.
- Aggregate `DISTINCT` and `FILTER (WHERE ...)` work for the core aggregate set.
- `HAVING` can contain the correlated subqueries implemented in task 8.
- Grouping keys and aggregate results may be referenced by legal output aliases
  and `ORDER BY` expressions.
- Differential/property cases cover duplicate/NULL keys, expression grouping,
  empty input, grouped errors, `HAVING`, `DISTINCT`, and `FILTER`.

**Notes:** Grouping sets, rollup, and cube remain outside Phase 2.

**Completed:**

- Grouped execution partitions filtered source rows by columns, expressions,
  aliases, and select-list ordinals with PostgreSQL NULL equality, empty-input
  behavior, output ordering, and primary-key functional dependencies.
- Grouped-column validation covers projection, `HAVING`, and `ORDER BY`,
  including correlated `HAVING` subqueries and PostgreSQL `42803` errors for
  ungrouped outer references.
- `HAVING` executes after aggregation for grouped and degenerate global groups;
  core aggregates support `DISTINCT` and `FILTER (WHERE ...)` together.
- Focused PostgreSQL differential tests cover duplicate and NULL keys,
  expression/alias/ordinal grouping, empty input, grouped errors, correlated
  subqueries, primary-key dependencies, aggregate `DISTINCT`, and aggregate
  `FILTER`. The generated differential suite and benchmark registry include
  grouped aggregate workloads.
- The Phase 2 manifest promotes grouping and having to `MustPass`; the reviewed
  report records 442 matching corpus statements, 141 skipped scripts, and 24
  passing Phase 2 conformance cases with no grouping-and-having blocker.
- `cargo test --workspace`, `cargo check --workspace --all-targets`, and the
  10,000-iteration property suite pass.

### Task 11 — `SELECT DISTINCT` and `DISTINCT ON` [COMPLETE]

**Goal:** Eliminate duplicate result rows with PostgreSQL ordering semantics.

**DoD:**

- `SELECT DISTINCT` removes duplicates after projection and before final
  ordering/limiting, using type-correct equality including NULLs.
- PostgreSQL restrictions on `ORDER BY` expressions with `DISTINCT` are
  enforced with matching error codes.
- `DISTINCT ON (expr, ...)` is included as a small PostgreSQL-specific extension
  because it reuses the same machinery and unlocks `select_distinct_on.sql`.
- `DISTINCT ON` validates the leading `ORDER BY` keys and retains the first row
  in requested order.
- Differential/property cases cover duplicate rows, NULLs, mixed types,
  ordering, limits, and metadata.

**Notes:** `DISTINCT ON` is the only deliberate query-surface addition beyond
the Phase 2 list in `spec.md`.

**Completed:**

- `SELECT DISTINCT` removes projected duplicates with PostgreSQL NULL and type
  equality before final ordering, offset, and limit processing.
- `DISTINCT ON` validates its leading order keys, evaluates reusable projected
  and ordered values once, and retains the first row in requested order.
- PostgreSQL-compatible validation covers non-selected `ORDER BY` expressions,
  mismatched `DISTINCT ON` order keys, and row-lock incompatibility with matching
  SQLSTATE categories.
- Focused differential tests cover duplicate and NULL rows, mixed output types,
  aliases and expression ordering, grouped aggregates, limits, errors, and
  result metadata. The generated differential suite and benchmark registry now
  include distinct workloads.
- The Phase 2 manifest promotes distinct queries to `MustPass`; the reviewed
  report records 442 matching corpus statements, 141 skipped scripts, and 25
  passing Phase 2 conformance cases with no distinct blocker.
- `cargo test --workspace`, `cargo check --workspace --all-targets`, and the
  10,000-iteration property suite pass.

---

## Milestone D — Query-producing DML

### Task 12 — `INSERT` / `UPDATE` / `DELETE ... RETURNING` [COMPLETE]

**Goal:** Return affected rows from mutations through both native and SQLx APIs.

**DoD:**

- All three mutation forms support `RETURNING *`, qualified wildcards,
  expressions, and aliases with correct column metadata.
- `INSERT`/`UPDATE` return new row values and `DELETE` returns old row values;
  defaults and assignment coercions are visible in the returned row.
- A failing statement returns no partial result and preserves existing statement
  atomicity and transaction-abort behavior.
- Native `StatementResult` and prepared-statement APIs represent row-producing
  DML without misclassifying it as an affected-count-only statement.
- SQLx `fetch*` works for DML `RETURNING`, while `execute` reports the affected
  count consistently.
- Differential/property tests cover all mutation forms, zero/many affected
  rows, errors, prepared parameters, metadata, and explicit transactions.

**Completed:**

- `INSERT`, `UPDATE`, and `DELETE ... RETURNING` share the bound projection
  machinery used by queries, including `*`, qualified wildcards, expressions,
  aliases, direct-column typmods, and target-table aliases.
- Inserted and updated rows expose their final assigned values; deleted rows
  expose their old values. Zero-row mutations still return described query
  results, and failures discard all mutation and RETURNING work atomically.
- Native execution and prepared APIs classify RETURNING as row-producing. SQLx
  prepared metadata and `fetch*` expose returned rows, while `execute` reports
  the affected-row count.
- Focused PostgreSQL differential tests cover all mutation forms, defaults and
  assignment coercion, zero/many rows, metadata, prepared parameters, explicit
  rollback, and constraint/expression failures. The generated differential
  suite and benchmark registry now include RETURNING workloads.
- The Phase 2 manifest promotes RETURNING to `MustPass`; the reviewed report
  records 442 matching corpus statements, 141 skipped scripts, and 26 passing
  Phase 2 conformance cases with no RETURNING blocker.
- `cargo test --workspace`, `cargo check --workspace --all-targets`, and the
  10,000-iteration property suite pass.

### Task 13 — Query-sourced and joined mutations [COMPLETE]

**Goal:** Reuse Phase 2 row sources in common mutation forms that are called out
by the Phase 1 deferrals and regression corpus.

**DoD:**

- `INSERT INTO ... SELECT ...` applies assignment coercion, defaults, generated
  values, and constraints to every source row atomically.
- `UPDATE ... FROM` and `DELETE ... USING` bind target/source aliases and execute
  join predicates with PostgreSQL ambiguity rules.
- A target row is mutated at most once when the source produces multiple
  matches, matching PostgreSQL's observable behavior.
- All forms compose with `RETURNING`, subqueries, and explicit transactions.
- Differential/property tests cover empty/multi-row sources, duplicate source
  matches, aliasing, constraint failures, rollback, and returned source/target
  values where PostgreSQL permits them.

**Notes:** These forms are adjacent Phase 2 additions enabled by joins and
subqueries. `MERGE` and `ON CONFLICT` remain outside Phase 2.

**Completed:**

- `INSERT INTO ... SELECT ...` materializes the complete source before writing,
  applies destination assignment coercion (including contextually typed string
  literals), fills omitted defaults, and validates row constraints atomically.
- `UPDATE ... FROM` and `DELETE ... USING` share combined target/source scopes,
  PostgreSQL alias and ambiguity resolution, source-aware `RETURNING`, and
  correlated subquery evaluation. Each target selects at most one source match.
- Prepared analysis and result metadata cover source-qualified expressions and
  parameters. Mutation locking conservatively protects target rows when source
  clauses participate.
- Focused differential tests cover empty and multi-row sources, duplicate
  matches, aliases, assignment coercion, source/target return values,
  constraint failures, correlated subqueries, and explicit rollback. Generated
  property actions cover all three forms.
- The Phase 2 manifest promotes query-sourced mutations to `MustPass`; the
  reviewed report records 442 matching corpus statements, 141 skipped scripts,
  and 29 passing conformance cases with no mutation blocker.
- The `update_from_row` benchmark measures about 28.0 us for pg_fake and 98.6 us
  for PostgreSQL 18. `cargo test --workspace`,
  `cargo check --workspace --all-targets`, and the 10,000-iteration property
  suite pass.

---

## Milestone E — Sequences and generated integer values

### Task 14 — Sequence catalog, DDL, and functions [COMPLETE]

**Goal:** Implement PostgreSQL sequence allocation and session-visible sequence
state.

**DoD:**

- Catalog/storage support named sequences independently of MVCC row versions.
- `CREATE SEQUENCE` and `DROP SEQUENCE` support integer type, start, increment,
  min/max, cycle, and cache options with PostgreSQL validation and error codes.
- `nextval`, `currval`, `lastval`, and two-/three-argument `setval` have correct
  first-call, session-local, bounds, and cycle behavior.
- Allocated values are not rolled back, including values consumed by a failed
  statement or aborted transaction.
- Concurrent sessions allocate distinct values; session `currval`/`lastval`
  observations remain isolated.
- Differential and multi-session tests cover options, errors, rollback,
  concurrency, drop/recreate, and prepared calls.

**Notes:** Sequence DDL follows the existing Phase 2 catalog limitations: DDL
inside an explicit transaction remains unsupported until transactional catalog
work in Phase 3. Physical caching is unnecessary, but accepted cache options
must not alter observable allocation. `ALTER SEQUENCE`, including `RESTART`, is
explicitly deferred because `sqlparser-rs` 0.62 and its current upstream parser
do not expose a PostgreSQL sequence-alter AST. It will be added after parser
support exists rather than through a project-local SQL parsing workaround.

**Progress:**

- Named sequence definitions and non-MVCC counters support integer type bounds,
  start, increment, min/max, cache acceptance, cycling, relation-namespace
  checks, implicit-batch DDL rollback, and nontransactional allocation.
- `nextval`, `currval`, `lastval`, and both `setval` forms support session-local
  observations, first-call behavior, bounds, rollback gaps, drop/recreate
  identity, prepared calls, SQLx bigint metadata, and concurrent uniqueness.
- Differential, multi-session, failed-statement, generated-model, and benchmark
  coverage is in place. The regression audit reaches 448 matching statements,
  retains 141 skipped scripts, and passes 30 Phase 2 conformance cases; the
  `sequence.sql` blocker advances from statement 1 to statement 7 (`OWNED BY`,
  assigned to Task 15). The `nextval` benchmark measures about 17.4 us for
  pg_fake and 26.4 us for PostgreSQL 18.

### Task 15 — `SERIAL` variants and identity columns [COMPLETE]

**Goal:** Build PostgreSQL's common sequence-backed column declarations on the
sequence engine.

**DoD:**

- `smallserial`/`serial2`, `serial`/`serial4`, and `bigserial`/`serial8` create
  the correct integer column, owned sequence, NOT NULL property, and default.
- Generated sequence names, ownership, drop behavior, explicit inserted values,
  and `pg_get_serial_sequence` match PostgreSQL for the supported default schema.
- `GENERATED ALWAYS` and `GENERATED BY DEFAULT AS IDENTITY` support sequence
  options and the legal `OVERRIDING SYSTEM/USER VALUE` insert behavior.
- Invalid type/default/duplicate identity declarations return matching error
  codes.
- SQLx metadata exposes the underlying integer OID and generated values decode
  normally.
- Differential/property cases cover serial variants, identities, explicit and
  default values, ownership cleanup, rollback consumption, and `RETURNING`.

**Notes:** Identity support is included because the catalog specification treats
it as the other sequence-backed column form and it reuses nearly all `SERIAL`
machinery.

`OVERRIDING SYSTEM VALUE` and `OVERRIDING USER VALUE`, along with identity
declarations containing more than one sequence option, are pending upstream
`sqlparser-rs` AST/parser additions. Version 0.62 parses identity declarations
but cannot represent those valid PostgreSQL `INSERT` clauses and only parses a
single identity sequence option. This project does not add local SQL parsing
workarounds.

**Progress:**

- `smallserial`/`serial2`, `serial`/`serial4`, and `bigserial`/`serial8` now
  create not-null integer columns backed by generated, owned sequences.
- `GENERATED ALWAYS` and `GENERATED BY DEFAULT AS IDENTITY` use the same owned
  sequence machinery, including accepted single parser-provided sequence
  options, generated defaults, explicit `BY DEFAULT` values, and `428C9` for
  disallowed explicit `ALWAYS` values.
- `CREATE SEQUENCE ... OWNED BY`, `pg_get_serial_sequence`, direct-drop
  dependency protection, table-drop cleanup, and implicit-batch DDL rollback
  are implemented and covered by focused native tests.
- `cargo test --workspace`, `cargo test -p pg_fake --test sequences`, and
  `cargo check --workspace --all-targets` pass. Differential/property coverage,
  SQLx-specific coverage, benchmarks, and the parser-limited insert clauses are
  explicitly deferred to a future parser update.

---

## Milestone F — Foreign keys

### Task 16 — Foreign-key metadata and immediate enforcement [COMPLETE]

**Goal:** Define foreign keys and enforce the default immediate referential
integrity rules.

**DoD:**

- Column- and table-level `REFERENCES`/`FOREIGN KEY` definitions support named
  constraints, single/composite keys, explicit referenced columns, and the
  referenced primary-key default.
- Creation validates table/column existence, arity, compatible types, and a
  referenced unique/primary key with PostgreSQL-compatible errors.
- INSERT/UPDATE of the referencing row checks the referenced key; NULL handling
  implements `MATCH SIMPLE` and `MATCH FULL`.
- Missing referenced rows return `23503`; statements remain atomic for
  multi-row writes and `INSERT ... SELECT`.
- Constraint metadata records table dependencies so unsupported drops fail
  loudly rather than leaving dangling references.
- Differential/property tests cover valid/missing keys, composite keys, NULL
  combinations, self-reference, errors, rollback, and `RETURNING`.

### Task 17 — Foreign-key actions and concurrency [COMPLETE]

**Goal:** Apply referential actions safely when referenced keys change.

**DoD:**

- `ON DELETE` and `ON UPDATE` implement `NO ACTION`, `RESTRICT`, `CASCADE`,
  `SET NULL`, and `SET DEFAULT` for single and composite keys.
- Cascades recurse through multiple foreign keys, terminate on cycles, and
  validate all resulting not-null, check, unique, and foreign-key constraints.
- The statement is atomic if any action or downstream constraint fails.
- Row/key locking prevents concurrent inserts, deletes, or key updates from
  committing a referential-integrity violation; waits, timeouts, and deadlocks
  use the existing transaction machinery.
- READ COMMITTED and REPEATABLE READ behavior is validated against PostgreSQL
  with controlled multi-session tests.
- Differential/property tests cover every action, chains, cycles, self-reference,
  rollback, and concurrent races.

### Task 18 — Deferred foreign-key checking [COMPLETE]

**Goal:** Complete standard PostgreSQL foreign-key timing semantics.

**DoD:**

- `DEFERRABLE`, `NOT DEFERRABLE`, `INITIALLY IMMEDIATE`, and `INITIALLY DEFERRED`
  are stored and validated on foreign-key definitions.
- Deferred violations are tracked per transaction and checked against the final
  transaction-visible state at `COMMIT`.
- `SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED | IMMEDIATE }` changes
  checking mode and immediately validates constraints switched to immediate.
- Commit-time failure aborts the transaction with `23503` and releases all
  locks/waiters correctly.
- Differential and multi-session tests cover temporarily broken references,
  repair before commit, immediate switching, rollback, cascades, and commit-time
  failure.

**Notes:** Savepoints remain Phase 3. PostgreSQL 18 `NOT ENFORCED`/`ENFORCED`,
`NOT VALID`, partitioned foreign keys, and permission checks are outside this
phase.

---

## Milestone G — UUID and temporal types

### Task 19 — UUID type [COMPLETE]

**Goal:** Add PostgreSQL UUID storage and common operations end to end.

**DoD:**

- `BaseType`, `PgType`, and `Value` support UUID with OID 2950 and canonical
  lowercase text output.
- PostgreSQL-accepted hyphenated, braced, and compact input forms parse; invalid
  forms return `22P02`.
- Equality, ordering, casts to/from text, coercion of unknown literals,
  uniqueness, grouping, joins, parameters, and `RETURNING` work.
- `gen_random_uuid()` and its `uuidv4()` alias are included as small core
  compatibility additions; generated values are distinct and correctly marked
  volatile.
- SQLx encode/decode/type metadata works for UUID values.
- Differential/property cases are derived from the applicable portion of
  `uuid.sql`, including format, ordering, invalid input, joins, aggregation, and
  generation.

**Notes:** UUIDv7 generation/extraction may be added only if it remains a small
extension after the base type lands; it is not a Phase 2 exit requirement.

### Task 20 — Date and time types [COMPLETE]

**Goal:** Add PostgreSQL `date` and `time without time zone` values with their
basic semantics.

**DoD:**

- Wrapped date/time representations preserve PostgreSQL range, microsecond
  precision, typmod rounding, `24:00:00`, infinity where applicable, and BC
  dates rather than exposing raw `chrono` behavior.
- OIDs, canonical ISO text I/O, typed literals, common unambiguous PostgreSQL
  input forms, invalid-input/overflow errors, text casts, comparisons, ordering,
  grouping, and uniqueness match PostgreSQL.
- Casts between each type and text use the central coercion rules in the
  directions PostgreSQL permits.
- `extract`/`date_part` support the fields applicable to date and time.
- SQLx encode/decode and metadata work for both types.
- Differential/property cases cover boundaries, leap years, BC dates, typmods,
  `24:00`, invalid values, ordering, and extraction.

**Notes:** `time with time zone` (`timetz`) is not listed in the Phase 2 type
table and remains unsupported.

### Task 21 — Timestamp, timestamptz, and session timezone [COMPLETE]

**Goal:** Add timestamp values and PostgreSQL timezone interpretation.

**DoD:**

- Wrapped timestamp/timestamptz representations support PostgreSQL range,
  microsecond precision, typmod rounding, epoch, infinity, and BC values.
- Timestamp without time zone ignores a supplied zone after validating it;
  timestamptz stores an instant and renders it in the session `TimeZone`.
- Numeric offsets, UTC, and IANA timezone names handle daylight-saving gaps and
  overlaps like PostgreSQL for the tested range.
- `SET [LOCAL] TIME ZONE`, `SHOW TimeZone`, transaction-local restoration, and
  prepared statements use session timezone state; strict mode does not treat
  timezone changes as no-ops.
- Casts among date, timestamp, timestamptz, and text plus `AT TIME ZONE` work in
  the supported directions.
- SQLx encode/decode and metadata work for timestamp and timestamptz.
- Differential/property cases cover offsets, named zones, DST boundaries,
  infinity/BC, typmods, casts, ordering, and session/transaction state.

### Task 22 — Interval type and temporal arithmetic [COMPLETE]

**Goal:** Add PostgreSQL's three-part interval value and the common temporal
operator matrix.

**DoD:**

- `Interval` stores months, days, and microseconds separately, with checked
  bounds and PostgreSQL-compatible equality/ordering behavior.
- Common PostgreSQL interval text forms, typed literals, typmods, canonical
  output, infinity if supported by PostgreSQL 18, and invalid-input errors are
  covered differentially.
- Unary negation; interval addition/subtraction; multiply/divide by numeric; and
  date/time/timestamp/timestamptz plus/minus interval follow PostgreSQL calendar
  and daylight-saving rules.
- Subtracting temporal values returns the correct integer or interval type.
- `extract`/`date_part`, `justify_days`, `justify_hours`, and
  `justify_interval` support interval values.
- SQLx encode/decode and metadata work for interval.
- Differential/property cases cover mixed-sign fields, month ends, leap years,
  DST transitions, overflow, infinity, ordering, and arithmetic identities that
  PostgreSQL guarantees.

### Task 23 — Clock hierarchy and deterministic time control [COMPLETE]

**Goal:** Implement the public mock-time API and PostgreSQL transaction/
statement/clock timestamp hierarchy from `spec.md` §1.4.

**DoD:**

- `DbBuilder` selects real or mock time; mock time is frozen until changed.
- The public control surface is exactly `db.set_time(t)` and
  `db.advance_time(duration)`, and both reject use when mock time is disabled.
- `now()`/`transaction_timestamp()` are fixed at transaction start,
  `statement_timestamp()` is fixed at statement start, and
  `clock_timestamp()` reads the current clock.
- `current_date`, `current_time`, `localtime`, `localtimestamp`, and SQL typed
  current-time expressions use the appropriate timestamp and session timezone.
- Autocommit, explicit transactions, prepared statements, multiple statements,
  and concurrent sessions capture times at the correct boundaries.
- Deterministic tests advance mock time between and during transaction/
  statement boundaries; focused real-clock differential tests compare
  relationships rather than exact instants.

**Notes:** The internal clock abstraction is not public. Sleep functions are not
required.

---

## Milestone H — Phase 2 conformance and release gate

### Task 24 — Phase 2 integration, regression audit, and benchmarks [COMPLETE]

**Goal:** Prove that the complete Phase 2 surface works coherently through the
native and SQLx APIs.

**DoD:**

- The Phase 2 conformance manifest from task 1 passes completely against
  PostgreSQL 18, including results, multiplicity/order, metadata, and SQLSTATE.
- The full embedded regression test passes with an updated explicit skip list;
  the handoff reports new passed-statement and skipped-script counts against the
  248/141 baseline and categorizes remaining blockers as fixture-related,
  Phase 3, or later/out of scope.
- End-to-end differential scenarios combine joins, correlated subqueries,
  grouping, `DISTINCT`, sequences/identity, `RETURNING`, foreign keys, UUID, and
  temporal values inside explicit transactions.
- SQLx prepared queries and transactions cover every Phase 2 type and every
  row-producing statement family.
- The property suite passes 10,000 iterations with Phase 2 operations enabled,
  and focused multi-session concurrency tests pass repeatedly.
- Benchmarks cover joins, grouping, subqueries, sequence-backed inserts,
  foreign-key writes, and temporal/UUID workloads; the report compares
  PostgreSQL 18 and records any workload that misses the project's order-of-
  magnitude speed target.
- The unsupported-feature registry and user-facing feature documentation are
  updated to distinguish completed Phase 2 behavior from Phase 3/later gaps.

**Notes:** This task fixes integration defects but does not add new SQL families.

**Completed:**

- The PostgreSQL 18 Phase 2 manifest passes all 32 cases, including the new
  explicit-transaction scenario that combines joins, correlated subqueries,
  grouping, `DISTINCT`, identity allocation, `RETURNING`, a foreign key, UUID,
  temporal values, and an interval.
- The full embedded regression audit now passes 463 statements (up from the
  248-statement Phase 1 baseline) with 141 explicitly reviewed skipped scripts,
  unchanged from the baseline. Fixture-related skips are platform collation,
  encoding, and COPY corpus behavior. Phase 3 skips include `WITH`, window
  functions, arrays, JSON/JSONPath, `ON CONFLICT`, views, rules, and stored
  generated columns. The remaining skips cover later/out-of-scope types,
  extensions, DDL, procedures, locking, and transaction facilities. The few
  Phase-2-adjacent corpus cases use excluded forms or fixture formatting; the
  conformance manifest has no Phase 2 blockers. In particular, identity
  `OVERRIDING ... VALUE` remains parser-limited as recorded in task 15.
- Existing native and SQLx prepared-query/transaction coverage exercises all
  Phase 2 types and row-producing statement families. A case-insensitive text
  sort with PostgreSQL-compatible case tie-breaking was fixed after the
  differential generator found an ordering difference.
- The PostgreSQL differential property suite passes 10,000 iterations in
  138.61 seconds. The focused multi-session sequence allocation test also
  passed three consecutive runs.
- Benchmarks now include identity-backed `INSERT ... RETURNING` and a
  UUID/timestamp/interval lookup. Against PostgreSQL 18, the recorded medians
  are 19.60 us versus 261.98 us and 81.45 us versus 259.10 us respectively;
  neither misses the order-of-magnitude target. Existing benchmark workloads
  cover joins, grouping, subqueries, and foreign-key writes.
- `README.md` distinguishes the completed Phase 2 surface from Phase 3/later
  gaps, and the explicit regression skip registry was refreshed.

---

## Phase 2 exit criteria

- All 24 tasks meet their DoD and have been approved before being marked
  complete.
- The Phase 2 conformance manifest passes against PostgreSQL 18.
- The full regression corpus shows no regression from the Phase 1 baseline and
  all remaining blockers are explicitly classified as out of Phase 2 or fixture
  limitations.
- Joins, subqueries, grouping/aggregates, `DISTINCT`, sequences/`SERIAL`/
  identity, `RETURNING`, foreign keys, UUID, and temporal types work through
  both the native API and SQLx.
- Tier-A behavior for the supported Phase 2 surface matches PostgreSQL for
  results, NULL semantics, constraints, transactions, sequence allocation, and
  SQLSTATE.
- Phase 3 remains unchanged: CTEs, recursive queries, `ON CONFLICT`, window
  functions, views, JSON/JSONB, arrays, savepoints, general session GUCs,
  SERIALIZABLE isolation, and transactional DDL are not pulled into this phase.
