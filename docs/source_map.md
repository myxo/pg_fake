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
| [`database/mod.rs`](../crates/pg_fake/src/database/mod.rs) | Database construction, mock time, and random seed configuration. |
| [`database/state.rs`](../crates/pg_fake/src/database/state.rs) | Shared catalog, table storage, transactions, locks, and sequence values; visible-catalog caching and touched-table tracking. |
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

## Prepared-statement analysis

[`analyzer/mod.rs`](../crates/pg_fake/src/analyzer/mod.rs) coordinates parameter
analysis, validates typed statements, and binds supplied values to placeholders.

- [`parameter_types/mod.rs`](../crates/pg_fake/src/analyzer/parameter_types/mod.rs)
  applies statement-level type expectations and finalizes unconstrained parameters.
  [`queries.rs`](../crates/pg_fake/src/analyzer/parameter_types/queries.rs) propagates
  expectations through SELECT, VALUES, set operations, joins, and FROM sources;
  [`expressions.rs`](../crates/pg_fake/src/analyzer/parameter_types/expressions.rs)
  constrains individual placeholders from operators and function arguments.
- [`validation.rs`](../crates/pg_fake/src/analyzer/validation.rs) validates
  assignments, predicates, RETURNING, and query clauses after parameter binding.
- [`subqueries.rs`](../crates/pg_fake/src/analyzer/subqueries.rs) substitutes typed
  subquery placeholders while respecting the surrounding statement scope.
- [`literals.rs`](../crates/pg_fake/src/analyzer/literals.rs) constructs typed SQL
  literals and casts, preserving PostgreSQL type modifiers.
- [`scopes.rs`](../crates/pg_fake/src/analyzer/scopes.rs) shares mutation scopes
  and projection-alias recognition among the analysis passes.

## Column scopes and binding

[`executor/scope/mod.rs`](../crates/pg_fake/src/executor/scope/mod.rs) defines bound
columns and row scopes. It resolves qualified and unqualified names, detects
ambiguity, selects wildcard outputs, and reads merged JOIN/USING columns.

- [`sources.rs`](../crates/pg_fake/src/executor/scope/sources.rs) binds tables,
  views, derived queries, and JSON table functions to column slots. It handles
  aliases and carries outer scopes into nested queries.
- [`joins.rs`](../crates/pg_fake/src/executor/scope/joins.rs) binds join inputs,
  validates ON/USING/NATURAL conditions, merges common columns, and checks lateral
  references in RIGHT and FULL joins.
- [`output.rs`](../crates/pg_fake/src/executor/scope/output.rs) describes query
  output names and types, including unknown literals and set-operation operands.
- [`subqueries.rs`](../crates/pg_fake/src/executor/scope/subqueries.rs) infers
  expression types with correlated subqueries and substitutes typed placeholders
  without evaluating query rows.

## Statement state

[`executor/context.rs`](../crates/pg_fake/src/executor/context.rs) owns
`StatementContext`: statement timestamps, timeout, random and sequence execution,
source snapshots, and cached evaluations shared by locking and execution. INSERT
preparations, UPDATE rows, mutation targets, CTE results, and subquery results keep
their existing occurrence and snapshot keys. Row-lock recheck requests preserve
progress when execution must wait.

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

[`lateral.rs`](../crates/pg_fake/src/executor/lateral.rs) binds correlated derived
sources to each preceding FROM row, preserves projected names, and gives set
operation branches their own query scopes. Correlated invocations use separate
derived-result caches so repeated outer values preserve volatile evaluations.
[`initplans.rs`](../crates/pg_fake/src/executor/lateral/initplans.rs) identifies
independent scalar and CTE occurrences before binding outer values and shares
their lazily evaluated results, including resumable recursive CTE rows.

[`executor/equality.rs`](../crates/pg_fake/src/executor/equality.rs) holds the
hashable equality keys shared by joins and membership predicates. NULL has no
such key. Its `are_rows_not_distinct` predicate separately expresses the NULL
equality used by grouping and duplicate handling.

[`executor/subqueries.rs`](../crates/pg_fake/src/executor/subqueries.rs) evaluates
scalar, `EXISTS`, `IN`, `ANY`, and `ALL` subqueries, preserving result types and
reusing results prepared during lock discovery. Correlated expressions first
substitute values from the outer row.
[`conditionals.rs`](../crates/pg_fake/src/executor/subqueries/conditionals.rs)
evaluates only the selected conditional branches when they contain subqueries.

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

## Scalar expressions and row rules

[`executor/expressions/mod.rs`](../crates/pg_fake/src/executor/expressions/mod.rs)
recursively evaluates scalar expressions and applies expression-level coercion.
Its [`types.rs`](../crates/pg_fake/src/executor/expressions/types.rs) resolves
expression and operator types;
[`literals.rs`](../crates/pg_fake/src/executor/expressions/literals.rs) parses
literals and identifies unknown strings, NULL, and placeholders;
[`functions.rs`](../crates/pg_fake/src/executor/expressions/functions.rs) handles
function signatures and values, including window-call validation;
[`comparisons.rs`](../crates/pg_fake/src/executor/expressions/comparisons.rs)
handles ordering, comparison eligibility, and row/list membership.

[`runtime.rs`](../crates/pg_fake/src/executor/expressions/runtime.rs) shares scalar
runtime signatures with parameter inference and dispatches temporal, floor, and
regex functions. [`temporal.rs`](../crates/pg_fake/src/executor/expressions/temporal.rs)
implements epoch conversion and bounded timestamp formatting/truncation.
[`coercion/time_zones.rs`](../crates/pg_fake/src/coercion/time_zones.rs) resolves
zones and supplies conversions shared by casts and `AT TIME ZONE`. Execution
passes the session time zone into central coercion. [`patterns.rs`](../crates/pg_fake/src/executor/expressions/patterns.rs)
implements LIKE escaping and the supported ASCII regular-expression predicates.
The exact runtime scope is recorded under Task 21 in `plan.md` and exercised by
`tests/fixtures/runtime_expressions.sql` in the SQLx crate.

[`column_defaults.rs`](../crates/pg_fake/src/executor/column_defaults.rs) validates
and evaluates column defaults, including sequence allocation.
[`row_constraints.rs`](../crates/pg_fake/src/executor/row_constraints.rs) checks
NOT NULL and CHECK constraints. Partial-index predicates live with index
operations in [`indexes.rs`](../crates/pg_fake/src/executor/indexes.rs).

## JSON expressions and table functions

[`executor/json/mod.rs`](../crates/pg_fake/src/executor/json/mod.rs) exposes JSON
operator typing, scalar functions, and table-function expansion.

- [`operators.rs`](../crates/pg_fake/src/executor/json/operators.rs) resolves and
  evaluates JSON operators, including extraction, existence, and containment.
- [`functions.rs`](../crates/pg_fake/src/executor/json/functions.rs) validates
  scalar function arguments and evaluates constructors, conversions, and jsonb_set.
- [`expansion.rs`](../crates/pg_fake/src/executor/json/expansion.rs) recognizes
  JSON functions in FROM, describes their columns, and produces rows with optional
  ordinality.
- [`paths.rs`](../crates/pg_fake/src/executor/json/paths.rs) resolves array indexes
  and applies path replacements or deletions shared by operators and jsonb_set.
- [`text.rs`](../crates/pg_fake/src/executor/json/text.rs) reads JSON text while
  preserving object entries and raw values, validates strings, and constructs
  SQL values. JSONB normalization remains in
  [`jsonb.rs`](../crates/pg_fake/src/jsonb.rs).

## Schema changes

[`executor/mod.rs`](../crates/pg_fake/src/executor/mod.rs) dispatches statements to
feature executors after checking the statement deadline.
[`table_ddl.rs`](../crates/pg_fake/src/executor/table_ddl.rs) creates and drops
tables, binds defaults and generated sequences, and supplies definition helpers
shared with [`alter_table/mod.rs`](../crates/pg_fake/src/executor/alter_table/mod.rs).
[`sequence_ddl.rs`](../crates/pg_fake/src/executor/sequence_ddl.rs) creates and drops
sequences, including ownership and dependency checks; runtime sequence allocation
remains in [`sequences.rs`](../crates/pg_fake/src/executor/sequences.rs).

## Table alterations

[`executor/alter_table/mod.rs`](../crates/pg_fake/src/executor/alter_table/mod.rs)
coordinates table rewrites: captures visible rows, applies operations in order,
updates catalog and row versions, validates the resulting rows, and cleans up
new sequences if the alteration fails.

- [`operations.rs`](../crates/pg_fake/src/executor/alter_table/operations.rs)
  dispatches individual ALTER TABLE operations and validates the schema after each.
- [`columns.rs`](../crates/pg_fake/src/executor/alter_table/columns.rs) builds
  column definitions, allocates serial/identity sequences, and changes defaults,
  nullability, or types with optional USING expressions.
- [`constraints.rs`](../crates/pg_fake/src/executor/alter_table/constraints.rs)
  builds named table and foreign-key constraints, including NOT VALID metadata.
- [`dependencies.rs`](../crates/pg_fake/src/executor/alter_table/dependencies.rs)
  checks dependent views and foreign keys, removes dependent objects, and updates
  stored column references during renames.

## Stored views

[`executor/views/mod.rs`](../crates/pg_fake/src/executor/views/mod.rs) handles
CREATE, DROP, and COMMENT ON VIEW.

- [`binding.rs`](../crates/pg_fake/src/executor/views/binding.rs) binds stored
  references to catalog objects, records table/sequence/constraint dependencies,
  and checks view dependency cycles.
- [`column_dependencies.rs`](../crates/pg_fake/src/executor/views/column_dependencies.rs)
  tracks referenced columns through aliases, wildcards, and natural/USING joins.
- [`expansion.rs`](../crates/pg_fake/src/executor/views/expansion.rs) expands
  nested views into queries while preserving their declared output columns.
- [`references.rs`](../crates/pg_fake/src/executor/views/references.rs) preserves
  stored view references and column positions across table/column renames and
  drops of unreferenced columns.
- [`cte_scope.rs`](../crates/pg_fake/src/executor/ctes/scope.rs) tracks CTE
  names that hide catalog relations during binding and reference traversal.

Trigger creation, renaming, and removal live together in
[`executor/procedural.rs`](../crates/pg_fake/src/executor/procedural.rs).

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

[`executor/locks/mod.rs`](../crates/pg_fake/src/executor/locks/mod.rs) discovers
rows to lock, retains mutation candidates, and checks concurrent row changes.

- [`ctes.rs`](../crates/pg_fake/src/executor/locks/ctes.rs) discovers locks for
  reachable CTEs while preserving pending mutation and row-lock recheck state.
- [`insert.rs`](../crates/pg_fake/src/executor/locks/insert.rs) discovers conflict
  rows and locks required by prepared trigger results, with conservative fallbacks
  when evaluating insert sources early would change behavior.
- [`foreign_keys.rs`](../crates/pg_fake/src/executor/locks/foreign_keys.rs) finds
  referenced rows that need share locks for inserted or updated values.

Unique point-lookup recognition lives with scans in
[`executor/from/scans.rs`](../crates/pg_fake/src/executor/from/scans.rs), shared by
lock discovery and mutation targeting.
[`txn.rs`](../crates/pg_fake/src/txn.rs) owns lock compatibility, queues, the
wait-for graph, transaction status, and visibility. These separate the SQL
requirements, blocking execution, and lock bookkeeping.

## Catalog objects and visibility

[`catalog/mod.rs`](../crates/pg_fake/src/catalog/mod.rs) owns schemas, the visible
object maps, catalog identity used by caches, and dependency updates spanning
multiple object kinds. Each object module keeps its definitions with its catalog
operations:

- [`names.rs`](../crates/pg_fake/src/catalog/names.rs) resolves relation names,
  creation namespaces, and temporary-schema precedence.
- [`tables.rs`](../crates/pg_fake/src/catalog/tables.rs) defines tables, columns,
  indexes, and triggers, and looks up, creates, replaces, or removes tables.
- [`constraints.rs`](../crates/pg_fake/src/catalog/constraints.rs) defines
  constraints and maintains foreign-key dependency and deferral metadata.
- [`sequences.rs`](../crates/pg_fake/src/catalog/sequences.rs) manages sequence
  definitions, ownership, and removal dependencies. Sequence values live in
  `executor/sequences.rs` because allocation survives transaction rollback.
- [`views.rs`](../crates/pg_fake/src/catalog/views.rs) manages view definitions,
  output columns, and view dependencies.
- [`functions.rs`](../crates/pg_fake/src/catalog/functions.rs) manages function
  definitions and preserves their identities when replacing them.

[`catalog/history.rs`](../crates/pg_fake/src/catalog/history.rs) owns transactional
catalog versions. Its interface materializes the catalog for a snapshot, records
DDL changes, discards aborted changes, and reclaims obsolete versions. Visibility
keys track catalog and pruning generations; temporary objects retain their
session ownership. Version chains and their visibility rules are private to this
module.

## Following a statement

1. A session parses SQL through [`parser.rs`](../crates/pg_fake/src/parser.rs).
   Parameterized calls use the prepared-statement path and
   [`analyzer/mod.rs`](../crates/pg_fake/src/analyzer/mod.rs).
2. The session handles settings and transaction commands, or chooses a snapshot
   for ordinary execution. SQL inside a `DO` block re-enters session execution.
3. It acquires relation locks and then validates prepared dependencies against
   the catalog visible after any wait.
4. It executes a prepared query plan when available; otherwise it coordinates
   CTEs, subqueries, row locks, and the general
   [`executor`](../crates/pg_fake/src/executor/mod.rs).
5. Commit or rollback finalizes catalog and row versions, restores settings,
   releases locks, and wakes waiting sessions.

The executor implements SQL operations; [`catalog/mod.rs`](../crates/pg_fake/src/catalog/mod.rs)
holds object definitions and visible object lookups;
[`storage.rs`](../crates/pg_fake/src/storage.rs) holds row versions and indexes.
[`coercion.rs`](../crates/pg_fake/src/coercion.rs) centralizes cast rules.

The session's existing behavior and concurrency tests are in
[`session/tests.rs`](../crates/pg_fake/src/session/tests.rs). Public API
integration tests live in `crates/pg_fake/tests`; SQLx and PostgreSQL differential
tests live in `crates/pg_fake_sqlx/tests`.
