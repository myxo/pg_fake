# pg_fake — Phase 3 Implementation Plan

Phase 3 delivers the scope from `spec.md` §9. Migration-critical DDL,
transaction, JSONB, temporal, locking, and query-expression features are
scheduled before the broader window, array, and SERIALIZABLE work so useful
SQLx application workloads become runnable earlier.

The Phase 3 commitments are: CTEs (including recursive and
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
- Before a task can be marked complete, the property suite must pass
  10,000 `chaos_theory` iterations or 10 min, run with
  `CHAOS_THEORY_CHECK_ITERS=10000 CHAOS_THEORY_CHECK_TIME=600s` and
  `cargo test -p pg_fake_sqlx --test property_tests`.
- After implementing a task, update its progress but do not mark it complete
  until the user approves the result, as required by `AGENTS.md`.
- Valid PostgreSQL syntax that `sqlparser-rs` cannot represent must be fixed in
  the parser dependency rather than recognized by project-local parsing.
- Features not listed here retain the unsupported-feature behavior from
  `spec.md` §10. Optimization-only clauses may be tolerated only when ignoring
  them cannot change Tier-A behavior, and strict mode must still reject them.
- Tasks 8 through 26 are the priority track. They must be taken in order before
  Tasks 27 onward unless a prerequisite defect forces a narrowly documented
  exception.

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

Every task in the priority track must promote representative application and
migration SQL into stable local regression fixtures.

Passing an entire source file is not a per-task requirement because those files
exercise extensions, privileges, procedural code, DDL, types, and planner
behavior beyond Phase 3. Each task instead promotes relevant statements into
the Phase 3 conformance set and records newly exposed blockers.

The Phase 3 surface is intentionally bounded:

- views are ordinary read-only views; automatic view updates, materialized
  views, recursive views, rules, privileges, and security-barrier or
  security-invoker behavior remain later work;
- JSONPath and SQL/JSON constructors and query functions are outside this phase;
- arrays cover one-dimensional arrays of supported scalar element types with
  the standard lower bound of one; multidimensional arrays, non-default lower
  bounds, arrays of composite/domain types, and array slices remain later work;
- the GUC registry models settings that affect supported behavior or are emitted
  by supported drivers. It does not attempt PostgreSQL's full server-wide
  configuration catalog;
- procedural functions, `DO`, triggers, schemas, index DDL, and `ALTER TABLE`
  are limited to the forms explicitly listed in Tasks 10–15. They are not a
  promise of complete PL/pgSQL or general-purpose PostgreSQL DDL support;
- `LATERAL` is limited initially to correlated relation/subquery forms;
  broader table-function support remains later work;
- `MERGE`, grouping sets, stored generated columns, new general-purpose type
  families, extensions, procedures, privileges, server-level database
  management, and wire-protocol support remain later work.

### Required migration workload

Tasks 10–20 must support the SQL forms below. This list is normative and
self-contained: an implementer does not need to discover any unlisted SQL input
or external directory. Each form must have a checked-in focused fixture, and
Task 20 must combine the fixtures into the ordered migration scenarios described
there.

- catalog and DDL: qualified `public` and `pg_temp` names; temporary tables with
  `ON COMMIT DROP`; transactional table, column, constraint, sequence, index,
  view, function, and trigger operations; `COMMENT ON VIEW`; trigger rename;
  partial and covering indexes; `NOT VALID` foreign keys followed by
  `VALIDATE CONSTRAINT`; and table locks;
- procedural SQL: no-argument `RETURNS TRIGGER` PL/pgSQL functions, row-level
  `BEFORE INSERT`, `BEFORE UPDATE`, and `BEFORE INSERT OR UPDATE` triggers, and
  anonymous `DO` blocks with the constructs enumerated in Task 15;
- data transforms: `INSERT ... SELECT`, `UPDATE ... FROM`, correlated scalar
  subqueries, `EXISTS`/`NOT EXISTS`, materialized and ordinary CTEs, inner and
  left joins, aggregate and window expressions, ordered limiting, and the
  exact expressions enumerated in Task 19;
- types used by the workload: Boolean, integer families, `numeric`, text and
  `char(3)`, `bytea`, UUID, date, timestamptz, interval, and JSONB, including
  their existing assignment casts, defaults, constraints, SQLx metadata, and
  parameter/result encoding;
- transaction behavior: SQLx migration transactions, `SET LOCAL`, table-lock
  acquisition and timeout, atomic failure of DDL/DML/procedural statements,
  rollback of a failed migration, and successful SQLx reapplication without
  re-executing already recorded versions.

“Unchanged” in Task 20 means that the same checked-in SQL body is passed to both
PostgreSQL 18 and `pg_fake` through SQLx without deleting, reordering, splitting,
or rewriting statements. Harness setup and SQLx migration bookkeeping may live
outside that SQL body. A missing SQL form is a plan defect, not permission to
silently tolerate or simplify it; update this section and assign the form to a
task before implementation continues.

---

## Milestone A — Conformance baseline and set operations

### Task 1 — Phase 3 regression manifest and progress reporting [COMPLETE]

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

### Task 2 — Query set operations [COMPLETE]

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

### Task 3 — Non-recursive CTEs [COMPLETE]

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

### Task 4 — Recursive CTEs [COMPLETE]

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

### Task 5 — Data-modifying CTEs [COMPLETE]

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

### Task 6 — `ON CONFLICT DO NOTHING` and arbiter inference [COMPLETE]

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

**Progress:**

- [x] Preserve names for primary-key and unique constraints and resolve conflict
  targets against their backing unique indexes.
- [x] Execute `DO NOTHING` row by row, including `RETURNING`, defaults,
  sequences, NULL keys, and non-conflict errors.
- [x] Lock conflicting rows, including uncommitted candidates, and recheck after
  concurrent commit or rollback.
- [x] Add focused native, SQLx differential/property, multi-session, and
  benchmark coverage.
- [x] Run formatting, focused/workspace tests, and the 10,000-iteration property
  gate.

### Task 7 — `ON CONFLICT DO UPDATE` [COMPLETE]

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

**Progress:**

- [x] Bind the target relation, its alias, and `excluded` for assignments and
  the optional conflict predicate.
- [x] Update the locked conflicting row through existing coercion, constraint,
  foreign-key, and `RETURNING` paths.
- [x] Enforce cardinality violations, secondary unique conflicts, statement
  atomicity, and READ COMMITTED/REPEATABLE READ conflict outcomes.
- [x] Extend differential/property, SQLx, controlled multi-session, and
  benchmark coverage.
- [x] Run formatting, workspace tests, and the 10,000-iteration property gate.

---

## Milestone D — Transactional catalog and migration DDL

### Task 8 — MVCC-versioned catalog foundation [COMPLETE]

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

### Task 9 — Transactional DDL for the supported catalog surface [COMPLETE]

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

**Notes:** This task makes the already-supported DDL surface transactional;
Tasks 10–15 add the bounded migration surface.

**Progress:**

- [x] Execute table and sequence creation/drop through catalog MVCC in explicit
  and implicit transactions.
- [x] Preserve table storage, indexes, constraints, sequence ownership, and
  nontransactional sequence allocations across commit and rollback.
- [x] Serialize conflicting relation and dependency DDL until transaction end.
- [x] Keep prepared statements bound to stable catalog identities across
  rollback and name reuse.
- [x] Add native, SQLx differential/property, controlled multi-session, and
  transactional-DDL benchmark coverage.
- [x] Run formatting, workspace tests, review, and the 10,000-iteration
  property gate.

### Task 10 — Qualified names, search path, and temporary relations [COMPLETE]

**Goal:** Resolve common migration namespaces and temporary objects.

**DoD:**

- `public.name`, `pg_temp.name`, and unqualified names obey
  PostgreSQL lookup, shadowing, duplicate-name, and missing-object behavior.
- `CREATE TEMP[TEMPORARY] TABLE`, `DROP TABLE`, and transaction cleanup isolate
  temporary relations per session and honor the required `ON COMMIT` forms.
- Qualified table, sequence, index, view, function, and trigger references used
  by the required migration workload bind to stable catalog identities.
- Unsupported schemas and search-path behavior fail explicitly; support is not
  broadened beyond the Phase 3 migration fixtures and SQLx startup requirements.
- Differential, transaction, and multi-session cases cover qualification,
  shadowing, rollback, cleanup, and prepared metadata.

### Task 11 — Migration `ALTER TABLE` families [COMPLETE]

**Goal:** Implement the table evolution operations required by the Phase 3
migration fixtures.

**DoD:**

- Add/drop/rename column, rename table, `ALTER COLUMN ... TYPE ... USING`,
  set/drop default, set/drop `NOT NULL`, and add/drop constraints execute
  transactionally.
- Existing rows are backfilled and coerced as PostgreSQL requires; validation
  failures are atomic and use compatible `SQLSTATE` values.
- Unique, check, and foreign-key constraints support the fixtures'
  `NOT VALID` and later `VALIDATE CONSTRAINT` lifecycle.
- Column/table renames preserve dependent indexes, constraints, sequences,
  triggers, views, prepared identity, and row data.
- Multi-action statements, `IF EXISTS`/`IF NOT EXISTS`, cascades explicitly used
  by the fixtures, rollback, and errors have focused differential coverage.

**Progress:**

- [x] Model named and validated constraints and catalog-preserving renames.
- [x] Execute supported `ALTER TABLE` operations atomically with row backfills,
  rewrites, defaults, nullability changes, and constraint validation.
- [x] Preserve existing sequence and foreign-key dependencies and rebuild unique
  indexes across schema changes.
- [x] Add focused native, SQLx differential/property, transactional, manifest,
  and benchmark coverage.
- [x] Run formatting, workspace tests, review, and the 10,000-iteration property
  gate.

### Task 12 — Index DDL and partial unique indexes [COMPLETE]

**Goal:** Model the indexes created, dropped, renamed, and used as arbiters by
common application migrations.

**DoD:**

- `CREATE [UNIQUE] INDEX [IF NOT EXISTS]`, `DROP INDEX [IF EXISTS]`, and
  `ALTER INDEX [IF EXISTS] ... RENAME TO` support qualified index and table
  names with PostgreSQL relation-namespace collision and missing-object
  behavior.
- Default B-tree indexes support one through four simple column keys, implicit
  or explicit `ASC`/`DESC` direction per key, and non-key `INCLUDE` columns.
  Included columns participate in dependency tracking but not uniqueness or
  conflict-arbiter identity.
- Partial-index predicates support the migration forms built from Boolean
  columns, `IS NULL`/`IS NOT NULL`, comparisons with typed literals, `IN`
  literal lists, parentheses, `AND`, and `OR`. Predicate evaluation uses
  PostgreSQL three-valued logic, so only rows for which the predicate is true
  belong to the index.
- Partial unique indexes enforce writes and participate in `ON CONFLICT`
  inference, including `ON CONFLICT (columns) WHERE predicate`, only when the
  key columns and predicate satisfy PostgreSQL inference rules.
- Index objects and dependencies follow transactional catalog visibility and
  table/column rename and drop behavior. Creating a unique index validates
  existing qualifying rows atomically; inserts and updates maintain membership
  when a row enters, leaves, or changes within a predicate.
- Differential/property cases cover duplicate data, one-to-four-column keys,
  ascending and descending keys, included columns, every supported predicate
  form, predicate transitions, rollback, name collisions, arbiter inference,
  and metadata. The Task 20 scenarios exercise create, drop, rename,
  ordinary partial indexes, partial unique indexes, and covering indexes.

**Notes:** Expression keys, collations, operator classes, non-B-tree methods,
`CONCURRENTLY`, storage parameters, tablespaces, and explicit NULL-ordering
options remain unsupported because the migration surface does not require
them. The parser dependency must expose `IF EXISTS` on `ALTER INDEX` and the
optional arbiter predicate in `ON CONFLICT (columns) WHERE predicate` in its
AST; valid PostgreSQL syntax must not be recovered by project-local SQL
parsing.

**Progress:**

- [x] Extend the parser AST for `ALTER INDEX IF EXISTS` and partial
  `ON CONFLICT` arbiter predicates.
- [x] Model named index identities, key directions, included columns, and
  predicates in the transactional catalog.
- [x] Execute create, drop, and rename operations with namespace, validation,
  dependency, and rollback behavior.
- [x] Enforce full and partial unique indexes on existing and changed rows and
  use them for supported `ON CONFLICT` inference.
- [x] Add focused native, SQLx differential/property, manifest, and benchmark
  coverage.
- [x] Run formatting, workspace tests, review, and the 10,000-iteration
  property gate.

### Task 13 — Ordinary views and catalog-object renames [COMPLETE]

**Goal:** Store named query definitions in the catalog and expand them with
PostgreSQL scope and metadata behavior.

**DoD:**

- `CREATE VIEW`, `CREATE OR REPLACE VIEW`, and `DROP VIEW` support qualified
  names, explicit column lists, replacement validation, `IF EXISTS`, and
  dependency-aware errors.
- Selecting from a view binds its stored query with the caller's snapshot and
  session settings while preserving view-column names, types, typmods, aliases,
  and nested view scopes.
- Views compose with joins, CTEs, subqueries, aggregation, prepared statements,
  and other views; later type/window tasks add their own view cross-coverage.
- View definitions are transactional and follow catalog snapshot visibility;
  dropping referenced objects or a referenced view fails rather than leaving a
  dangling definition.
- Recursive view dependencies are detected, and parameters or temporary
  caller-only names cannot leak into stored definitions.
- `COMMENT ON VIEW ... IS ...` stores, replaces, and clears view comments
  transactionally without affecting the view definition or dependencies.
- Mutations targeting views return a clear unsupported-feature error in this
  phase.
- `ALTER TRIGGER name ON table RENAME TO new_name` preserves trigger identity
  and dependencies across commit and rollback.
- Differential/property cases cover nested views, replacement, dependencies,
  metadata, name shadowing, transactions, errors, and prepared queries.
- The benchmark suite includes a nested filtered view query.

**Required migration forms:** The gate specifically requires
`CREATE VIEW name AS SELECT ...`, `COMMENT ON VIEW name IS <string>`,
`DROP VIEW IF EXISTS name`, and
`ALTER TRIGGER name ON table RENAME TO new_name`. Creating and dropping the
view and renaming the trigger must work in a SQLx migration transaction. The
workload does not write through the view, so its comment describing compatibility
does not expand this task to updatable-view semantics.

**Progress:**

- [x] Model transactional ordinary views, comments, stable output metadata,
  nested expansion, and catalog dependencies for tables, views, sequences,
  columns, and primary-key grouping behavior.
- [x] Execute create, replace, drop, comment, read composition, and explicit
  unsupported view mutations with temporary-schema and prepared-statement
  behavior.
- [x] Preserve stored view bindings across table/column renames and unrelated
  column drops, including correlated scopes, CTE shadowing, merged joins, and
  positional aliases.
- [x] Model stable trigger identities and transactional trigger rename while
  keeping general trigger creation explicitly unsupported until Task 15.
- [x] Add native, SQLx differential/property, migration, regression-manifest,
  and nested filtered-view benchmark coverage.
- [x] Run formatting, workspace tests, subagent review with all findings fixed,
  and the 10,000-iteration property gate.
- [x] Publish the parser-fork `ALTER TRIGGER ... RENAME` change, pin its commit
  in the workspace dependency, and verify without a local path override.

### Task 14 — Migration-local settings and table locks [COMPLETE]

**Goal:** Execute migration coordination statements rather than ignoring them.

**DoD:**

- `SET LOCAL lock_timeout` and `SET LOCAL statement_timeout` parse, validate,
  take effect for the transaction, and restore on commit or rollback.
- `LOCK TABLE ... IN ACCESS EXCLUSIVE MODE` and `... IN EXCLUSIVE MODE`,
  including multiple qualified relations, use the lock manager with the
  corresponding PostgreSQL compatibility.
- Lock waits, configured timeouts, deadlock participation, release, and
  concurrent DDL/DML conflicts have controlled multi-session coverage.
- Modes or settings outside the bounded migration surface remain explicit
  unsupported features until Tasks 28–29 broaden GUC behavior.

**Required migration forms:** Accept the literal assignments
`SET LOCAL lock_timeout = '5s'` and
`SET LOCAL statement_timeout = '30min'`. Support both one-relation
`LOCK TABLE public.relation IN ACCESS EXCLUSIVE MODE` and comma-separated
multi-relation `LOCK TABLE public.a, public.b IN EXCLUSIVE MODE`. Acquire a
multi-relation lock set atomically in deterministic catalog-identity order so
the statement cannot retain a partial set after timeout or error. Enforce
`statement_timeout` separately for each subsequent statement, with PostgreSQL's
per-statement timer reset; it is not merely parsed or stored.

**Progress:**

- [x] Implement transaction-local `lock_timeout` and `statement_timeout` with
  PostgreSQL unit parsing, restoration, per-statement deadlines, and `57014`
  cancellation.
- [x] Implement exclusive and access-exclusive table locks for qualified and
  multi-relation targets with atomic ordered acquisition.
- [x] Integrate relation locks with reads, writes, DDL, prepared execution,
  foreign keys, timeout precedence, deadlock detection, and transaction release.
- [x] Add native concurrency regressions, PostgreSQL SQLx differential/property
  coverage, regression-manifest entries, and table-lock benchmarks.
- [x] Run formatting, workspace tests, Astra review with all findings fixed,
  and the 10,000-iteration property gate.

### Task 15 — Procedural migrations and triggers [COMPLETE]

**Goal:** Add the procedural execution substrate required by migration
functions, triggers, and one-off blocks.

**DoD:**

- `CREATE [OR REPLACE] FUNCTION name() RETURNS TRIGGER ... LANGUAGE plpgsql`
  and `DROP FUNCTION IF EXISTS name()` support zero-argument trigger functions.
  Function bodies support `BEGIN`/`END`, `RETURN NEW`, assignment to `NEW`
  fields with both `:=` and PL/pgSQL's accepted `=` spelling, and nested
  `IF`/`ELSIF`/`ELSE` blocks whose conditions use comparisons, Boolean logic,
  and `IS [NOT] NULL`.
- `DO` blocks execute atomically, and errors abort the surrounding SQLx
  migration transaction.
- `DO` bodies support an optional `DECLARE` section containing `BIGINT` and
  `TEXT` locals; SQL statements from Task 19; multi-expression `SELECT ... INTO
  ...`; `GET DIAGNOSTICS variable = ROW_COUNT`; nested
  `IF`/`ELSIF`/`ELSE`; and `RAISE EXCEPTION` with a literal format string,
  `%` substitution arguments, and optional `USING HINT = <text literal>`.
  Variables bind in expressions and receive PostgreSQL assignment coercion.
- `CREATE TRIGGER`, `DROP TRIGGER IF EXISTS name ON table`, and the Task 13
  rename form support row-level `BEFORE INSERT`, `BEFORE UPDATE`, and
  `BEFORE INSERT OR UPDATE`, with `FOR EACH ROW EXECUTE FUNCTION name()`.
  Trigger return `NULL` skips the row and return `NEW` continues with any field
  mutations; other timings, statement triggers, transition tables, arguments,
  and non-trigger functions remain unsupported.
- Function and trigger dependencies, replacement, qualification, rollback,
  table/column changes, and drop behavior are represented in the transactional
  catalog.
- SQLx migration coverage creates trigger fixtures through supported SQL rather
  than catalog injection; remove the temporary `test-support` Cargo feature,
  `Db::seed_trigger_catalog_for_test`, and its executor/catalog-only helpers
  once `CREATE TRIGGER` is implemented.
- PostgreSQL differential tests cover every supported function, trigger,
  and `DO` control-flow shape, including insert/update trigger side effects,
  skipped rows, affected-row diagnostics, formatted errors, hints, and rollback.

**Notes:** This task is a fixture-bounded procedural interpreter, not complete
PL/pgSQL. Dollar-quoted bodies using `$$`, local variable references, and the
syntax above are mandatory; exception handlers, loops, dynamic SQL, records,
arrays, cursors, and general function calls are outside this task. It must never
accept an unimplemented construct as a no-op. Tasks 16–20 add cross-feature
expressions and validate complete migration blocks.

**Progress:**

- [x] Add sqlparser AST and PostgreSQL parsing for trigger functions, triggers,
  `DO` blocks, procedural statements, and `INSERT ... SELECT ... ON CONFLICT`.
- [x] Implement transactional function and trigger catalogs, dependencies,
  replacement, rename/drop behavior, and row-level execution.
- [x] Execute the bounded PL/pgSQL statements, locals, diagnostics, branching,
  assignments, formatted exceptions, and trigger row control required here.
- [x] Replace catalog-injected trigger fixtures with supported SQL and add
  native, SQLx, PostgreSQL differential, property, manifest, and benchmark
  coverage.
- [x] Pass formatting, workspace checks/tests, Astra review, PostgreSQL 18
  differential matrices, and the exact 10,000-iteration property gate.

---

## Milestone E — JSON and JSONB

### Task 16 — JSON type and text fidelity [COMPLETE]

**Goal:** Add PostgreSQL `json` storage while preserving its textual nature.

**DoD:**

- `BaseType`, `PgType`, and `Value` support `json` with OID 114 through native
  and SQLx APIs.
- Assignment and casts validate JSON while preserving whitespace, key order,
  duplicate keys, numeric spelling, and PostgreSQL `json` text behavior.
- Unknown literals, parameters, defaults, constraints, `RETURNING`, invalid
  documents, Unicode, nesting, metadata, and errors have differential/property
  coverage.
- Operations PostgreSQL does not define for `json`, including ordinary equality
  and ordering, remain rejected.

**Migration dependency:** The required migration workload does not declare
`json`; this task remains a Phase 3 type commitment and is not a prerequisite
hidden behind the Task 20 gate.

**Progress:**

- [x] Add native and SQLx `json` type support with PostgreSQL OID 114.
- [x] Validate JSON input while preserving its exact textual representation,
  including duplicate keys, numeric spelling, Unicode, and deep nesting.
- [x] Support literals, casts, assignments, defaults, parameters, constraints,
  `RETURNING`, metadata, and unknown-literal propagation.
- [x] Reject unsupported JSON equality, ordering, grouping, distinct, indexes,
  unique constraints, and related comparison-dependent operations.
- [x] Add native, SQLx, PostgreSQL differential, property, manifest, and
  benchmark coverage.
- [x] Pass formatting, workspace checks/tests, Astra review, PostgreSQL 18
  differential matrices, and the exact 10,000-iteration property gate.

### Task 17 — JSONB representation and comparison [COMPLETE]

**Goal:** Add normalized `jsonb` values with PostgreSQL equality and ordering.

**DoD:**

- `BaseType`, `PgType`, and `Value` support `jsonb` with OID 3802 through native
  and SQLx encode/decode APIs, including SQLx JSON wrappers.
- Normalization, duplicate-key handling, numeric values, and canonical output
  match PostgreSQL for all JSON value kinds.
- Equality, ordering, and hashing work in constraints, joins, grouping,
  `DISTINCT`, and set operations. Task 19 verifies JSONB window partition keys
  when the initial window implementation is available.
- Casts among `json`, `jsonb`, and text, nesting, metadata, malformed input, and
  normalization have differential/property coverage.

**Required migration forms:** JSONB columns, `NOT NULL`, input through SQLx,
and stored values must work. Fixtures seed object values containing nested
`amount`, `currency`, and `value` members and compare their stored values with
PostgreSQL. Task 18 owns extraction and subsequent numeric casts; Task 20
verifies their composition in the migration gate.

**Progress:**

- [x] Add normalized JSONB values, OID 3802, casts, equality, ordering, and
  hashing, including unique-index and hash-join keys.
- [x] Add SQLx JSON wrappers, parameters, metadata, and result decoding.
- [x] Add native, differential/property, manifest, and benchmark coverage.
- [x] Pass formatting, workspace checks/tests, subagent review, PostgreSQL 18
  differential cases, and the exact 10,000-iteration property gate.

**Validation:** Workspace tests and formatting pass; the required property
command passes all 13 tests, and the final JSONB generator also passes a focused
10,000-iteration run. Subagent review found no remaining issues. The regression
audit reports 651 matching statements, 141 skipped scripts, 32/32 Phase 2 cases,
and 47/58 Phase 3 cases. Clippy completes with 79 existing warnings; strict
`-D warnings` remains blocked by those warnings in unchanged code.

The existing procedural generator reaches the 600-second cutoff before 10,000
cases in one run. Two supplemental runs passed 6,406 and 6,439 cases (12,845
combined); the second used session-only `PGOPTIONS='-c synchronous_commit=off'`.
The JSONB generator completes all 10,000 cases within the original time limit.

The two JSONB benchmarks execute successfully. A short 10-sample run measured
insert/returning at approximately 66 microseconds for `pg_fake` versus 89 for
PostgreSQL, and the join/group workload at 560 versus 321 microseconds. These
measurements ran without concurrent tests and are indicative, not a recorded
baseline; the join/group workload currently misses the project's speed target.


### Task 18 — Core JSON and JSONB operators and functions [COMPLETE]

**Progress:**

- Implemented extraction, containment/existence, concatenation/deletion,
  builders, conversion, mutation, length, and type inspection.
- Added `FROM` expansion with aliases, ordinality, preceding-table references,
  inner/outer joins, and scalar-subquery arguments. `SELECT`-list expansion
  remains explicitly unsupported, as agreed.
- Added the bounded `text[]` argument representation needed by JSON paths and
  key lists, including native/SQLx parameters. Other element types, dimensions,
  explicit bounds, and general array operations remain Tasks 33–34.
- Added focused and generated PostgreSQL differential coverage, prepared-query
  metadata checks, mutation atomicity tests, conformance fixtures, and JSONB
  extraction/containment benchmarks.
- Fixed JSON differential cases live in `json_differential.rs`; the 12
  generated tests remain in `property_tests.rs`. Both suites share the
  PostgreSQL comparison and isolated-database harness in `common/differential.rs`.
  After the split, all four differential tests and all 12 property tests pass
  with `CHAOS_THEORY_CHECK_ITERS=100 CHAOS_THEORY_CHECK_TIME=30s`; review is clean.
- Subagent review is clean, including nested/outer joins, merged `USING`
  columns, qualified aliases, and grouped wildcard projections.
- Formatting and all 404 workspace tests pass (`--skip generated`; generated
  cases run in the separate required property gate). Clippy completes with the
  same 79 existing warnings. The regression audit reports 675 matching
  statements, 141 skipped scripts, 32/32 Phase 2 cases, and 50/60 Phase 3 cases.
- The required `CHAOS_THEORY_CHECK_ITERS=10000
  CHAOS_THEORY_CHECK_TIME=600s cargo test -p pg_fake_sqlx --test property_tests`
  gate passed all 16 tests before separating the four fixed differential tests. A preceding attempt hit
  PostgreSQL disk-full error `53100`; removing generated Rust incremental build
  caches resolved it, and the full rerun passed in 619 seconds.
- Both JSONB benchmarks execute against a 100-row fixture. A short 10-sample
  run without concurrent tests measured extraction at approximately 95 µs for
  `pg_fake` versus 66 µs for PostgreSQL, and containment at 213 versus 42 µs.
  Both currently miss the project's speed target. These are indicative timings,
  not a recorded baseline; Criterion's Gnuplot chart generation failed, but
  timing measurements completed.
- Implementation and review are finished; the user approved committing the task.


**Goal:** Cover the planned JSON surface used by application queries and
migrations.

**DoD:**

- Extraction supports `->`, `->>`, `#>`, and `#>>`; JSONB containment and
  existence support `@>`, `<@`, `?`, `?|`, and `?&` with PostgreSQL SQL-NULL
  versus JSON-null behavior.
- Concatenation and deletion support JSONB `||`, `-` for a text key or integer
  array index, and `#-` for a path. Builders and mutation support
  `json_build_object`, `jsonb_build_object`, `json_build_array`,
  `jsonb_build_array`, `to_json`, `to_jsonb`, and `jsonb_set`.
- Length and expansion support `json_array_length`, `jsonb_array_length`,
  `json_object_keys`, `jsonb_object_keys`, `json_each`, `jsonb_each`,
  `json_each_text`, `jsonb_each_text`, `json_array_elements`,
  `jsonb_array_elements`, `json_array_elements_text`, and
  `jsonb_array_elements_text`. Missing paths, negative indexes, duplicate keys,
  containment, parameters, and errors have differential/property tests.
- Set-returning JSON functions are supported in `FROM`, including references
  to preceding tables and functions. Expansion in the `SELECT` list and other
  unsupported placements fail loudly.
- Benchmarks cover JSONB extraction and containment.

**Notes:** JSONPath, SQL/JSON constructors, `JSON_TABLE`, JSON indexes, and
record-population functions remain later work.

**Required migration forms:** Only `jsonb #> unknown-path-literal`,
`jsonb #>> unknown-path-literal`, and `jsonb_typeof(jsonb)` are used by the
required migration workload. Path literals such as `'{amount,value}'` must
receive the operator's `text[]` type without requiring the general array feature
from Tasks 33–34. Missing paths, JSON null, SQL NULL, string versus numeric JSON
values, and invalid numeric casts require focused differential cases. Extracted
JSONB strings cast through `numeric` to `bigint`, including nested `amount`,
`currency`, and `value` fixtures, must match PostgreSQL. The other
operators and functions in this task remain independent Phase 3 commitments;
they are not prerequisites invented by the migration gate.

---

## Milestone F — Migration query completion

### Task 19 — Migration data-transform query subset

**Goal:** Execute relational expressions commonly embedded in migration
backfills and reconciliation blocks.

**DoD:**

- Window expressions support `row_number() OVER (ORDER BY expression)` and
  `count(*) OVER (PARTITION BY expression)` with PostgreSQL ordering, peer,
  NULL, and `bigint` result behavior. Differential cases verify JSONB partition
  keys, including equivalent normalized objects and numbers, and SQL NULL.
- Aggregate expressions support `count(*)`, `max(expression)`, and
  `string_agg(expression, delimiter ORDER BY expression)`. They work in a
  scalar subquery, in a multi-expression `SELECT ... INTO`, and over an empty
  input with PostgreSQL NULL/count behavior.
- Query predicates support `IS DISTINCT FROM`/`IS NOT DISTINCT FROM`, `IN`
  literal lists, `EXISTS`/`NOT EXISTS`, and the `~` regular-expression operator.
  The required regular expressions are anchored ASCII character classes,
  bounded repetitions, groups, and optional groups; invalid patterns return
  PostgreSQL-compatible errors.
- Scalar expressions support `coalesce`, `btrim(text)`, `jsonb_typeof`, searched
  `CASE`, casts through `text`, UUID, `numeric`, and `bigint`, integer/numeric
  multiplication and comparison, `CURRENT_TIMESTAMP + INTERVAL '7 days'`, and
  `extract(epoch FROM timestamptz)::bigint`, with PostgreSQL coercion, overflow,
  NULL, and result metadata.
- DML supports `INSERT INTO ... SELECT`, `UPDATE [AS alias] ... FROM relation`,
  `UPDATE` from a derived table, and `UPDATE` values computed by a correlated
  scalar subquery with `LIMIT`. Existing `ON CONFLICT (columns) DO NOTHING`
  composes with insert-select.
- Query composition supports ordinary and `AS MATERIALIZED` CTEs, multiple CTEs,
  nested scalar subqueries, inner and left joins, derived tables, qualified and
  temporary relations, `ORDER BY`, and `LIMIT`. Correlation and alias binding
  must match PostgreSQL in every combination named in this task.
- Every grammar form and built-in named above has a focused PostgreSQL
  differential fixture covering its positive case plus relevant empty,
  duplicate, NULL, invalid-input, and rollback behavior. Property generation
  covers casts, arithmetic, predicates, and the non-procedural query shapes.

**Notes:** `LIKE`/`ILIKE` and regex functions remain Task 21 runtime work; they
are not used by the required migration workload. Task 19 owns the temporal and
numeric forms named above because Task 20 depends on them and runs before
Task 21.

### Task 20 — Transactional migration-chain gate

**Goal:** Prove that the required migration forms compose into realistic,
transactional SQLx migration sequences.

**DoD:**

- Add three checked-in SQLx migration scenarios with descriptive names:
  `schema_evolution`, `procedural_triggers`, and `data_reconciliation`. Their
  files use only neutral relation and column names and are the sole inputs to
  both PostgreSQL and `pg_fake`; there is no engine-specific SQL variant.
- `schema_evolution` starts from an empty database, creates related tables with
  UUID, text, Boolean, integer, numeric, bytea, date, timestamptz, interval, and
  JSONB columns, then exercises the Task 11–13 sequence, constraint, index,
  rename, view, comment, validation, and drop forms. It includes a non-empty
  sequence backfill using `row_number`, `max`, `setval`, and `nextval`.
- `procedural_triggers` creates the Task 15 update-timestamp trigger and a
  conditional insert-or-update compatibility trigger, proves their effects on
  inserted and updated rows, renames and drops trigger objects, and includes a
  failing `DO` block that raises a formatted exception with a hint.
- `data_reconciliation` uses qualified permanent tables and an
  `ON COMMIT DROP` temporary match table. It exercises materialized CTEs, JSONB
  extraction and validation, candidate matching with `EXISTS`/`NOT EXISTS`,
  window counts, ordered `string_agg`, `SELECT ... INTO`, `UPDATE ... FROM`,
  `GET DIAGNOSTICS`, a `NOT VALID` foreign key followed by validation, and the
  Task 14 settings and table locks.
- Each scenario is applied file by file in version order through SQLx's normal
  migration transactions. After every version, schema objects and seeded rows
  match PostgreSQL 18 so intermediate objects later renamed or dropped remain
  observable.
- Procedural blocks run their complete catalog checks, JSONB parsing,
  reconciliation queries, diagnostics, and error paths without reduced or
  rewritten SQL.
- Files containing backfills, procedural validation, a
  `NOT VALID`/validation lifecycle, temporary tables, or locks also have a
  boundary test that applies the exact preceding scenario prefix, inserts the
  named legacy dataset, then runs the unchanged migration. A failure reports
  the scenario, version, and first failing statement index.
- Boundary cases cover an empty database state where applicable, successful
  non-empty backfills, rejected unsupported legacy currency values, trigger
  field propagation, matched and unmatched/ambiguous reconciliation data,
  JSONB string/numeric/missing-path variants, foreign-key validation failure,
  timeout while a table lock is held, and statement/transaction rollback after
  `RAISE EXCEPTION` (including its hint).
- After a full successful apply, invoking the SQLx migrator again executes no
  migration bodies and leaves schema, rows, sequences, triggers, and migration
  metadata unchanged. A test-only failing migration containing both catalog and
  row changes proves that SQLx and `pg_fake` roll the entire version back.
- Every item under “Required migration workload” and every SQL form explicitly
  named in Tasks 13–19 is executed by at least one scenario. Maintain a coverage
  table mapping each form to its scenario and version; the gate has no
  unsupported, unclassified, rewritten, or unresolved migration blocker.

**Observable comparison:** Compare catalog objects by semantic fields rather
than PostgreSQL internal OIDs: relation kind and qualification; ordered columns,
types, typmods, defaults, and nullability; constraint kind, columns, referenced
relation/actions, validation state, and predicate; index uniqueness, ordered
keys/directions, included columns, and predicate; sequence ownership/value;
view output metadata and definition behavior; function/trigger signature,
timing/events, dependencies, and side effects. Compare seeded table contents as
ordered typed rows, plus affected-row counts, SQLSTATE, and transaction outcome.

---

## Milestone G — Priority runtime SQL

### Task 21 — Temporal, formatting, numeric, and pattern expressions

**Goal:** Implement common scalar and aggregate expression families used by
application queries.

**DoD:**

- `to_timestamp`, `to_char`, `date_trunc`, and `AT TIME ZONE` support the
  argument types, formats, time-zone interpretation, and NULL/error behavior
  covered by the Phase 3 fixtures.
- `floor`, required numeric coercions, `LIKE`/`ILIKE`, regular-expression
  operations, and `string_agg` cover every runtime shape in the manifest.
- Expression results and metadata work through SQLx decoding, prepared
  parameters, grouping, ordering, filtering, and updates.
- Differential/property tests cover boundary timestamps, time zones, negative
  epochs/numbers, formatting, escaping, invalid inputs, NULLs, and collation
  assumptions.

### Task 22 — Bounded `LATERAL` joins

**Goal:** Execute common correlated lateral subquery shapes.

**DoD:**

- `LATERAL` subqueries on the required inner/left join forms are evaluated for
  each row with correct correlation scope, aliasing, NULL extension, ordering,
  and limiting.
- Planner/executor behavior composes with aggregates, CTEs, prepared
  parameters, JSONB, and temporal expressions used by the manifest.
- Illegal references and unsupported lateral table-function forms fail with
  compatible error categories rather than being treated as ordinary joins.
- Differential/property tests cover zero/one/many inner rows, nested scopes,
  NULLs, volatile expressions, metadata, and errors.

### Task 23 — Complete `SELECT` row-locking clauses

**Goal:** Implement work queues and the remaining planned PostgreSQL row-lock
surface.

**DoD:**

- `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`, and `FOR KEY SHARE` implement
  PostgreSQL's compatibility matrix.
- `OF relation`, multiple clauses, `NOWAIT`, and especially `SKIP LOCKED` select
  and lock the correct base rows after qualification, ordering, and limiting.
- Wait/recheck behavior, lock timeouts, deadlocks, rollback, and READ COMMITTED
  versus REPEATABLE READ outcomes use the existing lock manager without busy
  waiting.
- Legal composition and rejection rules for joins, subqueries, CTEs, views,
  grouping, windows, `DISTINCT`, and set operations match PostgreSQL.
- Differential/property and controlled multi-session tests include work-queue
  queries and a `SKIP LOCKED` benchmark.

### Task 24 — Advisory locks

**Goal:** Support transaction-scoped application coordination.

**DoD:**

- The manifest's `pg_advisory_xact_lock` and try-lock forms support required
  integer signatures, key identity, reentrancy, waiting, and return values.
- Advisory locks are isolated from row/table lock identities but participate in
  timeout, deadlock detection, transaction release, and rollback behavior.
- Session-level advisory-lock forms remain unsupported unless the manifest
  requires them.
- Controlled multi-session differential tests cover contention, ordering,
  repeated acquisition, abort, timeouts, deadlocks, and SQLx transactions.

### Task 25 — PostgreSQL compatibility utilities and maintenance statements

**Goal:** Cover the small server-facing surface reached on normal startup and
runtime paths without pretending to be a complete PostgreSQL server.

**DoD:**

- `pg_is_in_recovery()` returns a deterministic primary-server result for
  startup and migration logic.
- `regclass`, `pg_lsn`, and the explicitly supported catalog
  functions/relations have PostgreSQL-compatible types and observable
  semantics recorded in the conformance manifest.
- `TRUNCATE` supports relation lists, identity, foreign-key,
  transaction, and locking behavior.
- Unsupported catalog columns/functions and server-management statements fail
  explicitly; `CREATE DATABASE` and `DROP DATABASE` remain harness concerns,
  not SQL executed by `pg_fake`.
- Focused PostgreSQL differential tests cover all accepted utility queries,
  metadata, transaction behavior, and errors.

### Task 26 — SQLx application-workload conformance gate

**Goal:** Demonstrate that realistic SQLx workloads run on `pg_fake`, not merely
that isolated SQL parses.

**DoD:**

- The conformance application applies the Task 20 migration scenarios and runs all
  priority SQL families through `pg_fake_sqlx` pools, prepared queries, typed
  rows, and transactions.
- Representative workflows cover identity/session data, work claiming and
  accounting, payment/promotion state, request limits, thread/execution state,
  and append-only logs.
- Concurrent workflows exercise advisory locks, table/row locks, `SKIP LOCKED`,
  transactions, triggers, JSONB, temporal expressions, and rollback.
- Every in-scope manifest entry is either executed by the suite or has a focused
  replay; none remains unsupported, unclassified, or silently skipped.
- PostgreSQL 18 and `pg_fake` agree on Tier-A results, metadata, affected rows,
  transaction outcomes, and `SQLSTATE`; a documented command reproduces the
  full gate.

---

## Milestone H — Remaining Phase 3 transaction features

### Task 27 — Savepoints and subtransaction recovery

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

### Task 28 — Typed GUC registry and general `SET`/`SHOW`/`RESET`

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

### Task 29 — Transaction-local settings and GUC functions

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

**Notes:** Task 14 supplies the early `SET LOCAL` migration subset. This task
generalizes it onto the registry and adds all remaining
Phase 3 functional/session behavior.

---

## Milestone I — Complete window functions

### Task 30 — Named windows and remaining ranking functions

**Goal:** Extend Task 19's migration window substrate to the full planned
binding, partitioning, ordering, and ranking surface.

**DoD:**

- Named `WINDOW` clauses support inheritance and reject cycles, overrides, and
  illegal references with compatible errors.
- `rank`, `dense_rank`, `percent_rank`, `cume_dist`, and `ntile` implement
  peer-group, NULL-ordering, empty-partition, and result-type behavior alongside
  `row_number`.
- Multiple compatible/incompatible window specifications run after grouping and
  `HAVING` and before final `DISTINCT`, ordering, and limiting.
- Differential/property cases cover partitions, peers, NULLs, named windows,
  clause ordering, metadata, placement, nesting, and errors.

### Task 31 — Offset and value window functions

**Goal:** Add position-sensitive access to rows within a partition or frame.

**DoD:**

- `lag`, `lead`, `first_value`, `last_value`, and `nth_value` support PostgreSQL
  arguments, defaults, coercion, timing, and NULL behavior.
- `lag`/`lead` use the partition while value functions use the active frame.
- Unsupported NULL-treatment and direction syntax is rejected, not ignored.
- Differential/property cases cover edges, offsets, defaults, NULLs, peers,
  parameters, metadata, and errors.

### Task 32 — Aggregate windows and frame semantics

**Goal:** Complete existing aggregates over PostgreSQL window frames.

**DoD:**

- Core aggregates run as windows with correct result types, NULL behavior,
  default frames, and peer handling.
- `ROWS`, `RANGE`, and `GROUPS` support legal unbounded/current/offset bounds
  and represented `EXCLUDE` forms.
- Invalid bounds, ordering requirements, nesting, and unsupported modifiers
  produce compatible errors.
- Differential/property tests cover all frame modes, moving aggregates, peers,
  NULLs, exclusions, and final filtering/order; benchmarks cover ranking and
  moving aggregates.

---

## Milestone J — Arrays

### Task 33 — One-dimensional array type and I/O

**Goal:** Add one-dimensional arrays of supported scalar PostgreSQL types.

**DoD:**

- `BaseType`, `PgType`, and `Value` represent typed arrays, NULL elements, and
  empty arrays with correct array/element OIDs through native and SQLx APIs.
- Literals, `ARRAY[...]`, text I/O, parameters, defaults, casts, common-element
  resolution, equality, and lexicographic ordering match PostgreSQL.
- Unsupported dimensions/lower bounds/elements and malformed values fail
  explicitly; differential/property tests cover I/O, coercion, comparison,
  metadata, NULLs, and errors.

### Task 34 — Array subscripting, operators, aggregates, and functions

**Goal:** Support common PostgreSQL array expressions end to end.

**DoD:**

- One-based reads/assignments, concatenation, containment, overlap, and
  `ANY(array)`/`ALL(array)` match PostgreSQL bounds, duplicate, and NULL rules.
- The planned array inspection/mutation functions, `array_agg`, and supported
  `unnest` positions work; unsupported table-function forms fail loudly.
- Differential/property tests cover empty/NULL arrays, NULL elements,
  expansion, duplicates, coercion, aggregation, and errors; benchmarks cover
  containment and `array_agg`.

---

## Milestone K — SERIALIZABLE isolation

### Task 35 — SERIALIZABLE dependency tracking

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

### Task 36 — SSI validation and predicate conflicts

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

## Milestone L — Phase 3 conformance and release gate

### Task 37 — Phase 3 integration, regression audit, and benchmarks

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
- The Task 26 SQLx application-workload gate remains green and is reported
  separately from the broader synthetic/upstream conformance results.

**Notes:** This task fixes integration defects but does not add new SQL families.

### Task 38 — External PostgreSQL-driven fuzzing

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

- All 38 tasks meet their DoD and have been approved before being marked
  complete.
- The prioritized migration and SQLx application-workload gates in Tasks 20
  and 26 pass without unsupported, unclassified, or silently skipped SQL.
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
  views, rules, general-purpose PL/pgSQL/triggers/schema/index/`ALTER TABLE`
  beyond the explicitly planned subset, privileges, `MERGE`, grouping sets,
  broader table functions/`LATERAL`, generated columns, new type families,
  extensions, procedures, server-level database management, and wire-protocol
  support.
