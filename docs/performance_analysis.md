# SELECT performance analysis

## Current measurements

The current benchmark results separate several different costs:

| Workload | Average |
| --- | ---: |
| parse, analyze, and execute an indexed select | 16.03 us |
| reuse a prepared indexed select | 5.61 us |
| prepared 100-row heap scan | 24.04 us |
| prepared indexed lookup over 100 rows | 5.70 us |
| prepared indexed lookup over 10,000 rows | 5.74 us |
| SQLx `select_where_100_rows` | 37.47 us |

`select_where_100_rows` is a heap scan because its `id` column has no primary
key or unique constraint. It therefore measures a 100-row dynamically typed
predicate scan together with SQLx adaptation and an implicit transaction, not
the cost of an index lookup.

The stable indexed result at 100 and 10,000 rows demonstrates that index
selection works and that fixed executor overhead dominates an indexed lookup.
The heap scan is expensive because the SQL AST is repeatedly analyzed while
each candidate row is evaluated.

## Function trace

`trace.log` contains 742 entered spans for the captured query:

| Phase | Entered spans |
| --- | ---: |
| preparation | 167 |
| prepared execution | 570 |
| result conversion after execution | 5 |

Preparation binds the query scope four times. Prepared execution binds it two
more times. The execution phase includes 42 expression-type inferences, 40
identifier comparisons, 20 column resolutions, 22 cast checks, and 19 integer
literal parses.

A prepared statement currently retains the original `sqlparser` statement,
parameter types, and output metadata. Preparation computes a typed description
but discards it. Every execution then clones the original AST to substitute
parameters, clones and visits it to materialize subqueries, and rebuilds the
query scope, validation state, projection, ordering, distinct, grouping, and
access-path decisions.

The expression evaluator also resolves column names, infers operand types,
selects a common type, and applies coercions during row evaluation. A pushed
down filter is evaluated while reading the table and the complete `WHERE`
expression is evaluated again after that row is returned.

Other fixed work includes eagerly cloning every sequence schema and table name
into a sequence execution context for queries that do not use sequences. An
autocommit read also registers and commits a full transaction, scans every
table for committed transaction versions, runs version pruning, clears lock
state, and notifies waiters even when the statement changed nothing.

Function count is not a direct measurement of cost. Execution logging changes
optimization and adds tracing overhead. The trace is useful for finding
repeated semantic work, while Criterion measurements decide whether an
optimization is retained.

## Assessment

Merging the existing visitors can reduce preparation time but does not address
the main hot path. The visitors have different contextual requirements and
some mutate the AST. A single multipurpose visitor would add complexity while
leaving name resolution and type inference in per-row execution.

Moving preprocessing into prepared statements is the correct architectural
direction. The useful form of visitor consolidation is a recursive binder that
produces a typed, owned execution plan and gathers parameter, aggregate,
subquery, and column-reference information while doing so.

The execution plan should contain resolved table identities, column slots,
parameter slots, parsed typed literals, selected casts and operators, access
paths, residual predicates, direct projections, grouping and ordering plans,
subquery plans, and output metadata. Parameter values should be coerced once
into an execution frame rather than substituted into a cloned AST.

Prepared plans must retain catalog dependencies. Relevant DDL must either
leave a stable referenced identity valid or deliberately invalidate the plan;
a prepared statement must never silently target a replacement relation.

Changing the unique-index `BTreeMap` to a hash map or adding automatic
secondary indexes is not currently justified. The table-size-independent
indexed measurement shows that lookup complexity is not the dominant cost.

## Optimization sequence

Each optimization is measured independently. Its before/after benchmark result
is presented for approval before it is retained and before the next
optimization starts.

1. Introduce typed bound expressions and an owned prepared query plan.
2. Remove type inference, name resolution, literal parsing, and cast selection
   from per-row evaluation.
3. Select table scans and unique-index lookups during preparation and evaluate
   only residual predicates after access-path filtering.
4. Compile identifier projections to direct row slots.
5. Ensure each predicate is evaluated only once.
6. Skip subquery, aggregate, grouping, distinct, ordering, join, and row-lock
   machinery when the prepared plan shows it is unnecessary.
7. Construct sequence execution state lazily and avoid cloning unrelated
   catalog entries.
8. Add a read-only implicit-transaction fast path that preserves snapshots and
   timestamp semantics without write-version and lock cleanup.

SQLx locking and task-dispatch behavior is outside this optimization sequence.

## Measurement protocol

The primary comparison is the SQLx PostgreSQL-versus-pg_fake benchmark pair:

```sql
SELECT * FROM select_100_rows WHERE id = 50;
SELECT * FROM select_where_indexed_100_rows WHERE id = 50;
```

The existing native-core diagnostics remain necessary to distinguish plan
preparation, heap scanning, unique lookup, implicit transaction, and SQLx
adapter costs. Benchmarks run without execution logging. Multiple Criterion
samples, rather than function counts or a single elapsed-time sample, determine
whether a change is kept.

## Optimization 1 candidate measurement

The first candidate implements a vertical slice of the owned prepared plan for
simple single-table selects. Eligible queries retain resolved table identity,
column and parameter slots, operand types, predicate structure, projection
slots, and scan or unique-lookup access. Complex query shapes continue through
the existing AST executor. The transaction and SQLx locking paths are
unchanged.

The vertical slice necessarily exercises parts of later generalization steps:
for eligible queries it evaluates the bound expression without runtime name or
type resolution, projects direct slots, and evaluates the predicate once. The
later steps extend those properties to the remaining expression and query
shapes rather than duplicating this implementation.

The primary SQLx comparison used 200 Criterion samples:

| Query | Before | Candidate | Change |
| --- | ---: | ---: | ---: |
| 100-row heap predicate | 36.04 us | 18.24 us | 49.4% faster |
| 100-row primary-key predicate | 16.18 us | 12.98 us | 19.8% faster |

Native-core diagnostics provide the corresponding executor measurements:

| Query | Before | Candidate | Change |
| --- | ---: | ---: | ---: |
| parse, analyze, and indexed select | 16.03 us | 13.36 us | 16.7% faster |
| prepared indexed select | 5.61 us | 2.78 us | 50.4% faster |
| prepared 100-row heap predicate | 24.04 us | 6.02 us | 75.0% faster |
| prepared 100-row indexed predicate | 5.70 us | 2.78 us | 51.2% faster |
| prepared 10,000-row heap predicate | 1.98 ms | 340.26 us | 82.8% faster |
| prepared 10,000-row indexed predicate | 5.74 us | 2.86 us | 50.1% faster |

## Optimization 2 measurement

Bound comparisons already require operands with matching types, and prepared
parameters are coerced once before execution. Removing the two generic
coercion calls from each bound binary-expression evaluation therefore preserves
the fallback for mixed or unsupported expressions while eliminating redundant
work for every candidate row.

| Query | Before | Candidate | Change |
| --- | ---: | ---: | ---: |
| SQLx 100-row heap predicate | 18.24 us | 16.84 us | 7.7% faster |
| SQLx 100-row primary-key predicate | 12.98 us | 13.14 us | 1.3% slower, within noise |
| prepared indexed select | 2.785 us | 2.753 us | 1.2% faster |
| prepared 100-row heap predicate | 6.02 us | 4.63 us | 23.0% faster |
| prepared 100-row indexed predicate | 2.779 us | 2.751 us | 1.0% faster |
| prepared 10,000-row heap predicate | 340.26 us | 212.35 us | 37.6% faster |
| prepared 10,000-row indexed predicate | 2.862 us | 2.837 us | 0.9% faster |

The improvement scales with the number of rows whose predicate is evaluated.
An indexed lookup evaluates only one candidate, so its small native saving is
hidden by SQLx and transaction overhead. PostgreSQL timings varied materially
during the SQLx run, but the native heap diagnostics isolate and confirm the
per-row reduction.

## Optimization 3 measurement

Once a prepared plan has proven that a statement is a plain single-table
`SELECT` without subqueries or row-lock clauses, execution bypasses generic
sequence-context construction, subquery materialization, DDL bookkeeping, and
row-lock discovery. Transaction creation, snapshots, and the database mutex
remain unchanged, and no SQLx code is involved.

| Query | Before | Candidate | Change |
| --- | ---: | ---: | ---: |
| SQLx 100-row heap predicate | 16.84 us | 14.09 us | 16.5% faster |
| SQLx 100-row primary-key predicate | 13.14 us | 10.01 us | 23.9% faster |
| parse, analyze, and indexed select | 13.37 us | 11.25 us | 16.0% faster |
| prepared indexed select | 2.753 us | 1.083 us | 60.5% faster |
| prepared 100-row heap predicate | 4.63 us | 2.99 us | 35.2% faster |
| prepared 100-row indexed predicate | 2.751 us | 1.130 us | 59.2% faster |
| prepared 10,000-row heap predicate | 212.35 us | 211.66 us | no measurable change |
| prepared 10,000-row indexed predicate | 2.837 us | 1.197 us | 58.1% faster |

This removes a fixed setup cost. It is therefore most visible for indexed and
small-table queries, while expression evaluation dominates the 10,000-row heap
scan.

## Optimization 4 measurement

Prepared query plans now borrow their retained SQL AST for the shared
transaction checks instead of cloning it on every execution. The fallback path
still owns the parameter-bound AST it creates. Prepared `SELECT` plans also
skip the DDL classifier because their statement kind was established during
preparation.

| Query | Before | Candidate | Change |
| --- | ---: | ---: | ---: |
| SQLx 100-row heap predicate | 14.09 us | 12.37 us | 12.2% faster |
| SQLx 100-row primary-key predicate | 10.01 us | 8.55 us | 14.6% faster |
| prepared indexed select | 1.083 us | 286 ns | 73.6% faster |
| prepared 100-row heap predicate | 2.993 us | 2.199 us | 26.5% faster |
| prepared 100-row indexed predicate | 1.130 us | 331 ns | 70.7% faster |
| prepared 10,000-row heap predicate | 211.66 us | 212.38 us | no measurable change |
| prepared 10,000-row indexed predicate | 1.197 us | 395 ns | 67.0% faster |

The first 10,000-row heap measurement was anomalously slower; repeating that
diagnostic produced 212.38 us, within 0.34% of the accepted baseline. The full
test suite passed, as did all four SQLx differential property tests with 10,000
generated iterations.

## Optimization 5 measurement

The first read-only autocommit candidate bypassed transaction registration. It
improved the heap workload but regressed the primary-key workload by 8.0% in a
direct A/B test, so it was rejected and fully removed.

The accepted candidate retains normal implicit transaction registration and
snapshot creation. When a prepared plan proves the transaction was read-only,
completion removes its in-flight registry entry without advancing the write
commit sequence, scanning tables for modified versions, pruning versions,
releasing nonexistent row locks, or updating the wait graph.

| Query | Before | Candidate | Change |
| --- | ---: | ---: | ---: |
| SQLx 100-row heap predicate | 12.37 us | 11.16 us | 9.8% faster |
| SQLx 100-row primary-key predicate | 8.55 us | 8.43 us | within measurement variance |

All 178 regular tests passed. In the 10,000-iteration SQLx property run, the
sequence, set-operation, and interleaved-transaction groups passed. The general
SQL generator found a text-collation ordering difference for `SELECT DISTINCT
... ORDER BY`; replaying its exact seed with this candidate removed fails
identically, confirming that difference is pre-existing and unrelated to
read-only transaction completion.
