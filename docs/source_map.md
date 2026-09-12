# Source map

The native engine lives in `crates/pg_fake`. `pg_fake_sqlx` adapts its session
API to SQLx, `pg_fake_cli` provides the SQL shell, `pg_fake_benchmarks` measures
workloads, and `xtask` contains development commands.

## Database and sessions

[`lib.rs`](../crates/pg_fake/src/lib.rs) exposes the public types directly:
`use pg_fake::{Db, Session, Transaction};`. Implementation modules are private
and named after their responsibilities.

| Module | Responsibility |
| --- | --- |
| [`database.rs`](../crates/pg_fake/src/database.rs) | Database construction, shared engine state, mock time, and random seed configuration. |
| [`results.rs`](../crates/pg_fake/src/results.rs) | Result rows, column metadata, and affected-row counts. |
| [`catalog_inspection.rs`](../crates/pg_fake/src/catalog_inspection.rs) | Stable catalog descriptions for diagnostics and differential comparisons. |
| [`session/mod.rs`](../crates/pg_fake/src/session/mod.rs) | Session state, statement batches, snapshots, and execution order. |
| [`session/prepared.rs`](../crates/pg_fake/src/session/prepared.rs) | Parameter inference and binding, prepared handles, and cached read-lock requirements. |
| [`session/catalog_dependencies.rs`](../crates/pg_fake/src/session/catalog_dependencies.rs) | Discover tables, views, sequences, and constraints referenced by SQL; validate saved dependencies against the visible catalog. |
| [`session/transactions.rs`](../crates/pg_fake/src/session/transactions.rs) | SQL transaction control, the transaction guard, commit/abort cleanup, version reclamation, and session teardown. |
| [`session/settings.rs`](../crates/pg_fake/src/session/settings.rs) | Session settings, parsing their values, and saving/restoring settings around transactions. |
| [`session/constraint_timing.rs`](../crates/pg_fake/src/session/constraint_timing.rs) | `SET CONSTRAINTS` and switching deferred checks to immediate checks. |
| [`session/do_block.rs`](../crates/pg_fake/src/session/do_block.rs) | Anonymous PL/pgSQL blocks: local variables, control flow, and nested SQL sharing the block's deadline and statement timestamp. |

Catalog dependencies serve both prepared statements and lock discovery.
Keeping them independent of prepared handles lets ordinary queries and view
creation use the same catalog traversal and CTE scoping rules.

## Query execution

[`executor/query/mod.rs`](../crates/pg_fake/src/executor/query/mod.rs) coordinates
query validation, chooses the row-evaluation path, and applies final result
ordering, distinctness, and limits.

- [`select.rs`](../crates/pg_fake/src/executor/query/select.rs) evaluates ordinary
  SELECT rows and membership filters, including deferred projection evaluation.
- [`streaming.rs`](../crates/pg_fake/src/executor/query/streaming.rs) resumes
  query execution across consumer requests. `QueryStreamState` retains progress
  through unordered, ordered, grouped, `UNION ALL`, and materialized results.
- [`projection.rs`](../crates/pg_fake/src/executor/query/projection.rs) binds
  `SELECT` and `RETURNING` outputs, describes their columns, and evaluates values.
- [`ordering.rs`](../crates/pg_fake/src/executor/query/ordering.rs) resolves
  `ORDER BY` keys and performs sorting and bounded top-row selection.
- [`distinct.rs`](../crates/pg_fake/src/executor/query/distinct.rs) validates
  `DISTINCT` and `DISTINCT ON`, reuses output/order keys, and removes duplicates.
- [`limits.rs`](../crates/pg_fake/src/executor/query/limits.rs) resolves
  `LIMIT` and `OFFSET`, including NULL, `ALL`, and invalid row counts.
- [`expressions.rs`](../crates/pg_fake/src/executor/query/expressions.rs) compares
  bound expressions, prunes constant CASE branches, detects volatility, and
  substitutes aggregate values before expression evaluation.
- [`values.rs`](../crates/pg_fake/src/executor/query/values.rs) binds and executes
  `VALUES` row constructors, including column types, ordering, and row limits.
- [`windows.rs`](../crates/pg_fake/src/executor/query/windows.rs) collects window
  expressions belonging to the current query, computes their values over filtered
  rows, and substitutes them into projection, ordering, and distinct expressions.
- [`grouping/mod.rs`](../crates/pg_fake/src/executor/query/grouping/mod.rs) collects
  groups, evaluates aggregates and `HAVING`, and keeps volatile aggregate
  occurrences tied to their owning expressions, including deferred projections.
  [`validation.rs`](../crates/pg_fake/src/executor/query/grouping/validation.rs)
  resolves grouped expressions and primary-key dependencies;
  [`visitation.rs`](../crates/pg_fake/src/executor/query/grouping/visitation.rs)
  implements the engine's PostgreSQL group visitation ordering.
- [`set_operations.rs`](../crates/pg_fake/src/executor/query/set_operations.rs)
  describes and combines operands of `UNION`, `INTERSECT`, and `EXCEPT`. It owns
  operand type resolution, duplicate handling, and ordering/limiting the combined
  result. Recursive CTEs and streamed `UNION ALL` reuse its row and type operations.

The query coordinator delegates CTE handling to `executor/ctes` and source-row
production to `executor/from`.

[`executor/from/mod.rs`](../crates/pg_fake/src/executor/from/mod.rs) combines `FROM`
sources, including derived tables and JSON expansion. Queries and mutations share
this source-row interface. Its [`scans.rs`](../crates/pg_fake/src/executor/from/scans.rs)
owns visible table scans, index lookups, and predicate pushdown;
[`joins.rs`](../crates/pg_fake/src/executor/from/joins.rs) owns join conditions,
hash and nested-loop evaluation, and unmatched rows in outer joins.

[`executor/equality.rs`](../crates/pg_fake/src/executor/equality.rs) holds the
hashable equality keys shared by joins and membership predicates. NULL has no
such key. Its `are_rows_not_distinct` predicate separately expresses the NULL
equality used by grouping and duplicate handling.

[`executor/subqueries.rs`](../crates/pg_fake/src/executor/subqueries.rs) evaluates
scalar, `EXISTS`, `IN`, `ANY`, and `ALL` subqueries, preserving result types and
reusing results prepared during lock discovery. Correlated expressions first
substitute values from the outer row.

[`executor/outer_references.rs`](../crates/pg_fake/src/executor/outer_references.rs)
resolves those outer references while respecting inner scopes and output aliases.
Procedural SQL uses the same traversal with an explicit ambiguity policy;
[`procedural.rs`](../crates/pg_fake/src/executor/procedural.rs) supplies its variable
scope and statement-specific binding rules.

## Common table expressions

[`executor/ctes/mod.rs`](../crates/pg_fake/src/executor/ctes/mod.rs) materializes
read CTEs and exposes the entry points used by statement analysis, query execution,
and lock preparation. CTEs belong beside the query executor because a `WITH`
clause can also contain or feed mutations.

- [`analysis.rs`](../crates/pg_fake/src/executor/ctes/analysis.rs) expands CTEs for
  binding, parameter inference, and dependency discovery without executing them.
- [`references.rs`](../crates/pg_fake/src/executor/ctes/references.rs) discovers
  dependencies, checks forward references, and replaces materialized CTE references
  while respecting nested names and preserving column names and types.
- [`recursive.rs`](../crates/pg_fake/src/executor/ctes/recursive.rs) validates
  recursion and column types, schedules read dependencies in `WITH RECURSIVE`, and
  iterates the working table with duplicate handling and row demand.
- [`mutations.rs`](../crates/pg_fake/src/executor/ctes/mutations.rs) prepares
  mutation dependencies for locking and materializes statement CTEs. It reuses
  results saved in the statement execution context so lock preparation and
  execution share evaluations, and runs required mutations even when the outer
  query needs no rows.

## Row mutations

[`executor/writes/mod.rs`](../crates/pg_fake/src/executor/writes/mod.rs) exposes
INSERT, UPDATE, and DELETE execution and shares assignment binding and coercion.

- [`insert.rs`](../crates/pg_fake/src/executor/writes/insert.rs) applies prepared
  inserts, records affected rows, and checks foreign keys.
- [`insert_preparation.rs`](../crates/pg_fake/src/executor/writes/insert_preparation.rs)
  evaluates source rows, defaults, BEFORE triggers, and RETURNING before applying
  inserts. It retains source snapshots and cached evaluations across lock waits.
- [`conflicts.rs`](../crates/pg_fake/src/executor/writes/conflicts.rs) resolves
  `ON CONFLICT` arbiters, binds the target and `excluded` scopes, and prepares or
  applies conflict updates while enforcing the once-per-row rule.
- [`update.rs`](../crates/pg_fake/src/executor/writes/update.rs) prepares and applies
  UPDATE assignments and triggers, then checks constraints and referencing rows.
- [`delete.rs`](../crates/pg_fake/src/executor/writes/delete.rs) deletes selected
  versions and applies referencing foreign-key actions.
- [`targets.rs`](../crates/pg_fake/src/executor/writes/targets.rs) binds mutation
  scopes, selects target versions using FROM or USING, and prepares CTE row locks.
  It tracks rows already changed by the same command.
- [`returning.rs`](../crates/pg_fake/src/executor/writes/returning.rs) binds and
  evaluates RETURNING and constructs query or affected-row results.

## Locking

[`session/locking`](../crates/pg_fake/src/session/locking/mod.rs) coordinates
lock acquisition with statement execution. Its entry points acquire relation or
row locks, release the database mutex while waiting, and refresh snapshots when
required by the isolation level.

- [`relations.rs`](../crates/pg_fake/src/session/locking/relations.rs) collects
  relation lock requirements for queries, mutations, and explicit `LOCK TABLE`.
- [`ddl.rs`](../crates/pg_fake/src/session/locking/ddl.rs) handles schema
  changes and their dependent objects.
- [`foreign_keys.rs`](../crates/pg_fake/src/session/locking/foreign_keys.rs)
  follows foreign-key checks and cascading mutations to related tables.

[`executor/locks.rs`](../crates/pg_fake/src/executor/locks.rs) discovers the actual
rows an operation must lock. [`txn.rs`](../crates/pg_fake/src/txn.rs) owns lock
compatibility, queues, the wait-for graph, transaction status, and visibility.
These separate the SQL requirements, blocking execution, and lock bookkeeping.

## Following a statement

1. A session parses SQL through [`parser.rs`](../crates/pg_fake/src/parser.rs).
   Parameterized calls use the prepared-statement path and
   [`analyzer.rs`](../crates/pg_fake/src/analyzer.rs).
2. The session handles settings and transaction commands, or chooses a snapshot
   for ordinary execution. SQL inside a `DO` block re-enters session execution.
3. It acquires relation locks and then validates prepared dependencies against
   the catalog visible after any wait.
4. It executes a prepared query plan when available; otherwise it coordinates
   CTEs, subqueries, row locks, and the general
   [`executor`](../crates/pg_fake/src/executor/mod.rs).
5. Commit or rollback finalizes catalog and row versions, restores settings,
   releases locks, and wakes waiting sessions.

The executor implements SQL operations; [`catalog.rs`](../crates/pg_fake/src/catalog.rs)
holds object definitions and their transactional history;
[`storage.rs`](../crates/pg_fake/src/storage.rs) holds row versions and indexes.
[`coercion.rs`](../crates/pg_fake/src/coercion.rs) centralizes cast rules.

The session's existing behavior and concurrency tests are in
[`session/tests.rs`](../crates/pg_fake/src/session/tests.rs). Public API
integration tests live in `crates/pg_fake/tests`; SQLx and PostgreSQL differential
tests live in `crates/pg_fake_sqlx/tests`.
