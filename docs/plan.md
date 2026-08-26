# pg_fake — Phase 3 Implementation Plan

Phase 3 delivers the scope from `spec.md` §9: CTEs (including recursive and
data-modifying forms), `INSERT ... ON CONFLICT`, window functions, views,
JSON/JSONB, arrays, savepoints, general session GUC handling, the remaining
`SELECT ... FOR UPDATE` family, SERIALIZABLE isolation, and transactional DDL.
Set operations are included as an adjacent prerequisite for recursive CTEs.

The plan is a linear sequence of small tasks. Each task has a **Goal**, a
testable **Definition of Done (DoD)**, and **Notes** describing dependencies and
scope boundaries. The completed Phase 2 plan is archived in
`plan_phase2_complete.md`.

## Conventions

- "Differential test" means running the same SQL against PostgreSQL 18 and
  `pg_fake`, comparing result values, row multiplicity, ordered results when
  requested, column metadata, transaction outcomes, and `SQLSTATE`.
- Phase 3 starts from the reviewed Phase 2 baseline of 463 matching embedded
  regression statements, 141 skipped scripts, and 32 passing Phase 2
  conformance cases.
- Task 1 creates a Phase 3 manifest and records the first blocker for every
  Phase 3 feature. Upstream PostgreSQL SQL files remain unmodified; local
  fixtures may supply prerequisites that the corpus normally obtains from
  earlier out-of-scope statements.
- Every feature task must extend the property-based differential generator when
  its behavior can be generated meaningfully. Otherwise, the completion handoff
  must explain why and provide focused differential coverage.
- Every feature task must add or update a representative benchmark when it can
  materially affect common query, mutation, catalog, or concurrency workloads.
  Otherwise, the completion handoff must explain why.
- Before a task can be marked complete, the property suite must pass at least
  10,000 `chaos_theory` iterations with
  `CHAOS_THEORY_CHECK_ITERS=10000 CHAOS_THEORY_CHECK_TIME=600s cargo test -p pg_fake_sqlx --test property_tests`.
- After implementing a task, update its progress but do not mark it complete
  until the user approves the result, as required by `AGENTS.md`.
- Valid PostgreSQL syntax that `sqlparser-rs` cannot represent must be fixed in
  the parser dependency rather than recognized by project-local parsing.
- Features not listed here retain the unsupported-feature behavior from
  `spec.md` §10. Optimization-only clauses may be tolerated only when ignoring
  them cannot change Tier-A behavior, and strict mode must still reject them.

## Phase 3 regression focus

The primary upstream source files for this phase are:

- CTEs and set operations: `with.sql`, `union.sql`, and applicable cases from
  `subselect.sql`;
- conflicting inserts: `insert_conflict.sql`;
- windows: `window.sql`;
- JSON types: `json.sql` and `jsonb.sql`;
- arrays: `arrays.sql`;
- views and catalog behavior: `create_view.sql` and applicable cases from
  `updatable_views.sql`;
- transactions, savepoints, GUCs, and row locking: `transactions.sql`,
  `lock.sql`, applicable `SET`/`SHOW` statements elsewhere in the embedded
  corpus, and focused local multi-session scenarios where the upstream corpus
  cannot express controlled interleavings.

Passing an entire source file is not a per-task requirement because those files
exercise extensions, privileges, procedural code, DDL, types, and planner
behavior beyond Phase 3. Each task instead promotes relevant statements into
the Phase 3 conformance set and records newly exposed blockers.

The Phase 3 surface is intentionally bounded:

- views are ordinary read-only views; automatic view updates, materialized
  views, recursive views, rules, triggers, privileges, and security-barrier or
  security-invoker behavior remain later work;
- JSONPath and SQL/JSON constructors and query functions are outside this phase;
- arrays cover one-dimensional arrays of supported scalar element types with
  the standard lower bound of one; multidimensional arrays, non-default lower
  bounds, arrays of composite/domain types, and array slices remain later work;
- the GUC registry models settings that affect supported behavior or are emitted
  by supported drivers. It does not attempt PostgreSQL's full server-wide
  configuration catalog;
- `MERGE`, grouping sets, table functions, `LATERAL`, stored generated columns,
  triggers, and new general-purpose `ALTER TABLE` families are not pulled into
  Phase 3.

---

## Milestone A — Conformance baseline and set operations

### Task 1 — Phase 3 regression manifest and progress reporting

**Progress:** Complete (approved).

**Goal:** Turn the embedded PostgreSQL corpus and focused multi-session cases
into an actionable Phase 3 scorecard.

**DoD:**

- A checked-in manifest maps every Phase 3 feature to focused cases, including
  the upstream source and statement location where applicable.
- The regression runner reports full-corpus matching statements, skipped
  scripts, Phase 2 and Phase 3 conformance totals, and the first blocker for
  every Phase 3 feature.
- The baseline records 463 matching statements, 141 skipped scripts, and all 32
  Phase 2 conformance cases as non-regressing requirements.
- Transactional and concurrency features that cannot be represented by a
  single SQL script have named local scenarios in the same progress report.
- Fixture limitations, parser limitations, Phase 4/later features, and true
  implementation blockers are categorized separately.

**Notes:** This task changes test infrastructure only. It does not make the full
upstream corpus a Phase 3 exit criterion.

### Task 2 — Query set operations

**Progress:** Complete (approved).

**Goal:** Add the set-expression machinery required by recursive CTEs and common
PostgreSQL query composition.

**DoD:**

- `UNION`, `UNION ALL`, `INTERSECT`, `INTERSECT ALL`, `EXCEPT`, and `EXCEPT ALL`
  execute with PostgreSQL duplicate and NULL semantics.
- Input columns use PostgreSQL common-type resolution and coercion, reject
  mismatched arity, and expose names and metadata from the correct input.
- Operator precedence, parentheses, and top-level `ORDER BY`, `LIMIT`, and
  `OFFSET` match PostgreSQL for the supported query forms.
- Set operands may be `SELECT`, `VALUES`, derived queries, or already-supported
  subqueries and may contain parameters and aggregates.
- Differential/property cases cover duplicates, NULLs, empty inputs, coercion,
  metadata, associativity-sensitive syntax, and error codes.
- The benchmark suite includes `UNION ALL` and duplicate-eliminating `UNION`
  workloads.

---

## Milestone B — Common table expressions

### Task 3 — Non-recursive CTEs

**Progress:** Complete (approved).

**Goal:** Bind and execute named query results within one statement.

**DoD:**

- `WITH name AS (<query>)` supports one or more CTEs, explicit output-column
  lists, forward-reference errors, and references from the main query and later
  CTEs.
- CTE names obey PostgreSQL scope, shadowing, qualification, quoted-identifier,
  ambiguity, and output-metadata rules.
- A CTE may be referenced multiple times, including through joins and nested
  subqueries, without leaking state across statements or sessions.
- PostgreSQL's statement-level evaluation behavior is preserved for volatile
  expressions and side effects; `MATERIALIZED` and `NOT MATERIALIZED` are
  accepted only with semantics that cannot change Tier-A observations.
- CTEs compose with parameters, prepared statements, aggregation, `DISTINCT`,
  and set operations.
- Differential/property cases cover dependency chains, repeated references,
  nested scopes, name shadowing, volatility, metadata, and invalid recursion.

### Task 4 — Recursive CTEs

**Progress:** Complete (approved).

**Goal:** Execute bounded recursive queries using PostgreSQL's working-table
semantics.

**DoD:**

- `WITH RECURSIVE` supports a non-recursive seed combined with a recursive term
  through `UNION ALL` or duplicate-eliminating `UNION`.
- Each iteration sees only the current working set, appends the correct rows to
  the result, and terminates when no new working rows remain.
- Recursive names resolve only where PostgreSQL permits them; multiple recursive
  references, outer-join placement, mutual recursion, and type/arity errors use
  PostgreSQL-compatible `SQLSTATE` values.
- Column types and metadata are fixed from PostgreSQL's recursive-union type
  resolution rather than changing across iterations.
- Recursive CTEs compose with joins, subqueries, arrays, and final ordering and
  limiting once those dependencies land.
- Differential/property cases cover trees, graphs with explicit cycle guards,
  `UNION` cycle elimination, empty seeds, NULLs, coercion, and invalid forms.
- The benchmark suite includes a numeric series and a branching traversal.

**Notes:** `SEARCH` and `CYCLE` clauses are outside this phase. No arbitrary
recursion-depth limit is introduced because PostgreSQL does not impose one.

### Task 5 — Data-modifying CTEs

**Goal:** Allow a top-level statement to consume rows returned by mutations in
its `WITH` clause.

**DoD:**

- Non-recursive CTE bodies may be `INSERT`, `UPDATE`, or `DELETE ... RETURNING`
  where PostgreSQL permits data-modifying CTEs.
- Every data-modifying CTE executes exactly once to completion even when its
  result is referenced zero or multiple times.
- Main-query and sibling-CTE visibility follows PostgreSQL's single-snapshot
  rules; communication between mutations occurs through `RETURNING`, not by
  observing sibling writes directly.
- Multiple mutations, constraints, sequences, deferred foreign keys, and a
  failing substatement preserve statement atomicity and transaction-abort
  behavior.
- Prepared metadata and SQLx row production work for the main statement and
  CTE-returned columns.
- Differential tests cover unreferenced and multiply referenced mutations,
  conflicting writes, rollback, errors, and execution order observables.

---

## Milestone C — Conflicting inserts

### Task 6 — `ON CONFLICT DO NOTHING` and arbiter inference

**Goal:** Skip conflicting proposed rows using PostgreSQL unique-index inference.

**DoD:**

- `INSERT ... ON CONFLICT DO NOTHING` works with no target, a column/expression
  target supported by existing indexes, or `ON CONSTRAINT` for named unique and
  primary-key constraints.
- Arbiter inference applies PostgreSQL column order, predicate, collation, and
  operator-class validation for the subset represented by the catalog; requests
  for unsupported index features fail loudly.
- Multi-row inserts independently accept or skip proposed rows while preserving
  statement atomicity for non-conflict errors.
- `RETURNING` contains only inserted rows, and sequence/default values consumed
  by skipped proposals are not rolled back.
- Concurrent transactions cannot both commit rows that violate the arbiter;
  waiting, rollback, and committed-conflict behavior match PostgreSQL.
- Differential/property and multi-session tests cover every target form,
  duplicate proposals, NULL keys, partial success, metadata, and errors.

### Task 7 — `ON CONFLICT DO UPDATE`

**Goal:** Update the conflicting row using the proposed row exposed through
`excluded`.

**DoD:**

- `DO UPDATE SET ...` binds target columns, target aliases, and `excluded`
  columns with PostgreSQL ambiguity and qualification rules.
- The optional conflict-action `WHERE` predicate controls whether the locked
  conflicting row is updated; omitted updates do not appear in `RETURNING`.
- Assignment coercion, generated/default values, checks, unique constraints,
  foreign keys, cascades, and `RETURNING` run through the existing mutation
  paths atomically.
- A command that would affect the same existing row twice returns PostgreSQL's
  cardinality-violation code, and a second unique conflict during the update
  returns the matching constraint error.
- Concurrent insert/update races lock and recheck the current conflicting row
  with correct READ COMMITTED and REPEATABLE READ outcomes.
- Differential/property and controlled multi-session cases cover `excluded`,
  predicates, aliases, secondary conflicts, duplicate source rows, rollback,
  and prepared parameters.
- The benchmark suite includes conflict-free, `DO NOTHING`, and `DO UPDATE`
  inserts.

---

## Milestone D — Window functions

### Task 8 — Window binding, partitioning, ordering, and ranking

**Goal:** Add a window execution stage and the ranking functions that establish
its core row-set semantics.

**DoD:**

- `OVER (...)` binds `PARTITION BY` and window `ORDER BY` expressions using the
  input scope, while final projection aliases and final ordering obey
  PostgreSQL visibility rules.
- Named `WINDOW` clauses support inheritance and reject cycles, overrides, and
  illegal references with PostgreSQL-compatible errors.
- `row_number`, `rank`, `dense_rank`, `percent_rank`, `cume_dist`, and `ntile`
  implement peer-group, NULL-ordering, empty-partition, and result-type behavior.
- Multiple compatible and incompatible window specifications in one query
  produce correct results without changing the query's output row count.
- Window functions work after grouping and `HAVING` but before final
  `DISTINCT`, ordering, and limiting; illegal placement and nesting are rejected.
- Differential/property cases cover partitions, peers, NULLs, named windows,
  aggregates as inputs, clause ordering, metadata, and errors.

### Task 9 — Offset and value window functions

**Goal:** Add position-sensitive access to rows within a partition or frame.

**DoD:**

- `lag`, `lead`, `first_value`, `last_value`, and `nth_value` support their
  PostgreSQL argument, default, type-coercion, and NULL behavior.
- Offset and default expressions are evaluated with PostgreSQL timing and may
  use parameters and volatile scalar expressions where legal.
- Functions distinguish the partition from the active frame exactly as
  PostgreSQL does (`lag`/`lead` use the partition; value functions use the
  frame).
- Unsupported `RESPECT NULLS`/`IGNORE NULLS` and `FROM FIRST`/`FROM LAST`
  syntax is rejected rather than silently ignored.
- Differential/property cases cover partition edges, dynamic and invalid
  offsets, defaults, all-NULL inputs, peer groups, metadata, and errors.

### Task 10 — Aggregate windows and frame semantics

**Goal:** Run existing aggregates over PostgreSQL window frames.

**DoD:**

- Core aggregate functions execute as windows with and without `PARTITION BY`
  and window ordering, preserving aggregate result types and NULL behavior.
- `ROWS`, `RANGE`, and `GROUPS` frames support unbounded, current-row, and legal
  preceding/following bounds, including validated typed offsets.
- Default frames and peer handling match PostgreSQL, including the common
  `last_value` behavior under an ordered default frame.
- `EXCLUDE CURRENT ROW`, `EXCLUDE GROUP`, `EXCLUDE TIES`, and `EXCLUDE NO OTHERS`
  are implemented where the parser represents them.
- Frame-bound errors, illegal ordering requirements, nested windows, and
  unsupported aggregate modifiers return PostgreSQL-compatible errors.
- Differential/property cases cover empty/singleton partitions, peers, NULLs,
  every frame mode and exclusion, moving aggregates, and final filtering/order.
- The benchmark suite includes ranking and moving-window aggregate workloads.

---

## Milestone E — JSON, JSONB, and arrays

### Task 11 — JSON type and text fidelity

**Goal:** Add PostgreSQL `json` storage while preserving its textual nature.

**DoD:**

- `BaseType`, `PgType`, and `Value` support `json` with OID 114 through native
  and SQLx APIs.
- Assignment and explicit casts validate JSON while preserving insignificant
  whitespace, object-key order, duplicate keys, numeric spelling, and canonical
  text output behavior expected from PostgreSQL `json`.
- Unknown literals, text casts, parameters, defaults, constraints, joins where
  legal, and `RETURNING` use central coercion and metadata paths.
- Invalid documents, unsupported Unicode escapes, nesting, and numeric edge
  cases return PostgreSQL-compatible error codes.
- Operations PostgreSQL does not define for `json`, including ordinary equality
  and ordering, remain rejected.
- Differential/property cases are derived from the applicable portion of
  `json.sql` and include round trips, duplicate keys, Unicode, numbers, NULL,
  metadata, and errors.

### Task 12 — JSONB representation and comparison

**Goal:** Add normalized `jsonb` values with PostgreSQL equality and ordering.

**DoD:**

- `BaseType`, `PgType`, and `Value` support `jsonb` with OID 3802 through native
  and SQLx APIs.
- Input normalization removes insignificant whitespace, applies PostgreSQL
  duplicate-key rules, normalizes numeric values, and produces compatible text
  output independent of object insertion order.
- Equality, ordering, hashing/equivalence used by unique constraints,
  `DISTINCT`, grouping, set operations, joins, and window partitions match
  PostgreSQL for supported values.
- Casts among `json`, `jsonb`, and text follow central coercion rules and retain
  each type's distinct fidelity guarantees.
- Differential/property cases cover every JSON value kind, object key ordering,
  duplicate keys, numeric normalization, nesting, comparisons, constraints,
  metadata, and errors.

### Task 13 — Core JSON and JSONB operators and functions

**Goal:** Make JSON values useful for common application queries and mutations.

**DoD:**

- Field and path extraction supports `->`, `->>`, `#>`, and `#>>` with correct
  missing-path, negative-index, scalar, and SQL-NULL versus JSON-null behavior.
- JSONB containment and existence support `@>`, `<@`, `?`, `?|`, and `?&` with
  PostgreSQL recursive containment semantics.
- JSONB mutation supports concatenation and deletion operators represented by
  the parser, including key, index, and path deletion.
- Common functions include `json_typeof`, `jsonb_typeof`, array length,
  object/array expansion, `json_build_array`/`jsonb_build_array`,
  `json_build_object`/`jsonb_build_object`, `to_json`/`to_jsonb`, and
  `jsonb_set` for supported scalar and array inputs.
- Set-returning JSON functions are accepted only in query positions supported
  by the executor; unsupported table-function placement fails loudly.
- Differential/property cases cover nesting, missing paths, negative indexes,
  containment, duplicate keys, JSON nulls, SQL NULLs, mutations, and errors.
- The benchmark suite includes JSONB extraction and containment workloads.

**Notes:** JSONPath operators/functions, SQL/JSON constructors, JSON_TABLE,
indexes, and record-population functions remain later work.

### Task 14 — One-dimensional array type and I/O

**Goal:** Add one-dimensional arrays of supported scalar PostgreSQL types.

**DoD:**

- `BaseType`, `PgType`, and `Value` represent arrays with an element type and
  ordered values, including SQL NULL elements and empty arrays.
- Array OIDs and element OIDs are correct for every supported scalar element
  type through native prepared statements and SQLx encode/decode paths.
- Array literals, `ARRAY[...]`, text input/output, parameters, defaults, casts,
  and common-element type resolution match PostgreSQL for the supported subset.
- Equality and lexicographic ordering support constraints, joins, grouping,
  `DISTINCT`, set operations, and window partition/order keys.
- Ragged, multidimensional, non-default-lower-bound, incompatible-element, and
  malformed inputs fail explicitly with compatible error categories.
- Differential/property cases cover empty/all-NULL/mixed arrays, escaping,
  coercion, every supported element family, comparison, metadata, and errors.

### Task 15 — Array subscripting, operators, aggregates, and functions

**Goal:** Support common PostgreSQL array expressions end to end.

**DoD:**

- One-based scalar subscripting reads and assigns array elements where
  PostgreSQL permits it; out-of-range reads and assignment expansion match the
  supported one-dimensional semantics.
- Concatenation, containment, and overlap operators (`||`, `@>`, `<@`, `&&`)
  implement PostgreSQL duplicate, ordering, and NULL behavior.
- `ANY(array)` and `ALL(array)` integrate with the existing quantified
  comparison machinery, including empty and NULL arrays.
- Common functions include `array_length`, `cardinality`, `array_lower`,
  `array_upper`, `array_append`, `array_prepend`, `array_cat`, `array_position`,
  `array_positions`, `array_remove`, `array_replace`, and `array_agg`.
- `unnest` is supported in query positions already modeled by the executor;
  unsupported table-function and multi-array forms fail loudly.
- Differential/property cases cover bounds, empty arrays, NULL arrays/elements,
  duplicates, coercion, assignments, quantified comparisons, aggregation, and
  errors.
- The benchmark suite includes array containment and `array_agg` workloads.

---

## Milestone F — Transactional catalog and views

### Task 16 — MVCC-versioned catalog foundation

**Goal:** Make relation metadata obey the same snapshot boundaries as table
rows without duplicating transaction machinery.

**DoD:**

- Schemas, tables, sequences, constraints, and later views have stable internal
  identities and MVCC-visible catalog versions with creating and retiring
  transaction identities.
- Catalog name resolution uses the statement or transaction snapshot plus the
  current transaction's own changes, including drop-and-recreate of the same
  name.
- Uncommitted catalog changes are invisible to other sessions and become
  visible atomically with row changes at commit.
- Abort and garbage collection reclaim unreachable catalog versions without
  losing relation dependencies, sequence ownership, row storage, or prepared
  metadata safety.
- Existing autocommit and implicit multi-statement DDL behavior does not regress.
- Unit/property and controlled multi-session tests cover visibility, name reuse,
  concurrent snapshots, abort, dependency identity, and GC horizons.

### Task 17 — Transactional DDL for the supported catalog surface

**Goal:** Allow existing DDL operations inside explicit transactions with
PostgreSQL commit and rollback semantics.

**DoD:**

- Supported `CREATE` and `DROP` operations for tables and sequences may run in
  explicit transactions and roll back without leaking metadata, rows, indexes,
  constraints, or ownership changes.
- A transaction can query and mutate a table it created, stops resolving a
  table it dropped, and can roll back either change while concurrent sessions
  retain their snapshot-correct view.
- DDL followed by statement failure, transaction abort, or a later savepoint
  rollback restores the correct catalog and storage state.
- Conflicting concurrent relation creation/drop is serialized and reports
  PostgreSQL-compatible duplicate/undefined/dependency errors.
- Prepared statements analyzed before relevant DDL either continue with a
  valid stable identity or fail with a deliberate PostgreSQL-compatible error;
  they never silently target a replacement relation.
- Sequence values remain nontransactional even when their sequence definition
  is created or dropped transactionally.
- Differential and multi-session tests cover create/use/drop, rollback, name
  reuse, dependencies, prepared statements, and implicit versus explicit DDL.

**Notes:** This task makes the already-supported DDL surface transactional. It
does not add broad `ALTER TABLE`, schema, index, or type-definition families.

### Task 18 — Ordinary views

**Goal:** Store named query definitions in the catalog and expand them with
PostgreSQL scope and metadata behavior.

**DoD:**

- `CREATE VIEW`, `CREATE OR REPLACE VIEW`, and `DROP VIEW` support qualified
  names, explicit column lists, replacement validation, `IF EXISTS`, and
  dependency-aware errors.
- Selecting from a view binds its stored query with the caller's snapshot and
  session settings while preserving view-column names, types, typmods, aliases,
  and nested view scopes.
- Views compose with joins, CTEs, subqueries, aggregation, windows, JSON, arrays,
  prepared statements, and other views.
- View definitions are transactional and follow catalog snapshot visibility;
  dropping referenced objects or a referenced view fails rather than leaving a
  dangling definition.
- Recursive view dependencies are detected, and parameters or temporary
  caller-only names cannot leak into stored definitions.
- Mutations targeting views return a clear unsupported-feature error in this
  phase.
- Differential/property cases cover nested views, replacement, dependencies,
  metadata, name shadowing, transactions, errors, and prepared queries.
- The benchmark suite includes a nested filtered view query.

---

## Milestone G — Savepoints and session settings

### Task 19 — Savepoints and subtransaction recovery

**Goal:** Implement nested transaction checkpoints used by PostgreSQL clients
and SQLx nested transactions.

**DoD:**

- SQL `SAVEPOINT`, `ROLLBACK TO SAVEPOINT`, and `RELEASE SAVEPOINT` support
  nesting, duplicate names, nearest-name lookup, and PostgreSQL transaction-state
  validation.
- Row and catalog versions record enough subtransaction identity to discard
  work after a savepoint while preserving work before it and keeping the outer
  transaction active.
- Rolling back to a savepoint clears the aborted-transaction state, discards
  later deferred-constraint events, restores affected session-local settings,
  and releases locks acquired after the savepoint where PostgreSQL does.
- Releasing a savepoint reparents its state correctly so a later outer rollback
  still removes it.
- SQLx nested `Transaction` commit, rollback, and drop behavior drives the same
  state machine and never emits unsupported transaction control.
- Differential and controlled multi-session tests cover errors, repeated names,
  DML, transactional DDL, deferred foreign keys, lock wakeups, and nested SQLx
  transactions.
- The benchmark suite includes nested savepoint create/release and rollback.

### Task 20 — Typed GUC registry and general `SET`/`SHOW`/`RESET`

**Goal:** Replace scattered session-variable handling with a typed registry for
settings that affect supported behavior or driver compatibility.

**DoD:**

- One registry defines each supported setting's canonical name, aliases, type,
  default, accepted values/units, context, mutability, strict-mode policy, and
  effect on execution.
- `SET`, `SET SESSION`, `SHOW`, `RESET`, and `RESET ALL` use that registry with
  PostgreSQL-compatible case folding, formatting, validation, and error codes.
- The initial semantic set includes `TimeZone`, `lock_timeout`,
  `default_transaction_isolation`, and settings discovered by Task 1 that
  change behavior already guaranteed by the project.
- Planner-only or driver-probing settings may be tracked/tolerated only when
  their value cannot change Tier-A behavior; strict mode rejects unsupported
  settings consistently.
- Session settings are isolated between sessions, copied appropriately into a
  database snapshot's new clean sessions, and used by prepared execution at the
  same time PostgreSQL consults them.
- Differential/property cases cover aliases, units, invalid values, reset,
  session isolation, strict mode, and prepared statements.

### Task 21 — Transaction-local settings and GUC functions

**Goal:** Complete transactional GUC behavior and the common functional access
surface.

**DoD:**

- `SET LOCAL` changes a value only until the current transaction ends and
  restores correctly across commit, rollback, nested savepoints, and repeated
  session/local assignments.
- `set_config(name, value, is_local)` and `current_setting(name [, missing_ok])`
  share the typed registry and PostgreSQL error/NULL behavior.
- Transaction isolation selection follows the precedence in `spec.md` §5.4,
  including `SET SESSION CHARACTERISTICS AS TRANSACTION` and legal
  `SET TRANSACTION` timing.
- `SHOW transaction_isolation` and related read-only settings reflect the
  active transaction without allowing invalid direct mutation.
- Semantic settings take effect at PostgreSQL-compatible statement or
  transaction boundaries and do not retroactively change captured mock-time or
  snapshot state.
- Differential and multi-session tests cover commit/rollback, savepoints,
  session defaults, functions, missing names, prepared statements, and SQLx
  startup behavior.

---

## Milestone H — Row locking and SERIALIZABLE isolation

### Task 22 — Complete `SELECT` row-locking clauses

**Goal:** Implement PostgreSQL row-lock strengths, wait policies, and query
placement rules.

**DoD:**

- `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`, and `FOR KEY SHARE` acquire
  distinct lock modes with PostgreSQL's compatibility matrix.
- `OF relation`, multiple locking clauses, `NOWAIT`, and `SKIP LOCKED` select
  and lock the correct base rows after query qualification without locking
  null-extended or non-selected rows.
- Row locking composes with joins, subqueries, CTEs, views where legal,
  ordering, and limiting, and is rejected with the correct error category for
  grouping, windows, `DISTINCT`, set operations, and other illegal contexts.
- UPDATE chooses `FOR UPDATE` strength only when a key relevant to a usable
  unique index changes and otherwise uses `FOR NO KEY UPDATE`; foreign-key
  checks use the matching key-share behavior.
- Waits recheck visibility after wakeup and produce correct READ COMMITTED versus
  REPEATABLE READ outcomes; timeouts, deadlocks, `NOWAIT`, and `SKIP LOCKED`
  use the existing lock manager without busy waiting.
- Differential and controlled multi-session cases cover the full compatibility
  matrix, joins, `OF`, limits, wakeups, rollback, timeouts, deadlocks, and SQLx.
- The benchmark suite includes uncontended locking and a `SKIP LOCKED` work
  queue.

### Task 23 — SERIALIZABLE dependency tracking

**Goal:** Track the read/write dependencies needed to detect SSI dangerous
structures without changing READ COMMITTED or REPEATABLE READ behavior.

**DoD:**

- SERIALIZABLE transactions use transaction snapshots and record row, unique-key,
  and relation-scan reads plus writes with stable transaction identities.
- Read-write antidependencies are created when a concurrent writer changes data
  read by another serializable transaction, including rows that were absent at
  a point lookup.
- Dependency state survives statement boundaries, savepoints, waits, and
  concurrent commits, and is reclaimed only after no active transaction can
  participate in a dangerous structure.
- Read-only and read-write serializable transactions never observe data outside
  their snapshot; READ COMMITTED and REPEATABLE READ do not pay semantic costs
  or acquire predicate locks accidentally.
- Internal unit/property tests cover dependency graph construction, aborted
  transactions, overlapping snapshots, savepoint rollback, cleanup horizons,
  and unique-key gaps.

### Task 24 — SSI validation and predicate conflicts

**Goal:** Reject executions that are not serializable while allowing valid
concurrent histories.

**DoD:**

- Commit/statement validation detects PostgreSQL SSI dangerous structures and
  aborts an eligible transaction with `40001` without false success.
- Predicate reads for full scans, filtered scans, joins, aggregates, CTEs,
  views, and supported index/key lookups detect phantoms relevant to supported
  queries.
- Canonical write-skew, read-only anomaly, phantom insert/delete, unique-key,
  foreign-key, and `ON CONFLICT` scenarios match PostgreSQL outcomes under
  controlled interleavings.
- Safe serializable workloads, including read-only snapshots and non-overlapping
  writes, commit without systematic false positives.
- Serialization failure aborts the transaction, releases row locks and waiters,
  preserves nontransactional sequence allocation, and works with savepoint and
  SQLx error handling.
- Repeated randomized schedule tests compare committed histories with a serial
  reference model and assert that no accepted history violates serializability.
- The benchmark suite records uncontended SERIALIZABLE overhead and a contended
  write-skew workload.

---

## Milestone I — Phase 3 conformance and release gate

### Task 25 — Phase 3 integration, regression audit, and benchmarks

**Goal:** Prove that the complete Phase 3 surface works coherently through the
native and SQLx APIs.

**DoD:**

- The Phase 3 conformance manifest passes completely against PostgreSQL 18,
  including results, order/multiplicity, metadata, SQLSTATE, transaction
  outcomes, and controlled concurrent outcomes.
- The full embedded regression audit reports updated passed-statement and
  skipped-script counts against the 463/141 Phase 2 baseline and classifies
  every remaining blocker as fixture-related, parser-limited, or later/out of
  scope.
- End-to-end scenarios combine CTEs, `ON CONFLICT`, windows, views, JSONB,
  arrays, savepoints, session-local GUCs, transactional DDL, row locks, and
  SERIALIZABLE transactions.
- SQLx prepared queries, row decoding/encoding, nested transactions, and
  concurrent transactions cover every Phase 3 type and statement family.
- The property suite passes 10,000 iterations with Phase 3 operations enabled,
  and randomized plus focused multi-session tests pass repeatedly.
- Benchmarks cover set operations, recursive CTEs, conflicting inserts, windows,
  JSONB, arrays, views, savepoints, transactional DDL, row locking, and SSI;
  results are compared with PostgreSQL 18 and misses of the project's speed
  target are reported rather than hidden.
- The unsupported-feature registry and user-facing feature documentation
  distinguish completed Phase 3 behavior from later gaps.

**Notes:** This task fixes integration defects but does not add new SQL families.

### Task 26 — External PostgreSQL-driven fuzzing

**Goal:** Discover SQL through an external fuzzer running against PostgreSQL,
then replay the captured workload against `pg_fake` to find behavioral gaps that
the project's own generators do not explore.

**DoD:**

- Candidate fuzzers are researched and compared for PostgreSQL 18 support, SQL
  coverage, reproducibility, statement capture, schema awareness, stateful
  workload support, automation, licensing, maintenance, and minimization
  capabilities. The selected tool and rationale are recorded before harness
  implementation begins.
- The research determines whether campaigns use a fixed seeded database,
  complete stateful scripts, or both. The chosen reset/snapshot strategy makes
  every captured case reproducible without leaking state between replays.
- A harness runs the selected fuzzer against PostgreSQL 18 and records enough
  information to replay each case: generated SQL, setup or prior statements,
  seed and tool configuration when available, PostgreSQL result or error, and
  relevant session settings.
- Captured statements within the supported Phase 3 surface are replayed against
  `pg_fake`. The comparator checks Tier-A observations: result values and row
  multiplicity, order when requested, column metadata, affected-row counts,
  transaction outcomes, and `SQLSTATE` where PostgreSQL returns an error.
- Out-of-scope SQL, unsupported parser syntax, intentionally nondeterministic
  behavior, and infrastructure failures are classified separately and do not
  count as pg_fake mismatches. Classification is explicit so genuine in-scope
  failures cannot be silently discarded.
- Every in-scope mismatch emits a standalone replay artifact. Cases are reduced
  automatically when the selected tooling supports it, otherwise the harness
  preserves the smallest reproducible prefix and all information needed for
  manual reduction.
- Fixed mismatches are promoted into a deterministic regression corpus that runs
  without invoking the external fuzzer. Duplicate findings are grouped by a
  stable failure signature.
- A bounded smoke campaign runs in the normal test workflow, while a documented
  long-running command supports local or scheduled campaigns. Reports include
  generated cases, PostgreSQL-accepted cases, in-scope replays, classified
  skips, unique mismatches, and saved regressions.
- The Phase 3 release campaign completes with no untriaged in-scope mismatch;
  every finding is fixed and retained as a regression or explicitly approved
  as a documented fidelity limitation.

**Notes:** The external fuzzer supplements the existing `chaos_theory`
property generator rather than replacing it. Tool selection and the fixed versus
stateful workload model are intentionally research outcomes of this task.

---

## Phase 3 exit criteria

- All 26 tasks meet their DoD and have been approved before being marked
  complete.
- The Phase 3 conformance manifest passes against PostgreSQL 18 without
  regressing the 32 Phase 2 cases.
- The embedded regression corpus improves from the 463/141 Phase 2 baseline,
  and every remaining blocker is explicitly classified.
- CTEs, set operations, conflicting inserts, window functions, ordinary views,
  JSON/JSONB, one-dimensional arrays, savepoints, modeled GUCs, transactional
  DDL, row-lock variants, and SERIALIZABLE isolation work through native and
  SQLx APIs.
- Tier-A behavior for the supported Phase 3 surface matches PostgreSQL for
  results, NULL semantics, types, constraints, catalog visibility, transaction
  recovery, locking, concurrent outcomes, and SQLSTATE.
- The external PostgreSQL-driven fuzzing campaign has no untriaged in-scope
  mismatch, and minimized fixed cases are retained as deterministic regressions.
- Later work remains explicit: JSONPath and the broader SQL/JSON surface,
  multidimensional/non-default-bound arrays, materialized/updatable/recursive
  views, rules/triggers, broad schema/index/`ALTER TABLE` DDL, privileges,
  `MERGE`, grouping sets, table functions, `LATERAL`, generated columns, new
  type families, extensions, procedures, and wire-protocol support.
