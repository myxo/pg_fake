# Simple-query trace analysis

## Scope and method

The traces contain function entry and exit events, not timings. Therefore call
count alone is not evidence that a function is expensive. This report only
flags work when the trace and implementation together show that its amount can
grow with rows, columns, tables, AST size, or repeated executions and when that
work can be removed, moved to planning, or replaced with a better algorithm.

The fixture has three tables and the traced statement runs through
`Session::execute`, so each trace includes parsing and an implicit transaction.
Operations are reviewed separately before drawing cross-operation conclusions.

## `select_null`

Query:

```sql
SELECT id FROM users WHERE manager_id IS NULL
```

The table has three rows and five columns. Two rows pass the predicate.

### Finding: expression binding and subquery detection happen per row

Confidence: high. Priority: high.

For each input row, `evaluate_query_expression` calls `contains_subquery`, which
walks the expression AST with `SubqueryDetector`. It does this for the `WHERE`
expression on all three rows and for the projection expression on both output
rows: five complete subquery checks for a query whose expressions cannot change
during execution.

Column lookup is also repeated during evaluation. Resolving `manager_id` and
`id` calls `resolve_bound_column`, which linearly scans the five-column bound
scope and repeatedly calls `matches_identifier`. The trace contains six
`resolve_bound_column` calls and thirty `matches_identifier` calls. One lookup
belongs to projection planning; the other five are execution-time lookups, one
for every evaluated column reference.

Both properties are static. A general bound expression should encode column
slots and whether a subquery is present once. Execution should interpret that
bound form directly. This is broader than extending the current narrow
`PreparedQueryPlan`: the same bound form should serve one-shot execution after
one planning pass and prepared execution across calls. Complexity then changes
from roughly `rows * (expression AST + scope columns)` metadata work to one
planning pass plus O(1) slot reads per column reference.

### Finding: a read-only autocommit uses the write-commit path

Confidence: high. Priority: high when a database has many tables.

After the SELECT, `Session::execute` calls `commit_transaction`. The trace shows
`commit_transaction_versions` three times and `prune_versions` three times,
once for every table in the fixture. The implementation iterates all tables for
both phases even though this transaction cannot have created a row version.

There is already a suitable fast path: optimized prepared reads call
`commit_read_only_transaction`, which only finishes the transaction registry
entry. The ordinary executor should track whether the transaction performed a
mutation and use the read-only finish path when it did not. This avoids O(number
of tables), and potentially reclamation-related, work after every read. Merely
checking whether each table has pending entries is insufficient because the
outer all-table traversal remains.

### Finding: multiple whole-AST planning passes run before execution

Confidence: high that the passes exist; medium on the best consolidation.
Priority: medium after binding expressions.

Even without a CTE or subquery, the statement is cloned and traversed by CTE
materialization, then cloned and traversed again by uncorrelated-subquery
materialization. The latter also binds the query scope, and `execute_query`
binds the same scope again. Aggregate inspection performs another expression
clone/substitution/visitor pass, followed by separate predicate type inference,
projection planning, order planning, distinct planning, and grouping planning.

These should become one analysis/planning pipeline that produces a reusable
query plan. At minimum, cheap top-level feature checks can skip CTE and subquery
materializers when those constructs are absent, and the scope built while
analyzing subqueries should not be discarded and rebuilt. A single visitor may
collect subquery/aggregate/column-reference facts, but it should only replace
separate visitors where their scope and error-order semantics are compatible.

### Finding: CTE materialization clones the AST even on the no-CTE path

Confidence: high. Priority: medium for large statements, low for this small
query.

`materialize_query_ctes` starts with `query.clone()`. When `WITH` is absent it
still visits the clone looking for derived CTE references and returns the
cloned query; `materialize_ctes` then converts it back into a statement.
Uncorrelated-subquery materialization clones the statement again. This is
avoidable full-AST work for the common simple-query path. It is worth fixing as
part of the planning pipeline, but not as an isolated micro-optimization ahead
of the per-row binding and read-only commit issues.

### Not considered a useful target from this trace

The executor calls empty order-key and distinct-key helpers for each output row
and calls the final sort helper with no `ORDER BY`. These are constant-time
branches on this query. Special-casing them may reduce calls but does not change
the relevant scaling behavior, so they should not drive the optimization work.

## `select_filter`

Query:

```sql
SELECT id, name FROM users WHERE active = true
```

The table has three rows. Two pass the predicate and produce two projected
columns each.

### Finding: the pushed-down predicate is evaluated twice for passing rows

Confidence: high. Priority: high.

`visit_table_factor_rows` identifies `active = true` as a filter belonging to
the `users` factor and evaluates it while scanning all three rows. Rows that
pass are then sent to the callback in `execute_plain_select_rows`, which calls
`evaluate_where_clause` with the original complete predicate. Consequently the
filter is evaluated once for rejected rows and twice for accepted rows. The
trace shows three direct evaluation paths under the table scan and two more
under `evaluate_where_clause`.

Planning should split a predicate into pushed-down conjuncts and a residual
predicate. A conjunct fully evaluated by the scan must be removed from the
residual expression. This matters more as predicate expressions become larger
or more selective and also prevents the per-row analysis problem below from
being multiplied. Care is required for volatile expressions and SQL error
ordering; only predicates that are safe to push and evaluate once should be
moved.

### Finding: type resolution and coercion planning are repeated for every row

Confidence: high. Priority: high.

Evaluating the binary predicate does substantially more than compare two
values. Every evaluation calls `infer_expression_type` for both operands,
`resolve_operator_type`, `resolve_expression_pair_type`, and common-type
resolution. `evaluate_and_coerce` then infers each operand type again before
coercing its value. This trace contains 45 `infer_expression_type` calls, 62
`extract_ast_value` calls, and 175 `matches_identifier` calls for three input
rows and four projected values. Most of that is static type and name analysis,
not value-dependent execution.

The bound expression proposed for `select_null` should store the selected
operator implementation, operand slots/literals, source and target types, and
required casts. Runtime evaluation should load values, apply the already chosen
casts, and invoke the chosen operator. This converts repeated AST/type analysis
into one planning cost and is likely the most important general optimization in
the simple SELECT traces.

### Finding: projection columns use the generic expression evaluator

Confidence: high. Priority: medium; naturally solved by general binding.

`build_projection_plan` resolves `id` and `name`, but records ordinary
identifier projections as `ProjectionSource::Expression`. Execution therefore
runs subquery detection and name resolution again for every output value.
`ProjectionSource::Column` already exists and is used for wildcard expansion;
resolved identifier projections can use the same direct-slot representation.
For this trace that would remove four subquery walks and four scope scans even
before a complete bound-expression representation exists.

### Findings inherited from `select_null`

The no-op CTE/subquery materialization passes, repeated scope binding, and
all-table write-commit path are present unchanged. They are not duplicated here
as separate recommendations.

## `select_expression`

Query:

```sql
SELECT id, score + 1 AS next_score FROM users
```

All three rows are projected.

### Finding: literals are parsed and arithmetic is type-planned per row

Confidence: high. Priority: high as part of bound expressions.

The integer literal `1` is parsed three times while building projection
metadata, then five times for each row while executing `score + 1`: during
several type-resolution paths, coercion, and value evaluation. The trace has
eighteen `parse_integer_literal` calls for one literal. It also has 31
`infer_expression_type` calls even though neither the expression nor its types
can vary between rows.

Literal parsing and numeric operator selection belong in analysis. A bound
arithmetic node should contain `Value::Int4(1)`, the resolved input/result
types, any casts, and the selected arithmetic operation. Execution should not
reparse the AST literal or rediscover the common type. This is the same
architectural fix identified in `select_filter`, with especially direct
evidence that constant folding/compilation is currently absent.

This does not imply folding the whole expression: `score` is row-dependent.
Only the literal and static operator/type decisions should be prepared.

### Finding: already-resolved plain projection still rebinds per row

Confidence: high. Priority: medium independently, high as part of binding.

The `id` projection is resolved while building output metadata, yet remains an
AST expression and is resolved again for all three rows. Together with `score`
lookups, the trace records six runtime `resolve_column_value` calls and repeated
linear scope searches. Recording `id` and `score` as slots in the plan removes
this work.

### Findings inherited from earlier SELECTs

Subquery detection runs once for each of the six projected values, despite no
subquery being present. No-op statement materialization, repeated analysis
passes, and the all-table write-commit path also remain. There is no duplicated
filter evaluation because this query has no `WHERE` clause.

## `select_order_limit`

Query:

```sql
SELECT id, name FROM users ORDER BY score DESC LIMIT 2
```

All three rows are scanned and assigned an order key; two are returned.

### Finding: `ORDER BY ... LIMIT` fully sorts all candidate rows

Confidence: high. Priority: high for larger test fixtures.

`execute_plain_select_rows` collects every candidate into `Vec<OrderedRow>`.
`finalize_select_rows` then sorts the complete vector and only afterward applies
the offset and limit. This is O(n log n) comparisons and O(n) retained rows even
when only a small prefix is requested.

When there is no DISTINCT interaction requiring a different order of
operations, planning can select a bounded top-k operator where
`k = offset + limit`. A heap gives O(n log k) time and O(k) retained rows,
followed by a final sort of the selected rows. For this three-row trace the
difference is immaterial, but it is an algorithmic improvement for the exact
operation represented by the trace. Cases involving `DISTINCT`/`DISTINCT ON`
must preserve their current semantic ordering and may require a different
plan.

### Finding: the order expression is rebound for each row

Confidence: high. Priority: high as part of bound expressions.

`score` is resolved during `resolve_order_specs`, but each of the three rows
still evaluates it through `evaluate_select_expression` and
`evaluate_query_expression`, including a `contains_subquery` traversal and a
linear scope lookup. The two projected identifiers follow the same generic
path, producing nine expression/subquery checks in total. A planned order key
should store a direct input slot just as projections should.

### Finding: planning compares order and projection expressions with cloned ASTs

Confidence: high. Priority: low by itself, medium as part of a unified planner.

Grouping validation calls `compare_bound_expressions` to determine whether the
order expression matches either projected expression. Each comparison clones
and normalizes both expressions with visitors. The trace shows four
`normalize_bound_expression` calls for two comparisons. Slot-based bound
expressions make equality cheap and avoid these clones, but this fixed
per-statement work should not be optimized ahead of per-row evaluation or the
full-sort behavior.

### Findings inherited from earlier SELECTs

The no-op materialization passes and full write-commit path remain. `LIMIT 2`
itself is evaluated once, outside the row loop, which is appropriate.

## `insert_one`

Query:

```sql
INSERT INTO users (id, name, active, score)
VALUES (4, 'Dora', true, 17)
```

### Finding: simple DML is cloned twice by materialization stages that do no work

Confidence: high. Priority: high for large statements.

`materialize_ctes` immediately returns `statement.clone()` because an INSERT is
not represented here as a top-level query. The result is then passed to
`materialize_uncorrelated_subqueries`, which clones the full statement again
and walks the clone. This INSERT has neither construct, so both full-AST clones
and the visitor traversal are discarded preparation work.

The execution pipeline should first use analysis metadata to determine whether
materialization is required. Ultimately CTE/subquery handling should produce a
plan rather than successively cloning and rewriting the AST. This is a direct
example of a full clone for an operation that ends up doing nothing.

### Finding: inserting one row performs two full row clones

Confidence: high. Priority: medium, higher for wide/large values.

`execute_insert` passes `row.clone()` to `Table::insert` so it can retain the
original for foreign-key validation and `RETURNING`. `Table::insert` then makes
another `row.clone()` solely to build index entries after moving the original
row into its version. Values such as text and byte arrays therefore have their
payload copied twice.

The storage-layer clone is avoidable: build index keys from `&row` before
moving the row into the version chain. Removing the executor-level clone needs
an API that can safely reborrow the inserted row afterward; immediate
self-referential foreign keys must be able to see the newly inserted version,
so FK validation cannot simply move before insertion. `RETURNING` side-effect
and error ordering must also remain unchanged. Removing at least the internal
index clone is straightforward and does not change behavior.

### Finding: post-write bookkeeping scans the entire catalog/database

Confidence: high. Priority: high when many tables exist.

After the row is inserted, `contains_deferred_foreign_keys` scans every table
and constraint even though `users` has no foreign key. Commit then invokes
`commit_transaction_versions` and `prune_versions` for each of the fixture's
three tables, although only `users` was touched. The trace shows three calls to
each commit helper.

The catalog can maintain whether any deferred foreign key exists (or the
relevant constraint set can be derived while planning the target table).
Transaction state should track touched tables so version commit only visits
those tables. Reclamation can similarly track tables with queued candidates
instead of sweeping all tables on every commit. This changes bookkeeping from
O(all tables and constraints) toward O(touched/reclaimable tables).

### Finding: literal parsing and assignment type analysis are duplicated

Confidence: high. Priority: medium as part of DML planning.

Each numeric literal is parsed once for value evaluation and again for type
inference: `4` and `17` account for four `parse_integer_literal` calls. Target
column mapping and assignment casts are also rediscovered during execution.
A bound INSERT plan should resolve target slots and assignment coercions once
and store parsed constants. This becomes especially useful for prepared INSERTs
executed repeatedly; it is less important than the table-wide bookkeeping for
a single small VALUES row.

### Necessary work that should remain

Not-null, check, unique-index, and immediate foreign-key validation are expected
write-path work. Empty returning/default helpers contribute visible calls but
are constant-time branches and are not optimization targets by themselves.

## `insert_default`

Query:

```sql
INSERT INTO users (id, name) VALUES (5, 'Eve')
```

The omitted `active`, `score`, and `manager_id` columns use `true`, `0`, and
NULL respectively.

### Finding: catalog defaults are reparsed and retyped on every inserted row

Confidence: high. Priority: high for repeated and multi-row inserts.

`evaluate_column_default` is invoked for all three omitted columns. For the two
stored default expressions, it routes through generic assignment evaluation,
including literal extraction, type inference, cast validation, and coercion.
The default `0` is parsed once as a value and again to infer its type. These
default ASTs were already validated at `CREATE TABLE`, and their types and
target columns cannot change between inserts.

Catalog construction should store a bound default expression: parsed constants,
resolved result type, assignment cast, and any dynamic operation that must run
per row. Immutable literal defaults can be stored directly as target-typed
`Value`s. Volatile defaults such as sequences, clocks, or UUID generation must
still execute once per produced row, but their names, argument types, and cast
path can be prepared once. This removes repeated analysis without incorrectly
constant-folding dynamic defaults.

### Finding: the target schema is deeply cloned before defaults are evaluated

Confidence: high. Priority: medium.

`execute_insert` clones the complete `TableSchema`, including each column's
default AST and all constraints, to avoid borrow conflicts while mutating
storage. Thus this operation copies precisely the metadata it then reads for
default evaluation. A planned statement that owns compact target-column and
constraint metadata, or a state layout that permits independent catalog and
table borrows, would avoid work proportional to schema/constraint size on each
execution.

### Findings inherited from `insert_one`

The two no-op statement clones, two row clones, catalog-wide deferred-FK scan,
all-table commit/reclamation sweep, and repeated assignment analysis for the
explicit values remain. The default-specific planning above is the additional
issue revealed by this operation.

## `insert_many`

Query:

```sql
INSERT INTO users (id, name, active, score)
VALUES (6, 'Finn', true, 3), (7, 'Gina', false, 8)
```

### Finding: all input rows are fully materialized before any is inserted

Confidence: high. Priority: medium for large VALUES/INSERT-SELECT operations.

`execute_insert` first builds and validates every row into a `Vec<Vec<Value>>`,
then makes a second pass to check uniqueness, insert, validate foreign keys, and
evaluate `RETURNING`. Memory therefore grows with the complete input, in
addition to the parsed VALUES AST and eventual stored versions. The trace's
per-row build/evaluation calls all occur before the first `insert` call.

Rows can be produced and consumed incrementally for ordinary VALUES input. A
failure in a later row already aborts the transaction, so earlier versions
remain invisible and are discarded by rollback. `RETURNING` rows must still be
accumulated for the result, but an INSERT without `RETURNING` need not stage
input rows. INSERT-SELECT can eventually use the same producer/consumer shape,
subject to preserving a stable source view when source and target overlap.
Differential tests must pin down PostgreSQL's row-by-row error and volatile
default/sequence evaluation order; streaming must follow that order rather than
blindly preserve or change the current two-phase ordering.

### Finding: every unique index key is normalized twice per row

Confidence: high. Priority: medium.

For each row, `has_visible_unique_conflict` builds normalized keys for all
unique indexes. `Table::insert` then calls `add_index_entries`, rebuilding the
same keys. The trace shows four `build_row_index_key`/`build_index_key` pairs for
two rows and one primary-key index. Normalization can clone text, bytea, numeric,
and other owned index values, so this is more than a branch.

The storage API should prepare index keys once, use them for conflict checks,
and transfer them into index entries if insertion succeeds. This also aligns
with removing the full storage-layer row clone identified in `insert_one`.

### Finding: per-row expression setup scales linearly with VALUES cells

Confidence: high. Priority: medium as part of bound DML.

Compared with `insert_one`, literal evaluation, type inference, coercion, and
the creation of an empty constant-expression schema all double with two rows.
Evaluation of each value is necessary, but reconstructing the same target-slot
and cast decisions is not. A VALUES plan can bind each column position and
expected type once, then evaluate each row's already-bound cell expressions.

### Findings inherited from the other INSERTs

No-op AST cloning happens once per statement. Row cloning and duplicate index
normalization happen per row. Deferred-FK discovery and the all-table
commit/reclamation sweep happen once per statement. Constraint checks that
depend on each produced value correctly remain per row.

## `update_one`

Query:

```sql
UPDATE users SET score = 20 WHERE id = 1
```

One of the three rows matches, and `id` is the primary key.

### Finding: lock discovery and UPDATE execution independently scan the table

Confidence: high. Priority: very high.

`collect_required_row_locks` iterates all version chains, resolves visibility,
and evaluates `id = 1` for each of the three rows. After acquiring the resulting
lock, `execute_update` independently iterates all version chains and evaluates
the same predicate for all three rows. The trace consequently has six
`find_visible_version`, six `is_visible`, six comparison, and six
`evaluate_and_coerce` pairs for a three-row table.

The final successful lock-discovery pass already knows the target `RowId`s and
the snapshot under which they were selected. `acquire_row_locks` should return
that candidate set (or a mutation access object) to the executor so it does not
search again. A wait may legitimately force lock discovery to rerun with a new
READ COMMITTED snapshot, but after the last successful pass the database mutex
remains held through execution, so discarding the selected targets is needless.

### Finding: primary-key UPDATE does not use the unique index

Confidence: high. Priority: very high.

Both scans above call `iterate_version_chains`; neither recognizes that `id = 1`
can use the primary-key index. The SELECT scan path already contains a narrow
point-lookup optimization, but mutation lock planning and execution do not
share it. This UPDATE is therefore O(table rows) twice rather than an indexed
lookup plus mutation.

A common access plan should recognize equality between a unique indexed column
and a bound constant/parameter, then serve SELECT, lock acquisition, UPDATE,
and DELETE. Combined with reusing the lock-discovery candidates, this operation
should require one indexed lookup rather than two heap scans.

### Finding: predicate analysis is multiplied by both scans

Confidence: high. Priority: high as part of bound expressions.

Because generic expression evaluation performs type/operator resolution at
runtime, the duplicate scans multiply that work. The trace has 60
`infer_expression_type`, 83 `extract_ast_value`, 37 `parse_integer_literal`,
and 102 `matches_identifier` calls. The predicate and assignment contain only
two integer literals, `20` and `1`. A bound mutation plan should compile the
predicate and assignment once and be shared by lock selection and mutation
execution.

### Finding: matched rows are copied repeatedly

Confidence: high. Priority: medium for wide rows.

Even with no `FROM` and no `RETURNING`, target collection constructs a full
scope-sized row for every visible row. A match stores both `version.row.clone()`
and that constructed row. Mutation then clones the old row into `updated`,
clones `updated` when appending the version, and `append_updated_version` clones
it again for index construction. It also copies the updated values back into
the bound row although no returning projection exists.

Candidate selection should retain compact row identities and only the old/new
row data actually required by assignments, constraints, foreign-key actions,
and `RETURNING`. Index keys should be prepared without cloning the whole new
row, as with INSERT. A specialized no-`FROM`, no-`RETURNING` plan naturally
avoids the scope-sized placeholder rows and final copy-back.

### Findings inherited from INSERT

The UPDATE also deep-clones `TableSchema`, passes through two no-op statement
clones/materialization stages, scans the catalog for deferred foreign keys, and
sweeps all tables at commit.

## `update_expression`

Query:

```sql
UPDATE users SET score = score + 1 WHERE active = true
```

Two of the three rows match. Neither the predicate nor assignment touches the
primary-key column.

### Finding: unchanged indexes are fully checked and maintained

Confidence: high. Priority: high.

For each matched row, the executor calls `has_visible_unique_conflict` for the
complete updated row and `append_updated_version` adds entries for every unique
index. The only index is on `id`, but this statement changes only `score`, so
the index key cannot change. The trace has eight `build_row_index_key` and eight
`build_index_key` calls for two updated rows: conflict checking and version
append build one each, then commit-time pruning rebuilds the old and retained
keys. All four per row normalize an unchanged primary-key value.

The UPDATE plan should compute which indexes can be affected from the assigned
column set. Unchanged indexes need neither conflict checks nor new entry
construction. Version visibility still finds the new row through the existing
`RowId` entry, and reclamation metadata can record that the index was unchanged
instead of rebuilding old/new keys at prune time. For affected indexes, prepare
each old/new key once and reuse it for conflict checking, maintenance, and
reclamation.

### Finding: referential-action discovery scans all tables once per updated row

Confidence: high. Priority: high in schemas with many tables or many matches.

`apply_referencing_foreign_key_actions` begins by iterating every catalog table
and constraint to collect foreign keys that reference `users`. It is called
separately for both updated rows, even though no foreign key exists in the
fixture and even though `id`, the only possible referenced key here, is
unchanged.

The catalog should maintain reverse foreign-key metadata keyed by the parent
table. Mutation planning can then determine whether any assigned column
intersects a referenced key and skip the action path entirely for this UPDATE.
When actions are required, resolve the relevant constraints and column indexes
once per statement rather than rediscovering them for every row.

### Finding: assignment operator/type planning repeats per matched row

Confidence: high. Priority: high as part of bound expressions.

`score + 1` runs through subquery detection, column-name lookup, repeated type
inference, common-type resolution, literal parsing, and cast selection for each
of the two matches. Alongside the predicate's duplicate lock/execution scans,
the trace totals 95 `infer_expression_type` and 231 `matches_identifier` calls.
The assignment should be a bound arithmetic expression over the `score` slot
with a pre-parsed `Int4(1)` and preselected numeric operation/casts.

### Finding: all matching row bodies are staged before mutation

Confidence: high. Priority: medium for wide rows or broad updates.

`execute_update` collects every match into `targets`, storing multiple full-row
copies, before it mutates the first row. Lock discovery has already separately
collected the corresponding row identities. Reusing those identities and
loading/mutating one row at a time avoids memory proportional to
`matched rows * row width` and many clones. The statement still needs all
required row locks acquired before mutation to preserve the current waiting
model, but the lock list can be compact `RowId`s rather than cloned row bodies.

### Findings inherited from `update_one`

The predicate is evaluated during both lock discovery and execution, each pass
scans all rows, and post-statement bookkeeping scans global state. The new
findings here concern work repeated per updated row after candidates are found.

## `update_no_rows`

Query:

```sql
UPDATE users SET score = 0 WHERE id = 999
```

No row matches; `id` is uniquely indexed.

### Finding: a missing indexed key still causes two full scans

Confidence: high. Priority: very high.

As in `update_one`, lock discovery and execution each scan all three version
chains and evaluate `id = 999`. No locks or target rows are produced, yet the
trace contains six visibility checks and six comparisons. An indexed access
plan would establish absence with one unique-index lookup, and passing that
empty candidate set from lock discovery to execution would end row processing
immediately.

This is an important case for test workloads: idempotent cleanup and guarded
updates often affect zero rows. Their cost should be logarithmic in indexed
table size rather than two linear scans.

### Finding: zero-row DML still takes global write bookkeeping paths

Confidence: high. Priority: high.

Although no version is written, the static `statement_contains_dml` flag causes
`contains_deferred_foreign_keys` to scan the catalog. Autocommit then runs the
normal write commit, invoking `commit_transaction_versions` and
`prune_versions` for all three tables. This is the same avoidable work seen for
SELECT, now triggered by syntactically mutating but operationally read-only DML.

Transaction state should record actual mutations/touched tables, not infer them
only from statement kind. If an UPDATE/DELETE/INSERT-SELECT produces no writes,
it can finish via the read-only registry path. Deferred-constraint dirtiness
likewise only needs consideration after a row change relevant to a deferred
constraint.

### Necessary planning work

PostgreSQL must reject an invalid assignment type even when no row matches, so
planning and validating `score = 0` cannot simply be skipped. It should happen
once. The repeated predicate type/name/literal analysis across two scans is not
necessary and is covered by the bound-expression recommendation.

## `delete_one`

Query:

```sql
DELETE FROM users WHERE id = 3
```

One row matches the primary-key predicate.

### Finding: DELETE repeats the lock-selection scan and ignores the index

Confidence: high. Priority: very high.

The DELETE path has the same split as UPDATE. Lock discovery scans all three
rows and evaluates `id = 3`; `execute_delete` then scans and evaluates all three
again. The trace contains six visibility checks and six comparisons. Despite
the primary-key equality, both phases use `iterate_version_chains` rather than
the unique index.

The common mutation access plan proposed for `update_one` should cover DELETE:
perform one indexed lookup, acquire the selected row lock, and pass the final
candidate identity into execution. This replaces two O(n) scans with one
indexed lookup.

### Finding: DELETE copies matched row bodies even when consumers are absent

Confidence: high. Priority: medium for wide rows.

Target collection clones the old stored row and also builds a separate full
bound row. With no `USING`, no `RETURNING`, and no referencing foreign key in
the fixture, neither full copy is needed to mark the version deleted; a
`RowId`/version identity is sufficient. Old values should only be retained for
referential actions or returning expressions that actually require them, as
specified by the mutation plan.

### Finding: referential-action discovery is global for each deleted row

Confidence: high. Priority: high.

Before marking the row deleted, `apply_referencing_foreign_key_actions` scans
all catalog tables for inbound foreign keys. It finds none. Reverse-FK catalog
metadata would let the plan skip this call entirely for an unreferenced table
and avoid O(all tables and constraints) discovery per deleted row.

### Findings inherited from UPDATE

The predicate is type-resolved and literals are parsed throughout both scans;
the statement/schema clones, deferred-FK catalog scan, and all-table commit
sweep are also present.

## `delete_many`

Query:

```sql
DELETE FROM users WHERE active = false
```

One row matches in this fixture, but the predicate is not uniquely indexed and
can match an arbitrary number of rows.

### Finding: a necessary heap scan is performed twice

Confidence: high. Priority: very high.

Unlike `delete_one`, this predicate cannot use an existing unique index, so one
O(n) scan is expected. The second full scan and second evaluation of every
predicate are not. Reusing the candidate `RowId`s from the successful
lock-discovery pass halves the primary table work and removes a complete set of
runtime type/name/operator analysis.

### Finding: target staging and FK discovery scale with all matches

Confidence: high. Priority: high for broad deletes.

`execute_delete` stages cloned old and bound rows for every match before
deleting any. It then calls global referential-action discovery independently
for every target. For a delete matching `m` rows in a catalog with `t` tables,
this can add O(m * t) constraint discovery and O(m * row width) temporary
storage after the two O(n) scans.

Acquire all required target locks using compact identities, resolve inbound
foreign-key metadata once, and process the locked targets incrementally. Old
row values need only be loaded for actual FK keys/actions or `RETURNING`.

### Findings inherited from `delete_one`

Bound predicate evaluation, conditional row materialization, and touched-table
commit bookkeeping are the same remedies. The difference is that a heap scan
remains necessary unless a suitable non-unique index is introduced in the
future.

## `join_cross`

Query:

```sql
SELECT u.name, t.name FROM users u CROSS JOIN teams t
```

Three user rows times two team rows produce six output rows.

### Finding: the inner table is rescanned for every outer row

Confidence: high. Priority: medium for CROSS JOIN, higher when storage
visibility is expensive.

The streamed nested-loop path scans `users` once, then invokes
`visit_table_factor_rows` for `teams` once per user. The trace has four table
factor/version-chain iterations and nine visibility checks: three for users and
six for the three repeated two-row team scans. Scanning visible team rows once
would require five visibility checks.

The Cartesian output is inherently O(n * m), so caching the inner visible rows
does not change total output complexity. It does remove repeated storage/MVCC
work and catalog lookup, changing source discovery from O(n * m) to O(n + m).
The planner can materialize the smaller/reused side once, accepting O(m * row
width) memory, or use a reusable visible-row handle cache within the statement.

### Finding: every pair clones a full joined row although only two columns are used

Confidence: high. Priority: high for wide tables.

Each base-table visit allocates a vector as wide as the complete seven-column
join scope and fills unused positions with NULL. For every Cartesian pair,
`visit_nested_loop_join_rows` then constructs another seven-value vector by
cloning values from both sides. Only `u.name` and `t.name` are projected; the
other five values are copied and immediately discarded.

A joined row should be represented as references/handles to source rows or as
a compact set of slots required by join conditions, residual predicates,
ordering, grouping, and projection. Materialize owned output values only at the
projection boundary. This changes copying from O(output pairs * total source
width) toward O(output pairs * required width), which is algorithmically
meaningful for wide schemas and owned values.

### Finding: qualified projection lookup scans the full scope per output value

Confidence: high. Priority: high as part of bound expressions.

The six rows produce twelve values. Every value runs `contains_subquery` and
resolves a qualified name against the full joined scope. Qualified resolution
first scans for qualifier depth and then scans again for the column, producing
245 `matches_identifier` calls in this tiny join. Projection planning already
knows both slots; execution should read them directly.

### Not considered a primary target

`evaluate_join_condition` is invoked for each pair even though CROSS JOIN is
unconditional. Removing that constant branch is reasonable once join operators
are planned, but pair generation and output are unavoidable; it is not a
standalone algorithmic win.

The no-op materialization passes and all-table read commit from the SELECT
traces remain present.

## `join_inner`

Query:

```sql
SELECT u.name, t.name
FROM users u
JOIN memberships m ON m.user_id = u.id
JOIN teams t ON t.id = m.team_id
```

The fixture has three users, two memberships, and two teams. Two rows are
returned.

### Finding: multi-way equijoins fall back to recursive nested loops

Confidence: high. Priority: very high.

The executor has a hash-join path, but only when `table.joins.len() == 1`.
Because this query has two joins, it scans memberships for each user and scans
teams for each row that survives the first join. The trace records thirteen
visibility checks:

- three from scanning users once;
- six from scanning two memberships for each of three users;
- four from scanning two teams for each of two first-join matches.

It evaluates ten join conditions to produce two rows. As tables grow, the work
is O(users * memberships + first_join_matches * teams).

The planner should build a pipeline of join operators rather than select a hash
join only for the special case of one join. Here it can hash memberships by
`user_id` and teams by `id` (or use the unique team index), scan each source
once, and probe through both stages. Source visibility work becomes linear in
the three inputs, and candidate comparisons become proportional to actual hash
matches rather than Cartesian candidates.

### Finding: full joined rows are built before knowing whether a pair matches

Confidence: high. Priority: high.

For every one of the ten candidate pairs, the nested-loop code creates a full
nine-slot joined vector and clones source values into it before evaluating the
join condition. Six of those vectors fail a condition and are immediately
discarded. Surviving rows are then copied again at the next join level and only
two name columns are ultimately projected.

Hash/index join probes should compare compact precomputed keys first and build a
joined row handle only for matches. As in `join_cross`, that handle should refer
to source rows/slots so projection does not clone unneeded columns. This removes
both failed-candidate row construction and full-width copies for successful
pairs.

### Finding: join-key binding and type resolution repeat for every candidate

Confidence: high. Priority: very high as part of join planning.

Each equality condition resolves its qualified columns against the nine-column
scope and rediscovers operand/common types and casts. The trace contains 100
`infer_expression_type`, 122 `resolve_bound_column`, and 2,519
`matches_identifier` calls. Only ten comparisons and four projected output
values are data-dependent; identifier and operator resolution are not.

A planned equijoin should store left/right source slots, normalized key types,
and comparison/hash semantics once. The same bound slots should feed the final
projection. This is necessary for the hash pipeline above and removes the
largest repeated metadata work visible in the join traces.

### Findings inherited from `join_cross`

Base rows and successful join results use full-scope NULL-padded vectors, final
projection invokes subquery detection, and read-only commit sweeps every table.
The multi-way nested-loop fallback is the additional dominant issue here.

## `join_inner_filter`

Query:

```sql
SELECT u.name, t.name
FROM users u
JOIN memberships m ON m.user_id = u.id
JOIN teams t ON t.id = m.team_id
WHERE u.active = true
```

Two users pass the filter and both produce a joined row.

### Finding: filter pushdown is rediscovered for every repeated table scan

Confidence: high. Priority: high.

`visit_table_factor_rows` calls `collect_pushdown_filters` whenever it is
invoked. The outer users factor is visited once, memberships twice, and teams
twice. Each invocation re-examines the same `WHERE` AST and resolves its column
against the joined scope to decide whether the predicate belongs to that
factor. The trace has five filter-collection passes even though filter placement
is a static planning decision.

Predicate binding should assign each safe conjunct to a source/operator once.
The users scan then receives a bound `active`-slot predicate directly;
memberships and teams receive none. This avoids AST walks and name resolution
whose count currently grows with nested-loop rescans.

### Finding: the pushed filter is reevaluated on final rows

Confidence: high. Priority: high.

The users scan evaluates `u.active = true` for all three users and correctly
prevents the inactive user from entering the join. However, each of the two
final joined rows is passed to `evaluate_where_clause` with the original full
predicate, so both passing users are tested again. Of the trace's thirteen
comparisons, eight are join conditions and five are the three pushed filter
evaluations plus two redundant final evaluations.

As with `select_filter`, planning must retain only predicates not fully consumed
by pushdown as the residual `WHERE`. For this query the residual is empty.

### Finding: useful pruning does not fix the multi-join nested-loop algorithm

Confidence: high. Priority: very high.

Pushdown reduces source checks from thirteen in `join_inner` to eleven and join
condition evaluations from ten to eight, which is correct and useful. The
executor still rescans memberships per passing user and teams per first-join
match. A multi-stage hash/index plan would scan all three tables once, apply the
users filter once per user, and probe the two equijoins.

### Finding: static expression work remains dominant

Confidence: high. Priority: very high as part of planning.

The added filter raises the trace to 129 `infer_expression_type`, 135
`resolve_bound_column`, and 2,848 `matches_identifier` calls. These counts come
from repeated join-key planning, predicate type/coercion planning, pushdown
classification, residual reevaluation, and projection lookup. A single bound
plan should supply source filters, join keys, residual predicates, and
projection slots without any runtime identifier lookup.

### Findings inherited from `join_inner`

Candidate pairs are represented as full nine-slot cloned vectors before join
tests, nested inner sources are rescanned, and read-only commit scans all tables.

## `join_left`

Query:

```sql
SELECT u.name, t.name
FROM users u
LEFT JOIN memberships m ON m.user_id = u.id
LEFT JOIN teams t ON t.id = m.team_id
```

The query returns two matched users and one unmatched user with a NULL team.

### Finding: outer joins scan sources once but compare Cartesian candidates

Confidence: high. Priority: very high.

Because `can_stream_inner_join` rejects LEFT JOIN, this path materializes each
base factor once. The trace has the optimal seven source visibility checks
(three users, two memberships, two teams), unlike the repeated inner scans in
`join_inner`. It then uses nested loops for both stages: six user-membership
candidates and six intermediate-team candidates, for twelve join-condition
evaluations.

Hash joins can preserve LEFT JOIN semantics by hashing the right side, probing
once per left row, and emitting a NULL-extended result when no key matches.
Applying that operator at both stages changes candidate matching from
O(users * memberships + first_join_rows * teams) to expected linear build/probe
work plus actual matches, while retaining unmatched rows.

### Finding: outer-join materialization copies full intermediate relations

Confidence: high. Priority: high.

All base rows are stored as nine-slot NULL-padded vectors. For each of twelve
candidate pairs, the executor constructs another full vector before checking
the condition. Matched vectors are accumulated into a new `joined` collection;
unmatched left rows are cloned again for NULL extension. The second join repeats
the process over the first materialized intermediate. Only two name values per
final row are needed.

A hash outer-join pipeline operating on row handles/slot references removes
failed-candidate vectors and avoids copying full intermediate relations. It
should explicitly represent missing right rows rather than relying on
full-scope vectors prefilled with `Value::Null`.

### Finding: condition and projection binding repeat across all candidates

Confidence: high. Priority: very high as part of join planning.

The trace records 116 `infer_expression_type`, 144 `resolve_bound_column`, and
2,962 `matches_identifier` calls. It invokes subquery detection for every one
of the twelve join conditions and six projected values. Bound join keys and
projection slots eliminate this AST/name/type work and are prerequisites for a
hash outer join.

### Positive observation

The outer-join path does not rescan table storage; its problem is the
Cartesian-candidate and full-intermediate algorithm. This distinction matters:
reusing visible-row scans alone would not improve `join_left`; it needs a hash
outer-join operator and compact row representation.

### Findings inherited from other SELECTs

No-op AST materialization passes, repeated scope construction during analysis,
and the all-table read commit remain.

## Cross-trace source audit

### Finding: every generic statement snapshots the entire sequence namespace

Confidence: high. Priority: high when catalogs are large.

Before CTE, subquery, lock, or statement execution, `Session::execute_statement`
creates a `SequenceExecutionContext`. Its constructor clones every
`SequenceSchema` into a new map and clones every table name into a set so that a
later sequence lookup can distinguish wrong object type. All sixteen traces
show this context construction; none of the traced queries uses a sequence.

Sequence metadata should not be copied eagerly per statement. A plan can mark
whether sequence operations or sequence-backed defaults are reachable and
construct access only when needed. Alternatively, catalog relation lookup can
be exposed through a stable statement/catalog view so the context stores
references or shared immutable metadata. The required sequence value/session
state remains dynamic; the namespace clone does not.

### Finding: non-grouped SELECTs run primary-key grouping analysis

Confidence: high. Priority: medium.

`resolve_grouping_plan` calls
`extend_grouped_columns_with_primary_keys` before it knows whether grouping is
enabled. Even with an empty GROUP BY and no aggregate, that function walks all
bound columns, deduplicates relations with a vector, and searches the catalog by
table id for every relation. This explains the extra catalog table iterations
in simple SELECTs and one per joined relation in the join traces.

Aggregate/grouping detection should precede functional-dependency expansion.
If grouping is disabled, skip it. If enabled, the bound relation should already
carry its schema/primary-key metadata so the planner does not linearly search
the catalog by id.

### Finding: the narrow prepared fast path already validates the core direction

Confidence: high.

`PreparedQueryPlan` stores projection slots, bound expression slots/types, and
a scan-versus-unique access choice. Its executor avoids generic AST evaluation,
statement context construction, materialization visitors, and normal write
commit. However, it only accepts a single-table SELECT with a small binary
expression subset and no joins, ordering, limit, grouping, distinctness, locks,
or most expression forms. None of the more revealing trace cases can use it,
and the trace driver calls generic `execute` in any event.

The architectural recommendation is to generalize this idea into the normal
analyzer/executor boundary, not accumulate disconnected fast paths. One-shot
execution can build a bound plan once after parsing; explicit preparation can
retain that same plan across calls.

## Prioritized optimization program

### 1. Introduce a general bound logical/expression plan

Expected impact: highest and broadest. Prerequisite for most later work.

The plan should replace runtime AST interpretation for ordinary execution. It
should contain resolved source/column slots, parsed literals, inferred types,
selected casts/operators/functions, aggregate/subquery/volatility facts,
projection/order/group/distinct expressions, and target assignment metadata.
It should be produced by one coordinated analysis pipeline, while preserving
PostgreSQL error ordering where separate checks currently matter.

This removes per-row `contains_subquery`, `resolve_bound_column`,
`infer_expression_type`, literal parsing, common-type resolution, and repeated
visitor clones. It also lets one-shot and prepared execution share semantics
instead of maintaining a narrow separate `PreparedQueryPlan` executor.

### 2. Make mutation access and locking one pipeline

Expected impact: highest for UPDATE/DELETE and zero-row DML.

Choose heap versus unique-index access during planning. The final successful
lock-discovery pass should return compact candidate `RowId`/version identities
to execution. UPDATE and DELETE must not rescan and reevaluate the predicate.
Point mutations become indexed lookups rather than two table scans; broad
mutations retain one required scan rather than two.

Track actual mutation/touched-table state at the same boundary. A zero-row DML
can take the read-only transaction finish path, while a real mutation commits
only touched tables.

### 3. Build composable hash/index join operators with compact row handles

Expected impact: highest for multi-table queries.

Support a chain of hash/index joins rather than the current one-inner-join
special case, and support NULL-extension for LEFT JOIN. Assign pushed filters
and residual predicates once during planning. Compare compact bound keys before
constructing result rows, scan each base source once where the selected join
algorithm permits, and represent intermediate rows as source handles/slot
views. Materialize only values actually needed downstream.

This changes the inner/left join traces from Cartesian nested-loop matching to
expected linear build/probe work plus actual matches, and eliminates full-width
vectors for failed candidates.

### 4. Remove global catalog/table sweeps from statement bookkeeping

Expected impact: high for realistic schemas with many tables.

Maintain transaction touched-table sets and a set/queue of tables with reclaimable
versions. Maintain catalog metadata for deferred-FK presence and reverse foreign
keys by parent table. Lazily access sequence metadata rather than cloning all
sequences and table names. Skip grouping functional-dependency work when the
query is not grouped.

These changes remove O(all tables/constraints/sequences) work that appears even
in a one-row SELECT or zero-row UPDATE.

### 5. Make row and index-key ownership deliberate

Expected impact: high for wide rows and owned values.

Prepare each normalized index key once and transfer it from conflict checking
to insertion. On UPDATE, skip indexes whose columns are unchanged. Build index
keys before moving a row into storage so storage does not clone the whole row.
Retain old/new row bodies only when assignments, constraints, FK actions, or
`RETURNING` require them. Avoid full scope-sized NULL-padded rows for simple
mutations and joins.

### 6. Add bounded/streaming physical operators

Expected impact: workload-dependent but algorithmic.

Use a top-k operator for compatible `ORDER BY` plus `LIMIT/OFFSET`, reducing
O(n log n) sort and O(n) retained rows to O(n log k) and O(k). Stream VALUES
rows into INSERT when `RETURNING`/source-overlap semantics allow it instead of
staging all input rows. Cache a nested-loop inner source when Cartesian output
is required but repeated storage/MVCC scans are not.

### 7. Remove no-op AST and lock/materialization passes

Expected impact: medium; do with the planner rather than as scattered patches.

The common simple path should not clone the statement in CTE materialization,
clone it again in subquery materialization, or run a separate CTE lock phase
when no CTE exists. Query facts gathered by the bound plan should directly
select required rewrites/plan nodes and lock requirements. This is worthwhile
for large statements, but per-row analysis, mutation rescans, and join
algorithms should land first.

## Suggested scaling checks before implementation

The traces have no timings, so benchmarks should vary one dimension at a time
to confirm impact and prevent optimizing instrumentation noise:

- primary-key UPDATE/DELETE hit and miss at 1, 100, and 10,000 rows;
- non-indexed broad UPDATE/DELETE at the same sizes, counting predicate
  evaluations to verify one scan;
- simple SELECT and zero-row UPDATE with 1, 100, and 1,000 unrelated tables;
- two- and three-stage inner/left equijoins with independently scaled inputs;
- narrow versus wide rows with large text/bytea payloads for join, INSERT, and
  UPDATE clone costs;
- `ORDER BY ... LIMIT 10` with increasing candidate counts;
- repeated prepared and one-shot forms of the same supported statement to
  verify both use the same plan while only preparation reuses it across calls.

## Deliberately deprioritized work

Empty order/distinct/returning helpers, unconditional CROSS JOIN condition
branches, a handful of clock reads, and similar constant-time calls are visible
because the trace instruments every function. They do not explain scaling and
should only be cleaned up when naturally removed by the plan/operator changes
above. Call count without timing or scaling evidence is not enough to justify a
micro-optimization.
