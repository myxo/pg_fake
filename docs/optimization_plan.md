# Optimization experiments

Draft for approval, 2026-09-27. At drafting, no production optimization from this research had been implemented. This separate research plan records initial proposals and subsequent verified status; it does not alter `docs/plan.md` or the historical benchmark report.

## Recovery audit — 2026-09-30

The user authorized completing and committing measured experiments without further
approval. Historical “pending approval” labels below are not execution gates.
The September 29 prose includes results from reverted/unretained changes; it is
historical evidence, not proof that the current source contains those changes.
At recovery start HEAD was `c5adffb`; this plan itself was untracked.

- E1–E6, E8–E10 and E12 have corresponding implementations in the current
  source/history. Their historical before/after timings were not re-established; the fresh
  final audit below measures current source. E10 requires no additional source optimization.
- E7's prepared temporal nodes were absent. The restored extension now improves
  native prepared 100-row temporal execution 600.890 → 271.622 µs (54.8%)
  across three runs; 10/1,000-row cases improve 66.0%/53.0%. Independent
  review, 283 core tests, five SQLx runtime differential tests and seven migration
  tests pass. See [all timings and scope](optimization_research/e7_temporal.md).
  At the E7-only baseline, ordered queries still used fallback; E13 extends this.
  Committed as `e569753`.
- E11's copy-on-write maps remain, but default reconstruction was present.
  The new, smaller defaults-sharing experiment passes review, 282 core tests,
  and all six SQLx settings tests including PostgreSQL comparisons. Native
  inserts improve 23.273 → 14.775 µs, updates 31.556 → 23.408 µs, and transaction
  inserts 24.053 → 15.256 µs. See [reproduction and all runs](optimization_research/e11_defaults.md).
  Conditional settings write-back has not been restored or measured.
  Committed as `8310db2`.
- E13's prepared ordering/limits are restored with the shared bounded heap.
  All 18 native/SQLx cases improve across three runs, including wide-row pages.
  Independent review and core/view/SQLx differential checks pass. See
  [scope, rejected candidate, timings, and baseline mismatch](optimization_research/e13_ordering.md).
  Committed as `971f0cd`. Fresh bound-DML companions
  measure SQLx INSERT 39.646 → 28.609 µs (27.8%) and UPDATE 47.229 → 37.472 µs
  (20.7%); these compare existing APIs, not a new executor. See
  [all binding measurements](optimization_research/e13_dml_bindings.md);
  diagnostic committed as `c072977`.
  Optional cache sharing remains unrestored: its historical 7.3% gain adds
  ownership/invalidation machinery below the preferred threshold; no fresh
  cache-sharing gain is claimed.
- E14's reusable window templates are restored and independently reviewed.
  All 22 paired native/SQLx cases improve over three fresh runs, including
  row-number, moving-window and grouped controls. 284 core tests and both
  migration-transform PostgreSQL differential tests pass. See
  [all timings and reproduction](optimization_research/e14_windows.md).
  Committed as `64c30e9`.
- The grouping follow-up is restored with a 128-group threshold and shared
  equality keys. Native prepared 1,000-group latency improves 4,208.240 →
  2,769.666 µs (34.2%); 10,000 groups improve 191,862.945 → 35,195.179 µs
  (81.7%). Small-group means retain a disclosed 0.5–3.2% cost in the final
  repeat. 285 core tests, PostgreSQL grouping/window comparisons and source
  review pass. See [all candidates and controls](optimization_research/grouping.md).
  Committed as `5247ad1`.
- The final recovery audit covers 161 Criterion cases, a separate filtered
  tier-1 sweep and 13 fresh native family profiles. Of 71 paired full-sweep
  workloads, 35 are faster than PostgreSQL and only two meet PostgreSQL/10.
  Nested filtered views remain 11.12× slower; the target is not achieved.
  See [results, validation and remaining priorities](optimization_research/recovery_audit.md).
  The measured recovery experiments have separate commits after explicit Git
  authorization; the broader performance target remains unmet.

- A subsequent E8 simplification borrows the query when restoring output names,
  avoiding a full AST clone and temporary statement wrapper. The exact nested
  view SQLx workload improves 384.681 → 366.427 µs (4.7%). Retained under the
  smaller-gain simplicity exception; no universal scaling gain is claimed.
  Correctness validation and the 70-case tier-1/tier-3 rerun pass; independent
  final evidence review is clean. Committed as `287ca65`.
  See [measurement record](optimization_research/projection_names.md).

## Findings and priorities

The strongest opportunities are unnecessary database copies, repeated conversion of values back into SQL syntax, and work repeated inside row loops. Existing hash joins, indexed lookups, top-k ordering, prepared statements, and some subquery optimizations mean that a general recommendation to “add these features” would miss the problem. Extend the existing paths and simplify their boundaries.

Execute the >10×-slower work first, then the remaining tier 1 work, then tier 2/3 follow-ups. Changes in the first group may also improve tier 1. Keep each experiment independently measurable; the order below is not a proposal for one large executor rewrite.

The goal is **pg_fake latency ≤ PostgreSQL latency / 10**, measured through the same application-facing API and workload. Matching PostgreSQL is only an intermediate milestone. Removing one bottleneck will often reveal another. For example, eliminating an 84% regex-compilation cost has an idealized maximum benefit of about 6.25× by itself, not the roughly 358× reduction required to take that recorded workload from 35.8× slower to 10× faster.

## Evidence and measurement limits

Historical comparisons come from [the saved report](../crates/pg_fake_benchmarks/results/report.md) and its Criterion mean estimates, recorded on 2026-09-22, Apple M2, PostgreSQL target 18. These are SQLx client latencies, including PostgreSQL transport and protocol work, not isolated PostgreSQL executor CPU times. The environment file does not record the connection transport, server settings, or exact PostgreSQL minor version; record these in the next baseline.

New research uses an optimized native harness and macOS `sample`: four-second loops, two seconds of sampling at a requested 1 ms interval, setup outside sampling, instrumentation disabled. These probes reproduce representative SQL shapes, not the complete Criterion/SQLx workload. Most native probes call `Session::execute`, so include parsing; the UUID probe reuses a prepared statement. Autocommit also differs from the existing prepared diagnostic, which holds one transaction open. Do not divide these new probe times by the old PostgreSQL times to claim a new speedup.

See [research evidence and reproduction](optimization_research/README.md). Inclusive sample percentages can overlap and must not be added unless their call paths are disjoint. They establish where to experiment, not predicted end-to-end gains. Initial exploratory profiles overlapped debug-symbol generation; reported final profiles were repeated after stopping it. No fresh PostgreSQL comparison was run for this planning exercise. Prepared-repeat probes confirm that the major costs survive existing preparation: materialized CTE 8.32 ms, patterns 4.41 ms and temporal 1.27 ms, with the same dominant call families.

The benchmark suite shares a `PgFakeConnection`/database between many groups. Inserts and DDL generate backend-dependent amounts of retained transaction history during warmup and measurement. `TransactionRegistry::statuses` retains completed writes/aborts, even after tables are dropped. A constant-time lookup can coexist with expensive cloning of that registry. Filtered runs also execute other groups' fixture setup. Consequently, a filtered result is not necessarily comparable with the corresponding full-suite result.

### All recorded workloads >10× slower

`Ratio` is pg_fake / PostgreSQL. Latencies are saved Criterion means, in microseconds.

| Workload (tier prefix omitted) | Tier | pg_fake µs | PostgreSQL µs | Ratio | Experiments |
| --- | ---: | ---: | ---: | ---: | --- |
| skip_locked_queue_100_rows | 3 | 74,677.38 | 48.94 | 1525.94× | E1, E10 |
| transaction_repeatable_read_select_for_update | 2 | 68,121.56 | 85.52 | 796.56× | E1 |
| hashed_advisory_lock_acquisition | 3 | 37,321.23 | 79.65 | 468.54× | E1 |
| procedural_trigger_insert_update | 3 | 17,758.32 | 122.45 | 145.03× | E1, E6 |
| materialized_cte_100_rows | 2 | 8,973.87 | 79.45 | 112.95× | E4, E5 |
| recursive_cte_branching_traversal_127_rows | 3 | 8,182.48 | 122.72 | 66.68× | E4, E5 |
| uuid_any_100_rows | 2 | 3,063.09 | 53.67 | 57.07× | E3 |
| lateral_latest_per_parent_100_rows | 3 | 7,076.62 | 170.71 | 41.45× | E7, E9 |
| runtime_patterns_100_rows | 2 | 4,224.28 | 117.98 | 35.80× | E2, E7 |
| recursive_cte_numeric_series_100_rows | 3 | 2,520.60 | 75.16 | 33.54× | E4 |
| data_modifying_cte_update_100_rows | 3 | 2,200.98 | 105.93 | 20.78× | E6, E4 |
| nested_filtered_view_100_rows | 3 | 492.08 | 35.55 | 13.84× | E8 |
| sqlx_migration_chain | 2 | 71,836.08 | 6,562.77 | 10.95× | E1; migration checkpoint |
| runtime_temporal_100_rows | 2 | 1,233.32 | 117.82 | 10.47× | E7 |

## E0 — Establish comparable experiments

Before implementing a candidate, retain the current baseline and add controlled measurements beside it:

- Run the exact affected SQLx workload and an equivalent native prepared/unprepared pair. Separate prepare, execute, result materialization, and transaction boundaries. Fixed SQLx queries already use the statement cache; inserts/updates that interpolate a new ID into SQL largely miss it. Compare bound parameters separately rather than silently changing the benchmark.
- Give each group a fresh database, and add explicit history-size and unrelated-data-size axes. Preserve a full-suite run as a separate realistic long-session check. Keep fixture size constant between variants; also retain growing-insert workloads as intentional growth tests.
- For state-copy experiments vary completed writes/aborts independently of live data: 0, 100, 10,000, 100,000 transactions; 1, 100, 10,000 unrelated rows; 1 versus many tables. Include committed history as well as the aborted-history probe used here.
- For query algorithms use 10, 100, 1,000 rows, plus 10,000 where practical. Record rows scanned, expression bindings, AST clones, regex compilations, bytes/rows copied, or cache entries searched as appropriate. Counters are temporary research instrumentation, not a permanent logging requirement.
- Keep release build/features, tracing state, SQLx cache state, PostgreSQL version/settings/transport and output decoding equal across A/B runs. Do not profile setup, cleanup, symbol generation, or another benchmark concurrently.

Use at least three independent timing runs for an implementation decision, inspect Criterion confidence intervals, and rerun the affected tier plus tier 1. Prefer a ≥20% representative-workload gain or clear removal of bad scaling; a smaller result is only interesting when the change also materially simplifies code. Reject extra abstractions or special cases whose benefit disappears at realistic fixture sizes. These are experimental acceptance thresholds, not promises.

## First group — >10× slower

### E1 — Replace whole-database copies with the state an operation actually needs

**Status:** Complete in the current implementation; no new source change was needed during this verification. The native advisory probe measured about 38.4 µs at 0 history and 100,000 completed transactions, and about 39.0 µs with 10,000 unrelated rows. At 100 rows, native locking remained about 202 µs fresh versus 205 µs after 100,000 transactions; trigger work remained about 63.2 µs versus 63.3 µs. SQLx benchmarks measured 57.9 µs for advisory locking, 192.3 µs for the `SKIP LOCKED` queue and 98.6 µs for the trigger workflow, each 59–64% faster than its saved baseline. Source snapshots still copy the referenced table, so that cost grows with rows the query actually reads.

**Original evidence, before the current source-capture implementation.** `execute_query_inner` created `Arc::new(state.clone())` when capturing locking queries; `Session::execute_statement` did the same for triggered inserts. `prepare_insert_rows` had another source-state copy. This copied tables and the transaction-status registry, not just an MVCC snapshot. With 100,000 prior aborted writes, the advisory probe spent approximately 96% of samples cloning/dropping `DatabaseState`; the queue probe about 86%, and trigger workflow about 94%. The advisory probe rose from about 71 µs to 1.51 ms with history alone, and to 2.88 ms with two 10,000-row tables despite touching no table itself. The old full-suite times were consistent with this mechanism; their exact historical state size was not captured, so attributing every millisecond to it would be unjustified.

**Experiment.** First remove source-state capture for a table-free advisory expression when it serves no read source. Then represent locking/trigger source reads with a retained MVCC snapshot and explicit source rows/identities, or a narrow immutable source object. Borrow state while the storage guard is held. Preserve data that must survive a write or a lock wait explicitly. Do not replace a frozen source with arbitrary live reads, hold the mutex while waiting, or introduce a persistent-map rewrite for the entire engine.

**Measure/accept.** Time becomes independent of unrelated rows and completed transaction history; whole-state clone/drop disappears from these profiles. Measure fresh and long-session SQLx cases. Check READ COMMITTED rechecks, REPEATABLE READ failures, SKIP LOCKED ordering/limits, trigger visibility, self-insert sources, statement command IDs, sequence/advisory side effects exactly once, rollback and lock timeouts. The existing database fixture snapshot API remains a separate deliberate copy.

**Locations:** `executor/query/mod.rs::execute_query_inner`, `session/mod.rs::execute_statement`, `executor/writes/insert_preparation.rs`, `database/state.rs::DatabaseState`, `txn.rs::TransactionRegistry`.

### E2 — Compile a row-invariant regex once per execution

**Status:** Complete in the current implementation; no new source change was needed during this verification. The statement-local cache preserves dynamic-pattern and lazy-error behavior. `tier2_runtime_patterns_100_rows/pg_fake` measured 270.34 µs, 71.96% faster than its saved Criterion baseline. Native probes measured about 83 µs, 263 µs and 1.91 ms at 1, 100 and 1,000 rows. The regression test and independent review passed.

**Original evidence, before the current implementation.** Approximately 84% of the patterns probe was under regex building/compilation. `evaluate_regex` validated and compiled the same literal pattern for each row.

**Experiment.** Separate pattern validation/compilation from matching. Bind a literal or parameter-only pattern and flags once for the statement execution, then pass the compiled matcher into the row evaluator. Start with this ownership scope; no global or unbounded regex cache. A varying row-dependent pattern stays on the existing path unless a separate profile justifies more.

**Measure/accept.** One compilation per evaluated invariant pattern instead of one per row; measure 1/100/1,000 rows and repeated versus varying patterns. Preserve case flags, NULL behavior, supported-pattern checks, and lazy error behavior in CASE/short-circuit paths. This removes repeated compilation, not the whole workload's remaining interpreter overhead.

**Location:** `executor/expressions/patterns.rs::evaluate_regex` and the bound-expression path.

### E3 — Keep UUID-array parameters as typed values; prepare membership once

**Status:** Complete in the current implementation; no new source change was needed during this verification. UUID-array binds remain typed through execution, and direct UUID `= ANY($n)` predicates build one membership set per execution. `tier2_uuid_any_100_rows/pg_fake` measured 64.38 µs, 66.16% faster than its saved Criterion baseline. Native probes measured about 25.5 µs, 42.6 µs and 141 µs at 1, 100 and 1,000 rows. The prepared-parameter regression test and independent review passed.

**Original evidence, before the current implementation.** Approximately 86% of the prepared UUID probe was under coercion, with text-array parsing prominent. `bind_parameters` converted UUID arrays to a casted text literal through `create_typed_literal`; general `ANY` evaluation repeatedly reconstructed that array. The existing hashed-membership specialization accepted an AST tuple, not this array cast, so it did not address the benchmark.

**Experiment.** Let the bound expression refer to the parameter value directly, as the existing prepared scalar parameter does. Resolve array element coercion once per execution. For equality membership in a WHERE clause, build a typed membership set once, then scan; independently test probing the existing unique index for small candidate lists. Do the representation fix first so the two benefits are measured separately.

**Measure/accept.** No text formatting/parsing round trip for bound arrays; work approaches O(M + N), or O(M log N) for indexed probes, instead of repeatedly decoding M values for N rows. Cover empty/NULL arrays, NULL elements, duplicates, nonmatches, mixed coercible types, projection versus WHERE three-valued logic, ANY versus ALL, and parameters changing between executions. Index probes must not duplicate output rows.

**Locations:** `analyzer/mod.rs::bind_parameters`, `analyzer/literals.rs::create_typed_literal`, `executor/query/select.rs::execute_any_membership_rows`, `executor/expressions/comparisons.rs`, `executor/prepared.rs`.

### E4 — Keep materialized CTE and recursive working rows out of the SQL AST

**Evidence.** Materialized results become typed `VALUES` AST nodes in `create_cte_values_query`. Recursive execution clones its right-hand query and replaces references on each iteration. The series probe spends roughly a third of samples in AST visitors, with substantial AST cloning and scope binding. CTE materialization already exists; the problem is its representation and repeated analysis.

**Experiment.** Add a simple executor source for materialized typed rows, referenced by a statement-local CTE ID. Bind output columns and the recursive term once; each iteration replaces only the working-row input. Retain the existing recursive work-table algorithm. Apply the same representation to data-modifying CTE RETURNING output after preserving its mutation semantics.

**Measure/accept.** The number of AST nodes no longer grows with materialized row count, and recursive binding count does not grow with iteration count. Separate series depth, branching width, reused CTE count and row width. Preserve materialization barriers, demand/termination behavior, recursive UNION versus UNION ALL, type/typmod resolution, CTE scope shadowing, side effects once, and command visibility.

**Locations:** `executor/ctes/references.rs::create_cte_values_query`, `executor/ctes/recursive.rs::execute_recursive_cte`, `executor/ctes/mod.rs`.

**Current-state audit (2026-09-29).** The
requested typed row-source mechanism is already present: materialized CTE and
data-modifying CTE results are registered under statement-local IDs, and CTE
references are rewritten to those sources instead of embedding result rows in
`VALUES` ASTs. Recursive execution likewise installs each working set under a
row-source ID and builds a prepared recursive-term plan once when the term is
eligible. At the time of this audit, the generic fallback still analyzed
recursive terms that were outside that plan's supported shape, including joined
terms; E4's implementation result below addresses the joined fallback. E5
addresses join pair-comparison cost separately. The focused materialized,
recursive, parameterized-recursive, and data-modifying CTE tests pass. Current
native probes measured one-shot recursive series at 152.50 µs, branching
traversal at 316.55 µs, and repeated CTE self-join at about 96.74 µs for 100
rows (two runs). This audit establishes current behavior, not a before/after
optimization result.

**Implementation result (2026-09-29, independently reviewed; complete).** The prepared recursive-term path now accepts a single streamable
inner join with an `ON` equality over a CTE row source. It binds the joined
scope once and reuses `visit_streamed_join_rows` each generation; other shapes
retain the generic fallback. The native branching traversal probe averaged
241.83 µs over three runs versus a refreshed 307.92 µs over three before runs
(21.5% lower, saving 66.10 µs). The recursive CTE differential test and all
289 core library tests pass, as does formatting validation. Independent review
found no correctness issue. Commit `967da32` completes E4.

### E5 — Reuse the existing hash-join algorithm for materialized sources

**Evidence.** Ordinary base-table equality joins already hash, and materialized CTE row-source IDs are represented as table factors so they use that path. Derived query factors are not streamable and used `materialize_table_with_joins_rows` with nested pair loops. Join-condition evaluation takes about 44% of the materialized CTE profile and about 75% of recursive branching. The self-join tests 10,000 pairs to return 100 rows.

**Experiment.** Reuse equality-key extraction and hash joining over the typed source rows from E4; start with supported inner equijoins. Keep the explicit nested-loop fallback for other conditions. Avoid a general cost-based optimizer or another independent hash implementation.

**Measure/accept.** Eligible joins approach O(L + R + output), and pair comparisons stop growing quadratically. Test duplicate keys, NULLs, numeric equality/coercion, unmatched rows and residual predicates. Add outer joins only with the appropriate unmatched-row semantics, rather than expanding the first experiment indiscriminately. Output-size costs remain unavoidable for many-to-many joins.

**Location:** `executor/from/joins.rs`.

**Experiment result (2026-09-29, independently reviewed; complete).**
The derived-source path now reuses the existing equality-key resolver and key
normalization for eligible inner equijoins over materialized `SourceRow`s;
unsupported types, residual predicates, and outer joins keep their prior
fallback. A new `derived_source_join_100_rows` Criterion benchmark and native
`derived_join` probe cover two 100-row derived inputs. Three native runs
averaged 148.75 µs after the change over three refreshed runs versus 4,123.90
µs before (96.4% faster, saving 3,975.15 µs). At 1,000 rows the probe averaged
979.55 µs over three refreshed runs versus a 400.47 ms baseline (99.76% lower),
changing the observed growth from quadratic to near-linear for this unique-key
fixture. The shared streamed-join path also
measured 47.13 µs for a selective join and 155.63 µs for a many-match join in
single diagnostic runs; these are regression sentinels, not paired comparisons.
Focused tests cover duplicate keys,
NULL and unmatched keys, bpchar trailing spaces, mixed-numeric equality
coercion, and same-type residual-predicate fallback. The 289-test core suite,
five SQLx runtime
differential tests, all 21 view tests, formatting, and benchmark compilation
pass. The registered Criterion runner still requires the unavailable local
PostgreSQL service; native probes supply the paired timings. Independent review
found no remaining issue. Commit `c5adffb` completes E5.

### E6 — Index saved mutation inputs instead of searching and comparing the AST per row

**Status: Complete (commit `98db88a`).** Saved UPDATE inputs are indexed by
`RowId`, and the update span and statement cache entry are computed once
outside the target-row loop. The 100-row DML-CTE benchmark improved from 3.8 ms
to 1.3 ms; the 1,000-row case improved from 255 ms to 9.6 ms.

**Evidence.** `prepare_update_rows` searches `prepared_update_inputs` linearly for every target, comparing statement spans/ASTs before matching the row ID. Each cached row owns another update AST. The 100-row DML-CTE probe spends around 40% of samples computing UPDATE spans. There are also repeated target-to-prepared-row searches in lock discovery.

**Experiment.** Compute statement identity/span once. Store the immutable statement once and key saved row inputs by statement invocation plus RowId; validate version identity/current input when required. Move invariant AST comparisons and formatting outside row loops. Borrow the AST for ordinary evaluation and save owned state only when it must survive suspension. Keep evaluation order and resumable progress explicit.

**Measure/accept.** Span computation/AST comparison is O(1) per statement identity, row lookup is O(1) expected or O(log N), and saved AST storage is independent of affected-row count. Compare 1/100/1,000-row UPDATE and DML CTEs. Preserve volatile expressions, trigger calls and sequence allocation exactly once after waits; include rows changed by another transaction, partial failures and retries.

**Locations:** `executor/writes/update.rs::prepare_update_rows`, `executor/context.rs`, `executor/locks/mod.rs`, `executor/expressions/resume.rs`.

### E7 — Bind invariant expression information before scanning rows

**Evidence.** The temporal probe spends about 42% in type inference, versus about 8% in the sampled temporal-function category and 4% in numeric division. Many-match joins also spend substantial time resolving columns/types and walking expression trees. `evaluate_and_coerce` re-infers types; expression evaluation repeatedly detects advisory functions and subqueries. The existing `PreparedExpression` already demonstrates a simpler slot/type-based representation, but supports only a small subset.

**Experiment.** Extend that existing representation incrementally to common scalar functions/casts and ordinary fallback predicates/projections. Resolve column slots, operand/result types, coercions and feature flags once. Keep runtime values, volatile evaluation and contextual settings at execution time. First cover the measured temporal expression and basic join predicates; do not build a new parallel compiler or merge unrelated visitors solely to reduce their count.

**Measure/accept.** Type inference and structural feature detection scale with expression size, not rows × expression size. Run tier 1 joins, temporal queries, JSON/array expressions and scalar subqueries. Preserve error codes, type/typmod metadata, CASE laziness, timezone changes, volatility and correlated scopes. Reprofile before considering any arithmetic/chrono/allocator tuning.

**Locations:** `executor/prepared.rs`, `executor/expressions/mod.rs::evaluate_and_coerce`, `executor/expressions/resume.rs::evaluate`, `executor/subqueries.rs::evaluate_query_expression`, `executor/query/projection.rs`.

**Progress (2026-09-29, pending review and user approval).** The existing
prepared-expression path now binds numeric casts and the measured scalar
functions `to_timestamp`, `date_trunc`, `to_char`, and `floor` into typed
expression nodes. Runtime timezone is supplied at execution, so a reused plan
observes session timezone changes. Mixed-type comparisons retain the generic
path because point lookup needs its existing comparison coercion. The prepared
100-row temporal probe averaged 268.29 µs over three refreshed runs, versus
603.23 µs before (55.5% faster, saving 334.94 µs). A prepared ordered-read
guard averaged 13.00 µs. Regressions confirm that the prepared plan is selected
and matches forced-generic execution in UTC and America/New_York, including
contextual coercion of `to_timestamp('1')` and projection labels for functions,
casts, and nested expressions. All 288 core tests, the migration data-transform
differential suite, the prepared-reuse end-to-end test, formatting, and
benchmark compilation pass. Independent review found no remaining issue; user
approval is pending.

### E8 — Build view wrappers structurally and expand once per analyzed statement

**Progress.** `freeze_view_output` already builds its projection/derived-table wrapper directly from the AST in the current source, so the original formatting/reparse hotspot no longer applies. A fresh native 100-row view profile measured 3.1% in `freeze_view_output`, 31.9% in scope binding, 31.1% in AST visitors, and 30.7% in AST cloning. For prepared queries, cache the recursively expanded view statement at prepare time and execute that statement directly, skipping the repeat view-probe traversal. The SQLx nested-view benchmark improved from 408.41 µs before this change to 376.12 µs after it (7.9%, 32.29 µs); `cargo test -p pg_fake --lib`, all 21 view tests, formatting, and independent review passed. Prepared-query regressions cover parameter binding, fresh base rows, `search_path` reprepare, catalog invalidation, and rollback after a transactional view replacement. Await user approval before marking E8 complete.

**Evidence.** The saved pre-change profile attributed about 22% to `freeze_view_output`, which formatted and reparsed a query. That evidence is stale for the current source; direct AST construction is already present. The remaining current profile is dominated by scope binding, AST traversal, and cloning.

**Experiment.** Keep the structurally constructed wrapper. Share the expanded statement within the explicit prepared plan, retaining dependencies from the original view references so catalog invalidation remains correct. Consider predicate pushdown through a simple nonvolatile projection/filter only as a separate measured follow-up.

**Measure/accept.** No internal SQL text reparse; expansion/binding count is independent of output rows and does not repeat across statement phases. Vary view depth and width. Cover frozen output names, quoted identifiers, aliases, nested/recursive view detection, transactional CREATE OR REPLACE/DROP, search_path and prepared-plan invalidation. Keep this within existing catalog-dependency rules, not a new global cache.

**Locations:** `executor/views/expansion.rs::freeze_view_output`, `executor/views/binding.rs`, `session/prepared.rs`.

### E9 — Bind a lateral inner query once, then supply outer-row slots

**Evidence.** The lateral workload repeatedly executes an inner scan for 100 outer rows; profiles put substantial time in AST visitors and type inference, while top-k maintenance is small. The fixture has ten distinct parent keys repeated across 100 rows. Existing uncorrelated initplan reuse is already implemented.

**Experiment.** First reuse the bound inner plan with explicit outer-value slots, using E7 rather than cloning/substituting and reanalyzing expressions for each invocation. Then independently test a statement-local index/grouping of the inner relation for the equality predicate. Parameter-result memoization is a separate optional experiment only for a proven nonvolatile read-only subplan; it is not safe for arbitrary lateral SQL.

**Measure/accept.** Binding count is constant; scan work is measured against outer rows and distinct keys. Test repeated versus all-distinct parent keys, duplicate children, no child (LEFT JOIN NULL extension), ORDER BY/LIMIT ties, volatility, nested correlation and lock-bearing subqueries. Retain the simple fallback when eligibility is unclear.

**Locations:** `executor/lateral.rs`, `executor/lateral/initplans.rs`, `executor/from/subqueries.rs`, `executor/from/joins.rs`.

**Experiment result (2026-09-29, pending user approval).** A cached parameterized inner-query template measured 7.248 ms before and 7.177 ms after, so that extra binding/cache machinery was discarded. A statement-local result memo for correlated queries is retained only when the fully filtered query is read-only, nonvolatile, and non-locking. Its key includes the substituted query text, source span, and invocation path. The refreshed repeated-key SQLx workload measured 7.248 ms before and 1.664 ms after (77.1% lower); the intermediate retained-path runs were 1.623, 1.627, and 1.628 ms. Tests cover repeated and distinct keys, nested correlation, a pushed outer-join filter, top-level parameters, duplicate children with ORDER BY/LIMIT, LEFT JOIN NULL extension, and per-row `nextval`. Streaming lateral queries remain unchanged.

### Migration checkpoint

The migration workload combines SQLx bookkeeping/advisory locking, DDL, a windowed CTE UPDATE, sequences, foreign keys, index creation and a view. Attribute its components rather than creating a “migration optimization” from the aggregate timing. Rerun the exact migrator after E1, E4/E6 and E8, with fresh and accumulated history, and only promote a remaining component when a profile identifies it. The adapter's migration lock/unlock paths use the same advisory-query machinery as E1. An actual SQLx `Migrator::run`/`undo` probe using the benchmark's four migration definitions measured 2.06 ms from an initially fresh database and 7.44 ms after 100,000 aborted writes. In the latter profile, database clone/drop samples are about 79% of samples under core Session methods (not 79% of all thread samples or end-to-end wall time).

## Second group — Remaining tier 1 work

### E10 — Remove per-output SELECT copies from ordinary ordered reads

**Evidence.** `execute_query_inner` constructs a `DerivedProjection { select: select.clone(), ... }` for every deferred output row, even when the origins vector is empty. The ordered-read profile spends approximately 22% in AST cloning; sorting itself is around 7%. A bounded heap for LIMIT/OFFSET already exists, as does deferred projection.

**Experiment.** Do not construct row-origin projection metadata when no origin requires it. Where it is needed, share one immutable SELECT description and keep the row-specific source separately. Retain the existing top-k and delayed projection algorithms.

**Measure/accept.** Zero SELECT clones per ordinary output row; at most one shared description per query for origins that need it. Vary output width/count and LIMIT/OFFSET. Preserve lazy projection, ordering/NULL placement, ties, lock rechecks and views that depend on row-origin metadata.

**Locations:** `executor/query/mod.rs` near construction of `DerivedProjection`; `executor/query/select.rs`, `executor/query/ordering.rs`.

**Status: Complete (2026-09-29).** The current source already
meets the acceptance condition: ordinary deferred output rows with no origins
skip `DerivedProjection` construction, while origin-bearing rows share one
`Arc<Select>` per query and retain their own source row. No additional source
change was retained. Current ordered-read timings were 73.847 µs for
`limit_offset_ordered_100_rows/pg_fake` and 76.197 µs for
`order_by_100_rows/pg_fake`, versus the immediately preceding measurements of
73.557 µs and 76.822 µs respectively. The differences (+0.4% and −0.8%) are
within run-to-run noise and do not establish a new speedup. The library tests
(282) and view tests (21) passed. Added
`refreshes_ordered_view_projection_after_lock_wait` to exercise the view-backed
ordered lock-recheck path; it passes and independent review found no issue. The
registered scaling benchmarks vary 100/1,000 output rows, 2/8 projected
columns, and a 1,000-row `LIMIT 10 OFFSET 400` page. Their current means were
449.0 µs (100×2), 489.9 µs (100×8), 872.3 µs (1,000×2), 1.28 ms (1,000×8),
and 656.0 µs (page); the standard harness detected no significant change
against the just-recorded current-state baselines. These measurements describe
scaling and do not imply a before/after E10 improvement. The
full package test run fails at the
differential test `matches_global_aggregate_results`, which reports reversed
text `min`/`max` results for
`SELECT min(label), max(label) FROM (VALUES ('a'), ('MiXeD')) AS labels (label)`;
its later mutex-poison failures are cascades. Independent review of the
implementation found no correctness issue; existing and new tests cover
NULL/tie ordering, LIMIT/OFFSET, row-lock rechecks, derived projections, LEFT
JOIN NULL extension, and the view-backed ordered recheck.

### E11 — Stop rebuilding and copying all settings for ordinary statements

**Evidence.** Creating/applying the GUC execution context accounts for about 32% of the native insert profile and 20% of the update profile. It clones current/commit settings, recreates defaults including maps/search-path formatting, and copies them back. Even native prepared point reads pay transaction-level settings copies in autocommit; holding a transaction open hides this cost in the existing prepared diagnostic.

**Experiment.** Keep immutable defaults at DB/session scope. Share an unchanged settings snapshot or borrow it within execution; use an explicit changed-settings record or copy on actual SET/set_config mutation. Start by removing default reconstruction and unconditional copy-back. Choose the smallest representation that makes commit/rollback behavior clear; no global state.

**Measure/accept.** Plain DML/read work does not scale with the number of untouched settings. Measure insert, update, transaction insert and prepared point read with default settings and many custom settings. Preserve SET LOCAL, RESET/RESET ALL, savepoints, aborted transactions, search_path, timezone-sensitive casts and changes made during an expression.

**Locations:** `session/settings.rs::create_guc_execution_context` / `apply_guc_execution_context`, `session/transactions.rs::start_transaction`, settings capture/restore.

**Progress (2026-09-29, pending approval).** `Session` now keeps one shared
immutable default-settings snapshot. Each statement reuses it, and GUC
write-back updates session/current-on-commit/custom state only when expression
evaluation changed that state. Before/after native measurements with 0 versus
32 custom settings were effectively flat across settings counts. For 0
settings, insert improved from 23.87 to 14.71 µs (38.4%), update from 32.83 to
23.85 µs (27.4%), and transaction insert from 23.87 to 14.96 µs (37.3%). For
32 settings the corresponding improvements were 37.8%, 26.9%, and 37.7%. The
prepared point read stayed near 1.35–1.46 µs; the default-settings run regressed
by about 0.10 µs, while the 32-setting run was within 1% of baseline. Existing
SQLx benchmarks also improved in adjacent filtered runs: insert 58.49 → 48.68
µs, update 47.51 → 36.41 µs, and transaction insert 58.61 → 47.05 µs. Unit,
view, array, migration-transform, and compatibility-utility suites pass. Tests
check `set_config` persistence and savepoint rollback, explicit transaction
commit/rollback, and discarding a setting change from a failed statement. The
complete `pg_fake` library suite passes (285 tests), and formatting checks
pass. Independent review found no correctness issue; it verified the shared
defaults, conditional write-back, rollback/savepoint restoration, and
failed-statement behavior. E11 remains pending user approval.

### E12 — Validate point updates without copying the entire target table

**Status:** Complete. Commit `e78386d` replaces the validation table clone with a statement-local pending-key overlay. The native point-update probe at 10,000 target rows improved from 3,520 µs to 32.8 µs; `tier1_update_row/pg_fake` improved from about 105.3 µs to 46.9 µs.

**Evidence.** `prepare_update_rows` clones the target `Table` as a validation workspace before checking updated rows. This includes version chains and indexes even for one indexed target. The saved tier 1 update fixture contains one live row, so it conceals table-size scaling. The native point-update probe rises from 53 µs with 100 live target rows to 211 µs with 1,000, despite updating one indexed row. At 1,000 rows, table destruction plus row-storage cloning account for approximately 64% of samples (index-copy work is additional).

**Experiment.** Check committed/visible keys using the existing indexes and represent only this statement's pending key changes for uniqueness validation. Preserve the current conflict ordering, including interactions between multiple rows updated in one statement. Avoid copying an unrelated set of row versions just to validate one changed row.

**Measure/accept.** Point-update latency and copied bytes no longer grow linearly with unrelated target-table rows. Profile 1/100/1,000/10,000 live rows, then measure multirow updates with key collisions, NULL unique keys, partial indexes, concurrent versions and triggers. This is a separate experiment from E6's replay lookup.

**Location:** `executor/writes/update.rs::prepare_update_rows`, `storage.rs` uniqueness checks.

### E13 — Make existing preparation useful across common paths

**Evidence.** The saved core diagnostic shows 18.56 µs parsing/analyzing versus 0.441 µs for prepared reuse **inside an existing transaction**. SQLx already has a bounded statement cache, but cache hits clone the owned `PreparedStatement`, and the reusable query plan rejects ORDER BY/LIMIT/joins and does not cover DML. Parameterized fallback execution substitutes typed literals into an AST. These are distinct costs; “add a cache” is not the experiment.

**Experiment.** After E7/E10/E11, compare unchanged one-shot workloads with explicit prepared reuse. Let SQLx cache entries share immutable prepared statements rather than deep-cloning their AST/plan on each hit if profiling still supports that cost. Extend existing bound execution to common ordering/limits and DML without duplicating semantic logic. Give the changing-ID insert/update benchmark a bound-parameter companion; keep the original measurement visible.

**Measure/accept.** Report prepare time, first execution, warm execution and autocommit separately. Preserve parameter metadata, transactional DDL dependencies, search_path changes, result-type change errors and explicit-statement semantics. A paired warm parameterized point-read probe measured 15.76 µs through SQLx versus 2.68 µs through the native prepared API, both in autocommit on the same fixture shape. This includes the adapter/runtime boundary and prepared-statement cloning; it does not isolate either cost. The existing 100-row adapter diagnostic hides much of this fixed overhead behind ordered-query work. Do not promise that the 42× core diagnostic transfers to SQLx latency. Do not move potentially blocking SQL onto an async runtime thread merely to remove spawn_blocking overhead.

**Locations:** `session/prepared.rs`, `executor/prepared.rs`, `pg_fake_sqlx/src/connection.rs::run`.

**Progress (2026-09-29, pending user approval).** The SQLx persistent
statement cache now shares immutable prepared statements through `Arc`; cache
hits no longer deep-clone the prepared AST and query plan. Explicit statements,
cache misses, and nonpersistent executions keep owned prepared statements.
Three optimized direct SQLx point-read probe runs averaged 32.87 µs before and
30.46 µs after (7.3%, or 2.41 µs faster). This controlled same-build
comparison is distinct from the earlier sampled probe's 15.76 µs result. A
registered warm bound point-read benchmark now covers this cache-hit path.
Cached-plan recovery stays inside SQLx's persistent-cache path: it refreshes
before aborting an explicit transaction and rejects changed result columns.
Tests cover table recreation inside a transaction and `SELECT *` result-shape
changes. The core library (285 tests), SQLx unit tests (3), SQLx driver tests
(17), formatting, and benchmark compilation pass. The direct Criterion runner
could not connect to PostgreSQL in this sandbox; the standalone SQLx probe
supplied the A/B timings. Independent review found no remaining issue after
checking cache invalidation, result-column preservation, and explicit-
transaction recovery.

The prepared query plan now also handles `ORDER BY` keys that name projected
columns and simple literal/NULL `LIMIT`/`OFFSET` clauses. Other ordering forms,
nonliteral row counts, `FETCH`, unordered finite limits, finite limits with
computed projections, and all `LIMIT 0` queries keep the generic executor
fallback. The
existing prepared 100-row, two-key order probe improved from 46.58 µs to
12.62 µs across three runs (72.9%, 33.96 µs saved). Its ordered-page probe
improved from 41.48 µs to 10.28 µs (75.2%, 31.20 µs saved). A paired SQLx
probe for a parameterized 50-row ordered read averaged 128.91 µs with warm
persistent caching and 153.44 µs with caching disabled (16.0%, 24.53 µs saved).
A registered SQLx cache-hit benchmark covers parameterized ordering and ordered
pages; it compiles, while the Criterion runner still requires an unavailable
PostgreSQL connection. The core suite passes all 286 library tests; the SQLx
suite passes when its existing `reports_phase2_regression_progress` gate is
skipped. That gate currently reports 816/850, and the core differential suite
has the previously recorded global text `min`/`max` mismatch. E13 remains
pending user approval.

Bound-DML companion probes were then added to the benchmark registry and
`benches/workloads.rs`, with separate databases for inline and parameterized
variants so the INSERT rows and UPDATE target stay equivalent. The research
adapter probe measured bound INSERT at 26.29 µs versus 40.62 µs inline (35.3%
lower), and bound UPDATE at 41.09 µs versus 49.21 µs inline (16.5% lower).
Both use changing values with a stable parameterized SQL string; the inline
case still changes SQL text. Benchmark compilation and formatting pass. The
registered Criterion runner has not been executed because it starts a local
PostgreSQL container, unavailable in this environment. E13 remains pending
user approval.

### Coverage and targets for every tier 1 group

| Group | Saved pg_fake µs | Saved PG µs | 10× goal µs | Main experiments |
| --- | ---: | ---: | ---: | --- |
| insert_row | 61.77 | 93.13 | 9.31 | E11, E13; E7 if constraints remain significant |
| update_row | 53.93 | 91.70 | 9.17 | E11–E13, E6 |
| transaction_insert | 65.01 | 136.23 | 13.62 | E11, E13; separate boundary cost |
| select_100_rows | 33.94 | 59.72 | 5.97 | E13; measure result ownership after fixed costs |
| select_where_100_rows | 19.76 | 34.65 | 3.47 | E13, E11; existing scan path |
| select_where_indexed_100_rows | 13.00 | 31.68 | 3.17 | E13, E11; existing unique lookup |
| limit_offset_ordered_100_rows | 83.57 | 42.68 | 4.27 | E10, E7, E11, E13 |
| order_by_100_rows | 124.79 | 67.06 | 6.71 | E10, E7, E11, E13 |
| selective_inner_join | 106.63 | 40.96 | 4.10 | E7, E11, E13; preserve existing pushdown/hash join |
| many_match_inner_join | 166.00 | 67.47 | 6.75 | E7, E11, E13; output-size floor |

These targets are acceptance goals to measure toward, not forecasts that one pass will attain them.

## Third group — Tier 2, then tier 3

### E14 — Read aggregate/window outputs from slots rather than rewriting expression ASTs per row

The five-window offset probe spends about half its samples materializing window expressions, including substantial cloning and traversal. `materialize_window_expression` substitutes computed values into cloned expressions; related aggregate output code does the same. Bind references to aggregate/window result slots once and evaluate those slots per output row. Borrow unchanged expressions when there is no aggregate substitution.

First measure tier 2 grouped/ordered aggregates and row_number; then tier 3 multi-window and moving-frame workloads. Preserve separate volatile occurrences, FILTER/order semantics, peer groups, default frames, offsets and NULL behavior. The code also rebuilds partition/order information per window function; only combine identical deterministic specifications after the first change is profiled. Do not start with a moving-window algorithm rewrite when the current profile points elsewhere.

Locations: `executor/query/windows.rs::materialize_window_expression` / `calculate_window_values`, `executor/query/grouping/mod.rs::materialize_aggregate_expression`.

**Progress (2026-09-29, pending user approval).** A refreshed native five-window
probe averaged 1,076.64 µs before this change. The first loop-only rewrite
removed the temporary matching-index vector but saved only 4.2% (45.25 µs), so
it was refined before retention. The final change avoids the AST-keyed
occurrence map for the common single-match case, while retaining occurrence
tracking for duplicate volatile window calls. Review also found that
deterministic calls were deduplicated across owners even though materialization
is owner-specific. Collection now deduplicates within an owner; a regression
with the same `row_number()` in two projections and `ORDER BY` reproduced the
prior error and passes after the fix. On the final source, three runs averaged
799.17 µs, 25.8% lower (277.47 µs saved). A new native `row_number` probe
currently
measures 134.4–136.0 µs; the existing grouped aggregate probe measures
175.4–178.5 µs. Those are current referents, not before/after claims. The
refreshed post-change profile still identifies `materialize_window_expression`
and AST cloning as material costs, so a larger slot-binding follow-up remains
open. A follow-up that precomputed an owner-specific AST-to-slot map measured
808.23 µs over three runs, 1.1% slower than the retained implementation's
799.17 µs mean; it was discarded because its extra structure did not improve
latency. The core library (286 tests) and all seven migration data-transform
tests pass. E14 remains pending user approval.

The reusable-template follow-up replaces each owner-bound window occurrence
with a private slot marker once before output-row evaluation. Each row then
fills those markers from the already-computed window values instead of
re-matching full function ASTs. Three direct native five-window probe runs
averaged 563.11 µs versus 797.48 µs for three fresh baseline runs in the same
build environment (29.4% faster, saving 234.37 µs). A 1,000-row five-window
probe measured 4.81 ms; the single-window probe measured 114.7–117.6 µs, and
the grouped-aggregate probe measured 178.5–180.7 µs. These latter timings are
current reference measurements, not before/after claims. A regression verifies
that marker text used as a quoted SQL column name is not substituted. The core
library (286 tests), all eight migration data-transform tests, formatting and
benchmark compilation pass. Independent review found no remaining issue. E14
remains pending user approval.

A fresh profile showed AST visitors at 35.7% inclusive and
`calculate_window_values` among the largest self-sample symbols, so an
experiment reused partition/order keys and sorted groups for identical
deterministic window specifications. The five-window probe averaged 459.20 µs
versus a fresh 554.39 µs baseline (17.2% faster, saving 95.19 µs). Because the
cache adds ownership and lookup machinery and misses the plan's 20% preference
without simplifying the code, the experiment was discarded. After reverting
it, three runs averaged 556.45 µs, within 0.4% of the fresh baseline. E14's
retained template result remains unchanged.

### Experiment-level commit and timing audit (2026-09-29)

Commit `2ef24b5` accidentally bundled changes from E7, E10, E11, E13, E14,
and the high-cardinality grouping follow-up into one 85-file commit (5,881
insertions, 163 deletions), including raw Criterion outputs and research
profiles. It is slated for reversion and must not be treated as an accepted,
experiment-sized implementation commit. Keep accepted code changes in separate
commits by experiment, with the measured before/after timing in each commit
message. Record compact benchmark summaries and reproduction instructions;
include raw profiles or Criterion samples only when needed to reproduce or
review a result.

The bundled changes' measurements do not justify treating every change as an
optimization. E7's prepared temporal path improved 603.23 → 268.29 µs (55.5%),
and E11's insert/update/transaction-insert paths improved by 26.9–38.4%, with
flat scaling from 0 to 32 custom settings. E13's prepared ordered-read and page
probes improved 72.9% and 75.2%; the parameterized SQLx ordered-read probe
improved 16.0% (24.53 µs). Its separate persistent-cache change improved the
point-read probe 7.3% (2.41 µs), below the plan's preferred 20% gain; decide
explicitly whether that absolute saving warrants the added cache-sharing
machinery before retaining it. E13's bound INSERT probe improved 35.3%, while
bound UPDATE improved 16.5%; retain these as separate measured cases. E14's
window-template change improved the five-window probe 29.4%, and the
high-cardinality grouping change improved the 1,000-group probe 32.0%.

E10 established no significant before/after gain: its ordered-read scaling
measurements describe current costs only. Do not present E10 as a speedup or
retain a source optimization on that evidence. The additional identical-window
specification cache measured 17.2% faster but added ownership and lookup
complexity, so it was discarded; the retained E14 template result is the
separate 29.4% improvement above.

### Reprofile before adding further optimizations

| Remaining workload family | Next decision after shared improvements |
| --- | --- |
| Defaults, RETURNING, identity, FK writes, deletes, upserts, UPDATE FROM, nested savepoints | Remeasure E1/E3/E6/E11/E12 benefits; preserve sequence and constraint semantics. Do not assume constraint/index lookup is linear. |
| JSON/JSONB, array containment, temporal binding, UUID arithmetic | Apply typed-value/expression work where measured; inspect coercion/materialization before inventing new indexes. |
| Grouping, DISTINCT, UNION/UNION ALL, ordered aggregates | Group lookup currently scans groups, but the small ten-group probe does not establish it as the dominant cost. Profile high group cardinality before a hash-group experiment. Keep existing set/dedup algorithms until measured. |
| Derived/scalar subqueries, correlated EXISTS, read CTEs | Existing EXISTS/uncorrelated reuse already handles some shapes. Compare those paths with general execution after E4/E5/E7. |
| DDL, ALTER rewrite, catalog introspection, indexes, temporary tables | Measure schema-count/dependency scaling separately from history and ordinary execution overhead. Keep DDL invalidation and visibility explicit. |
| Database snapshots | The 100-row fixture snapshot is about 15 µs in the baseline. No persistent-map conversion without evidence from larger realistic fixtures and their write cost. |
| Uncontended concurrency, SERIALIZABLE, explicit table locking | Preserve the specified single-lock architecture for now; measure shared per-statement overhead first. |
| Old MVCC snapshots and transaction histories | Point lookup is already essentially flat through 100,000 completed transactions. The 10,000-version old-snapshot probe is slow, but belongs after normal workloads. Profile retained-chain traversal and reclamation before changing either. |
| Forced same-row contention | The benchmark deliberately requests a 100 µs Tokio sleep; scheduler/timer behavior also affects its observed wait. Do not count this deliberate wait as executor inefficiency or remove it to improve the score. |
| Specialized settings, JSONB joins, recursive/locking combinations | Rerun after the common fixes and promote only measured residual costs. |

**High-cardinality grouping follow-up (2026-09-29, pending user approval).**
Profiling a 1,000-group `SELECT id, count(*) FROM t GROUP BY id ORDER BY id`
showed the linear group match as the dominant collection cost. A statement-local
hash lookup now handles grouping types with existing equality keys; unsupported
types retain the PostgreSQL equality scan, comparing each key using its bound
type so bpchar trailing spaces behave consistently when another key disables
hash lookup. After that fix, three probe runs averaged 3,099.06 µs, versus the
four-run 4,554.83 µs baseline (32.0% faster, 1,455.77 µs saved). The
10,000-group probe measured 37.96 ms; the ten-group probe remains near 181 µs.
Focused tests cover NULL, bpchar trailing-space and JSONB equality, mixed
hashable/unhashable keys, and grouped execution resumption. Independent review
found no remaining issue; user approval is pending.

## Caching boundary and simplicity rules

The user approved investigating missing reuse/preparation while keeping cache proposals separate. E2's per-execution compiled matcher, E3's per-execution membership structure and E4's CTE working rows are explicit execution state. E13 improves the existing prepared path. Optional lateral result memoization needs its own volatility/visibility proof. A transparent cross-statement core parse/plan cache would conflict with specification §2.1 and is excluded from this plan unless separately approved along with a specification change.

Avoid custom allocators, unsafe pointer identities, SIMD, lock sharding, JITs, parser replacement, broad persistent storage changes, and scattered query-specific fast paths. Prefer existing enums/plans, references or small shared immutable objects, and work proportional to rows actually touched. Do not eagerly evaluate expressions to save time when that changes side effects or errors.

For each implementation experiment: retain the before measurement, make one conceptual change, run relevant PostgreSQL differential/property/concurrency tests, profile and time again, review whether the code became simpler, and either retain the result or discard it. Reassess the ordering after E1 because it can change the long-session benchmark picture dramatically. Per project instructions, obtain subagent review and user approval before marking implementation tasks complete.
