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
query execution, including SELECT, grouping, windows, CTEs, and streaming.

- [`values.rs`](../crates/pg_fake/src/executor/query/values.rs) binds and executes
  `VALUES` row constructors, including column types, ordering, and row limits.
- [`set_operations.rs`](../crates/pg_fake/src/executor/query/set_operations.rs)
  describes and combines operands of `UNION`, `INTERSECT`, and `EXCEPT`. It owns
  operand type resolution, duplicate handling, and ordering/limiting the combined
  result. Recursive CTEs and streamed `UNION ALL` reuse its row and type operations.

The recursive CTE iteration and demand-driven streaming paths stay in the query
coordinator. The shared `are_rows_not_distinct` predicate makes the NULL equality
used by grouping and duplicate handling explicit.

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
