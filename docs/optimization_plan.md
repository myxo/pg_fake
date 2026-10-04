# Tier 1 optimization plan

**Status: implementation authorized. O1–O3 are complete; O4–O9 remain pending.**

Research date: 2026-10-03. Scope: make everyday inserts, updates, transactions,
selects, ordering/paging and simple inner joins substantially faster while
preserving the project's PostgreSQL fidelity contract. This document is separate
from the Phase 4 compatibility plan in [plan.md](plan.md).

The strongest opportunities are to compile ordinary join execution, remove
general-purpose write preparation from simple mutations, and reduce SQLx handoff
cost. The statement cache also needs a real memory bound: a controlled insertion
experiment retained approximately 600 MiB more memory with unique literal SQL
than with bound parameters or caching disabled. Replacing the storage map or
adding a different join algorithm is not the first step.

## Evidence and measurement limits

The checked-in [benchmark report](../crates/pg_fake_benchmarks/results/report.md)
was inspected alongside the timed SQL and executor paths. Fresh research adds:

- A complete Tier 1 PostgreSQL comparison: 20 Criterion samples, 1-second warmup
  and 2-second measurement per benchmark. The clean rerun reproduces the major
  findings of the saved report. An earlier run overlapped a compiler and was
  discarded; Criterion's printed change percentages compare transient local
  measurements, not the checked-in report, and are not optimization evidence.
- 49 isolated probe configurations, each run in three fresh processes for
  0.5 seconds after 64 warmup operations. Values below are medians of run means,
  not percentile latencies or Criterion confidence intervals.
- Eight fresh macOS `sample` profiles, each sampling for 3 seconds at a nominal
  1 ms interval during a 5-second workload. They cover prepared point/full reads,
  joins, paging, update, insert/delete cycles, and a cached SQLx point read.
- Equal-size memory experiments: 32,768 measured inserts plus 64 warmup inserts
  into the same constrained table, using `/usr/bin/time -l` peak resident bytes.

The machine is Apple M2 / aarch64, macOS 15.7.9, Rust 1.98.1. Probes use release
optimization with debug symbols and one Tokio worker; the comparison suite uses
its existing runtime configuration. All database execution is local. Fixtures,
preparation and process startup are outside probe timers; result destruction is
inside. Native reads here use autocommit, whereas some existing diagnostic
benchmarks keep an explicit transaction open. Their sub-microsecond numbers are
therefore not interchangeable with these autocommit measurements.

Probe tables `l` and `r` have `id`, `bucket`, and a text payload; the existing
join benchmarks have only two integer columns. Probes use short SQL/table names,
and update a stable constrained row rather than inserting/deleting its fixture
around every timed update. These are diagnostic workloads, not replacements for
the official benchmarks. Large fixtures are seeded in batches of 100 to avoid
quadratic same-statement unique validation during setup. Insert/delete and
begin/insert/rollback cycle measurements include the entire named cycle; they
are not isolated insert or commit timings.

Durable inputs, timing triples, Criterion mean confidence intervals, profiles,
memory measurements, source fingerprints and compressed samples are retained
locally outside version control.
The older `target/optimization_research` profiles were inspected but not used to
quantify the current plan. No production engine or adapter implementation was
changed during this research.

### Fresh Tier 1 comparison

All timings are arithmetic means in microseconds. PostgreSQL includes its
normal SQLx, protocol and local-server costs, so these are application-facing
comparisons, not equivalent executor CPU measurements.

| Existing benchmark | pg_fake | PostgreSQL | PostgreSQL / pg_fake |
| --- | ---: | ---: | ---: |
| Insert row | 41.72 | 93.39 | 2.24x |
| Update row | 33.27 | 97.10 | 2.92x |
| Transaction insert | 43.84 | 137.55 | 3.14x |
| Select 100 rows | 27.69 | 58.07 | 2.10x |
| Heap equality select, 100 rows | 14.16 | 34.58 | 2.44x |
| Indexed equality select, 100 rows | 8.21 | 34.46 | 4.20x |
| Ordered limit/offset, 100 rows | 32.75 | 42.14 | 1.29x |
| Multi-key ordering, 100 rows | 39.40 | 66.16 | 1.68x |
| Selective inner join | 89.68 | 39.47 | 0.44x |
| Many-match inner join | 158.63 | 65.04 | 0.41x |

Both join cases are more than twice as slow as PostgreSQL. Paging barely wins.
The remaining advantages are useful but fall short of the specification's
orders-of-magnitude aspiration.

### Isolated execution costs

| Probe, 100 rows per source | Native execute | Native prepared reuse | SQLx cached query |
| --- | ---: | ---: | ---: |
| `SELECT 1` | 9.23 | 6.47 | 14.53 |
| Full select, 100 output rows | 17.78 | 10.39 | 25.67 |
| Heap equality, one output row | 13.02 | 4.96 | 11.26 |
| Primary-key equality, one output row | 9.68 | 1.32 | 8.24 |
| Multi-key order, 100 output rows | 31.12 | 20.22 | 38.51 |
| Ordered paging, 10 output rows | 29.27 | 19.08 | 30.22 |
| Selective inner join, one output row | 87.75 | 76.31 | 90.53 |
| Many-match inner join, 100 output rows | 154.57 | 144.62 | 162.55 |
| Update one constrained row | 29.77 | 21.89 | 35.19 |

Native `query(sql, &[])` prepares again on every call: the indexed probe takes
19.69 us through that API. The same SQLx point query takes 18.47 us through the
raw string executor, 32.26 us with `.persistent(false)`, and 8.24 us cached.
Consequently, the existing `adapter_overhead_select_100_rows` diagnostic cannot
measure adapter overhead by subtracting its two results: its core path prepares
on each call while SQLx reuses a prepared statement. The approximately 6.9 us
gap between equivalent prepared core and cached SQLx point probes includes
handoff, cache lookup, conversion and different API machinery; it is not a
precise measurement of `spawn_blocking` alone.

| Prepared scaling probe | 100 source rows | 10,000 source rows |
| --- | ---: | ---: |
| Heap equality | 4.96 | 406.45 |
| Unique-index equality | 1.32 | 1.32 |
| Ordered paging, `LIMIT 10 OFFSET 40` | 19.08 | 1,559.54 |
| Multi-key ordering | 20.22 | 2,319.14 |
| Selective join, heap tables | 76.31 | 6,360.09 |
| Same join, primary keys on both tables | 33.69 | 1,954.92 |
| Heap join with explicit equality filters on both sides | 107.26 | 8,713.98 |

Adding `AND r.id = 50` slows the existing interpreted heap join despite reducing
its hash-build input. Likewise, filtering both bucket inputs slows the 100-row
many-match join from 144.62 to 294.74 us. Per-row filtering overhead is substantial;
predicate propagation alone is not a demonstrated improvement. Both indexed
join tables still scale poorly because the right-side index is not used to
probe the left equality key in this workload.

### Profile findings

Percentages are approximate, inclusive shares of retained active stack samples,
except the allocator column, which counts leaf frames. Categories overlap and
must not be summed. Inlining, sampling bias and deduplicated symbols limit
precision. The stack-collapse tool removes common waiting stacks; the summary
also removes remaining recognizable wait leaves. The SQLx raw profile has
9,064 thread samples but only 1,453 retained active samples. Waiting is evidence
of handoffs, not CPU time spent computing results.

| Profile | Principal inclusive categories | Allocator leaf share |
| --- | --- | ---: |
| Selective join | Source scans 65.6%; expression evaluation 43.9%; type/name resolution 27.4%; AST traversal 19.0% | 19.5% |
| Many-match join | Expression evaluation 54.9%; scans 36.3%; type/name resolution 27.2%; AST traversal 22.8%; hash-join frames 8.1% | 18.6% |
| Prepared update | Type/name resolution 9.0%; expression evaluation 9.4%; AST cloning 8.2% | 32.6% |
| Prepared insert/delete cycle | AST traversal 17.1%; AST cloning 9.3% | 33.2% |
| Prepared ordered paging | Top-K retention 33.7%; sorting 13.9%; read bookkeeping 4.8% | 23.6% |
| Prepared full select | Prepared query 60.8%; read bookkeeping 8.1% | 35.3% |
| Prepared point select | Transaction start 15.8%; commit 30.4%; pruning 7.8% | 24.9% |

The SQLx point profile's largest active leaf is `__psynch_cvsignal` (305 of
1,453 active samples). This supports investigating the scheduler boundary, but
does not show that the entire native/SQLx latency gap can be removed. Removing
all allocator cost from the update profile would cap CPU speedup near 1.48x;
allocation reduction alone cannot deliver a 5x write improvement.

### Literal inserts and cache memory

| Equal 32,832-row insertion experiment | Mean us/insert | Peak RSS MiB |
| --- | ---: | ---: |
| Native literal execute | 20.23 | 33.1 |
| Native prepared parameter | 15.81 | 33.1 |
| SQLx distinct literal query per row, persistent | 41.30 | 635.5 |
| SQLx bound parameter, persistent | 27.10 | 34.1 |
| SQLx distinct literals, nonpersistent | 39.49 | 33.8 |

The 0.5-second timing probes independently show a similar 41.10 versus 27.18 us
literal/bound SQLx difference. Binding helps preparation reuse; it does not yet
give inserts a compiled mutation kernel. Nonpersistent literal execution saves
memory but barely improves latency.

In [connection.rs](../crates/pg_fake_sqlx/src/connection.rs), the advertised
100 MiB statement-cache limit increments/decrements only `sql.len()`. It omits
owned ASTs, dependencies, plans, parameter types, duplicated keys and container
overhead. Approximately 1.5 MiB of distinct SQL in this experiment is associated
with roughly 600 MiB additional RSS. This is an association, not an exact AST
allocation count. Bounded cache accounting is needed even if speed is unchanged.

## Rules for every optimization attempt

- Measure before and after every optimization attempt, including unsuccessful
  experiments. Use identical workloads, fixtures, build settings and environment;
  run the affected diagnostics and every Tier 1 pg_fake benchmark. Keep raw
  results and report absolute times plus time reduction as
  `100 * (before - after) / before` percent.
- Do not commit an optimization unless it produces a noticeable percentage
  reduction in execution time on its targeted workload, reproducible in
  independent runs and exceeding measured noise. A tiny or inconclusive change,
  cleaner code, fewer allocations or memory savings alone is insufficient.
  Record unsuccessful attempts and discard or revise their implementation.
- No Tier 1 pg_fake benchmark may become slower. Reject any confirmed slowdown,
  however small; there is no 5% allowance and improvements elsewhere cannot
  compensate for it. Repeat measurements to resolve apparent slowdowns or noisy
  results; do not commit while a possible regression remains unresolved.
- Commit each accepted optimization separately, with its focused validation.
  If one task contains several independent optimizations, measure and commit
  each separately rather than bundling their gains or regressions.
- Include timing evidence in every optimization commit message: benchmark names,
  before/after times with units, percentage reductions, measurement settings,
  and a reference to retained results for the full Tier 1 comparison confirming
  no regressions. Summarize relevant correctness checks as well.

These rules govern all optimization implementations below, including the
memory-oriented O2 work. Research, benchmark infrastructure and plan edits do
not themselves claim an execution-time optimization.

## Implementation tasks, in recommended order

Effort ranges are engineering estimates for one developer including focused
validation, not commitments. Numerical targets are investigation/acceptance
budgets on this machine, not measured future results. Re-measure after each
task; do not add percentage improvements from overlapping paths.

### O1 — Make performance gates equivalent and reproducible [COMPLETE]

Use the existing Criterion suite and `cargo x bench` filtering, recording and
reports. Keep the ten Tier 1 comparisons; add parameterized insert/update cases,
bound literal fixture/cache growth with untimed cleanup, and use prepared native
execution in the existing adapter comparison. Record a fresh baseline before
measuring each optimization. Keep generated captures under `target/`.

Completed 2026-10-03 with only these focused benchmark changes. No additional
runner, filtering, result checks or revision-tracking layer is needed.

### O2 — Bound retained SQLx statement memory [COMPLETE]

**Effort:** 1–3 days. **Dependency:** O1. **Confidence:** high for memory reduction;
low for a large latency gain.

- Account for retained statement data, not only SQL text. Use a conservative
  owned-size estimate with a bounded entry-count backstop, and store entry cost
  so replacement, LRU eviction and cache clearing update accounting consistently.
- Include the typed key and replan state; avoid deep-cloning dependency metadata
  into every equivalent statement where immutable catalog identity permits
  sharing. Prioritize a robust bound before introducing a complex size walker.
- Preserve `.persistent(false)`, explicit statements, parameter-type keys,
  search-path re-resolution, transactional DDL invalidation and changed-result
  metadata behavior. An explicit statement retained by the caller is distinct
  from a cache-owned entry.

**Acceptance:** the configured cache policy places a defensible bound on
cache-owned memory in a 32k/100k distinct-SQL stress test; LRU/replan tests verify
accounting. RSS is diagnostic, not an assertion that allocator RSS equals the
cache limit. All Tier 1 timings must satisfy the rules above: memory reduction
alone does not qualify this optimization for commit without a noticeable time
reduction. Disabling caching or rewriting literal SQL is not the proposed default.

Completed 2026-10-03. The SQLx cache now accounts for typed keys, prepared
syntax/metadata, replan state and conservatively estimated catalog definitions,
with a 2,048-entry backstop. Catalog dependencies share immutable schemas.
The matched distinct-literal insert median improves 41.323→38.280 us (7.36%);
32k peak RSS falls 635.55→34.72 MiB, and 100k RSS is 81.16 MiB. Focused
Tier 1 repeats resolve noisy full-run update/indexed/join readings without a
confirmed slowdown. Raw timings, memory measurements, Tier 1 comparisons and
validation notes are retained locally outside version control.

### O3 — Compile ordinary inner joins and their filters [COMPLETE]

**Effort:** 4–7 days. **Dependency:** O1. **Confidence:** high that this is the
largest join opportunity; medium on the final speedup.

Relevant code: [prepared.rs](../crates/pg_fake/src/executor/prepared.rs),
[scans.rs](../crates/pg_fake/src/executor/from/scans.rs),
[joins.rs](../crates/pg_fake/src/executor/from/joins.rs),
[query/select.rs](../crates/pg_fake/src/executor/query/select.rs).

- Extend the existing enum-based bound plan with ordinary table inner joins:
  table IDs, column slots, access paths, typed equality keys, local filters and
  residual predicates. Bind names/types once and evaluate prepared parameters
  directly. Start with the two supported integer-equality benchmark shapes.
- The current prepared builder excludes ordinary table joins: `StreamedJoin`
  is admitted only for a special CTE source. Prepared ordinary joins therefore
  still reconstruct general scopes, projections and execution context.
- Implement a context-free join kernel or explicitly construct the context it
  requires. Simply relaxing the builder gate is wrong: the current prepared join
  executor calls `context.expect(...)`, while the session fast path supplies
  `None`.
- Preserve the existing hash join as the algorithm. Compile source filters and
  output expressions, avoid repeated type/name resolution, and avoid evaluating
  an already-applied immutable predicate again after the join.
- Add one-shot support by building this bound plan once per execution; it need
  not introduce a native SQL-text plan cache, which the specification excludes.

**Validation:** differential generated integer joins, duplicates, NULL keys,
empty inputs/results and parameters; preserve metadata, SQLSTATE, statement
visibility and SSI reads. Other joins/coercions remain on the general path.

**Acceptance:** both official joins must beat their fresh PostgreSQL comparison;
first budgets are selective join ≤35 us and many-match join ≤55 us through SQLx.
Require lower type/name/AST profile shares and improved scaling. These budgets
represent approximately 2.6–2.9x improvement, not a promised 10x.

Completed 2026-10-03 for two-table `integer = integer` inner joins with a
simple local equality filter and direct-column projection. The full Tier 1
SQLx selective and many-match joins improve 90.332→22.876 us and
159.060→29.459 us and beat PostgreSQL 18's 39.745/66.023 us. The separate
prepared-read plan preserves the existing non-join executor. Matched paired
probes show paging/order flat within noise, although the full Criterion run
shows 3.0–3.3% slower paging/order; the user explicitly accepted that possible
tradeoff for the join gain. Raw timing, scaling, profile and validation results
are retained locally outside version control. Other join shapes
remain on the general path.

### O4 — Reduce join intermediates and select useful access paths [PENDING]

**Effort:** 2–4 days. **Dependency:** O3. **Confidence:** high for wasted work;
medium for benefit on the smallest tables.

- The hash join currently collects both sources into owned rows, builds
  `Vec<Vec<usize>>` match lists, clones rows into `joined`, then visits them.
  Emit matches directly into projection using reusable scratch slots or borrowed
  source rows/row IDs. Keep owned values only when required for the final result.
- Prune unused payload columns before cloning. Choose an indexed probe when a
  selective left row joins to a unique right key; avoid hashing all right rows
  just because both sides happen to have indexes.
- Propagate safe same-type equality constants through inner joins only after
  compiled filters exist. Preserve remaining predicates and snapshot/SSI
  accesses. Do not propagate these rules across outer joins or volatile/error
  expressions without proving the semantics.
- Choose scan/hash/probe with small, explainable rules; defer a cost-based
  optimizer and arbitrary join reordering.

**Acceptance:** indexed selective joins should stop scaling linearly with the
unselected right table; demonstrate it at 100 and 10,000 rows. Targets after O3
are a further 10–30% where copying matters, not an additional universal multiplier.

The first O4 access-path step uses unique indexes on the filtered left column
and right join key for paired point probes. When the left filter fixes its join
key without those indexes, the hash build retains only matching right rows.
At 100 rows, matched SQLx indexed probes improve 16.726→8.853 µs (47.07%);
at 10,000 rows they improve 990.497→7.422 µs (99.25%). The unindexed SQLx
probe improves 16.689→10.230 µs (38.71%) at 100 rows. Full Tier 1 joins
improve 22.876→16.589 µs (27.48%) and 29.459→26.334 µs (10.61%).
Other O4 intermediate-copying and access-path work remains pending. Raw runs
are retained locally under `target/o4-*`. The full Tier 1 run found no confirmed
regression; exact paired O3/O4 Criterion repeats resolved noisy write readings:
insert 38.195→37.880 µs, transaction insert 42.435→41.971 µs, and update
42.717→42.394 µs (20 samples, 1-second warmup, 2-second measurement).

A terminal-stage general hash-join experiment removed match-list and joined-row
materialization but was discarded: the 100-row two-filter SQLx join measured
121.600→124.981 µs and the 10,000-row prepared case measured
8,653.049→8,750.160 µs. The accepted next step compiles an inner join with
one simple integer equality filter on each source. Matched SQLx probes improve
120.670→10.475 µs (91.32%) for 100 selective rows,
310.287→19.767 µs (93.63%) for 100 many-match rows, and
8,365.555→410.470 µs (95.09%) for 10,000 selective rows. The full Tier 1
Criterion run and matched repeats found no confirmed slowdown. Exact paired
update, indexed-select and join repeats resolved noisy full-run readings; raw
results remain under `target/o4-double-filter-*`. General join
intermediate-copying work remains pending.

The next O4 access path probes a unique right join key when the left equality
filter fixes that key, even if the left column is not unique. A fixed 100-row
left table joined to a right table with 100 or 10,000 rows improves from
10.248→8.544 µs (16.63%) and 195.096→8.803 µs (95.49%) through SQLx;
the 10,000-row native prepared probe improves 181.009→2.959 µs (98.37%).
These are medians of three matched baseline/candidate runs. The full Tier 1
Criterion comparison and exact repeats found no confirmed slowdown; the
noisy transaction-insert reading resolved to 42.601→42.662 µs with overlapping
intervals. Final exact repeats measured the selective and many-match joins at
11.748→11.681 µs and 21.290→20.812 µs; a reverse-order indexed-select repeat
measured 8.590→8.261 µs. Heap equality select was flat in exact repeats.
Raw results remain under `target/o4-asymmetric-*`. General join intermediate
copying remains pending.

An unfiltered two-table equality join now borrows visible source rows while
building its hash table and reuses a joined-row buffer during result visits.
The path retains the original left scan, right scan, then result-visit order;
filtered, frozen-source and other join shapes retain the prior executor. For
`SELECT l.name, r.name FROM l JOIN r ON l.id = r.id`, three paired 0.5-second
SQLx probes improve 94.849→67.366 µs (28.98%) at 100 rows and
6,876.073→3,946.860 µs (42.60%) at 10,000 rows. Native prepared probes
improve 71.602→46.766 µs (34.69%) and 6,412.428→3,429.883 µs (46.51%).
The full Tier 1 Criterion run improves the selective and many-match joins
16.770→16.393 µs and 26.785→26.233 µs. Its noisy update and small heap/insert
differences were checked with alternating baseline/candidate probes; no
non-join slowdown was reproducible. Raw timings and the full comparison remain
locally under `target/o4-borrowed-*`. Other O4 work remains pending.

A further O4 access path probes a unique right join key when an equality filter
on a different, nonunique left column selects few rows. It stops collecting
left rows after more than 1/32 of the right table's stored row count qualifies,
then builds the existing hash table and continues the same left scan. For
`SELECT l.name, r.name FROM l JOIN r ON l.id = r.id WHERE l.bucket = 1`, three
paired 0.5-second SQLx probes improve 16.638→8.719 µs (47.60%) at 100 rows
and 998.467→247.163 µs (75.25%) at 10,000 rows. Native prepared probes
improve 8.903→2.802 µs (68.53%) and 977.087→226.997 µs (76.77%). A dense
filter taking the hash branch is 24.264→25.033 µs (3.17% slower) at 100 rows
and 1,670.455→1,712.220 µs (2.50% slower) at 10,000 through SQLx. A separate
paired probe of the existing selective join reads 9.889→10.322 µs (4.38%
slower); the user explicitly accepted that join tradeoff for the sparse gain.
The quiet back-to-back full Tier 1 Criterion pair instead reads
17.182→16.539 µs and 27.635→26.409 µs for the official joins. Other apparent
full-run write slowdowns did not reproduce in direct paired probes; exact
update and transaction-insert Criterion repeats have overlapping confidence
intervals. Raw comparisons remain locally under `target/o4-offkey-*`; earlier
runs that overlapped compilation were discarded. Other O4 work remains pending.

A symmetric O4 access path handles a right-only equality filter when its column
has a unique index and the left join key is unique. It reverses the two sources
inside the prepared inner-join plan and remaps projected columns, so the existing
unique probe reads at most one filtered right row and probes the left key.
`SELECT l.name, r.name FROM l JOIN r ON l.id = r.id WHERE r.id = 50` improves
44.463→8.785 µs (80.24%) at 100 rows and 1,094.680→7.810 µs (99.29%) at
10,000 rows through SQLx, using medians of three 0.5-second runs. Native
prepared timings improve 30.964→1.444 µs (95.33%) and 1,077.135→1.525 µs
(99.86%). Right-only-filter joins without both unique indexes retain the
general path. The isolated full Tier 1 comparison found no confirmed
regression; exact alternating update, insert, heap-select, indexed-select and
transaction-insert repeats resolved noisy readings. Raw timings and the separate
baseline build remain locally under `target/o4-right-filter-*`. General join
intermediate-copying work remains pending.

### O5 — Prepare simple mutation structure once; specialize resumption [PENDING]

**Effort:** 4–7 days. **Dependency:** O1. **Confidence:** high for redundant work;
medium for target latency.

Relevant code: [writes](../crates/pg_fake/src/executor/writes/),
[locks/mod.rs](../crates/pg_fake/src/executor/locks/mod.rs),
[expressions/resume.rs](../crates/pg_fake/src/executor/expressions/resume.rs),
[session/mod.rs](../crates/pg_fake/src/session/mod.rs).

- `build_prepared_query_plan` only returns plans for queries. Prepared INSERT
  and UPDATE avoid parsing, but still bind assignments/targets, clone schemas,
  build validation state, construct resume keys and traverse ASTs on execution.
- Add bound INSERT VALUES and single-table UPDATE plans with typed input slots,
  assignment/coercion plans, unique access, constraint expressions, and immutable
  feature flags. Compile static CHECK/default structure while evaluating runtime
  defaults and constraints at their PostgreSQL-required times.
- Add a lean execution route for eligible statements without triggers, upserts,
  foreign keys, subqueries, advisory functions or RETURNING. Keep constraints,
  transaction visibility, lock modes and unique-conflict waits fully enforced.
  Unsupported shapes retain the current general machinery.
- Avoid unconditional INSERT AST cloning and expression cursor creation for
  operations proven not to require expression-level resumption. Audit repeated
  unique validation and target/update clones before removing them; some checks
  are necessary after blocking and snapshot refresh.
- Prefer stable per-statement occurrence IDs to repeated `to_string()` keys.
  Lazily create fallback caches. Existing SessionSettings maps already share
  through Arc; this is not a proposal to eliminate a full-map copy on every call.

**Validation:** literal and bound writes, NULL/CHECK/unique violations,
overflow/coercion SQLSTATE, multirow statement atomicity, savepoints/rollback,
Read Committed rechecks, Repeatable Read conflicts, deadlocks and SSI.
Verify sequences and volatile functions execute once across waits on fallback
paths. Eligibility must be revalidated after relevant transactional DDL.

**Acceptance:** first budgets are native prepared constrained insert/update
≤8–12 us and official SQLx literal insert/update ≤25/22 us. Report cache misses
separately. If only allocations improve, expect tens of percent rather than 5x.

### O6 — Move native rows into the SQLx adapter [PENDING]

**Effort:** 1–2 days. **Dependency:** O1. **Confidence:** high for eliminating an
allocation; medium for latency effect.

In [connection.rs](../crates/pg_fake_sqlx/src/connection.rs) `map_results`
rebuilds each `Vec<Value>` as `Vec<PgFakeValue>`, repeats type information per
cell, creates an intermediate result vector, and then collects stream output.
[row.rs](../crates/pg_fake_sqlx/src/row.rs) already represents a borrowed value
as a native value reference plus type info.

- Store owned native row vectors in PgFakeRow; resolve type info from shared
  columns in `try_get_raw`. Preserve PgFakeValue for explicitly owned values.
- Transfer row vectors directly, reserve output capacity, and avoid unnecessary
  intermediate `Either` collections where SQLx's required ordering permits.
- Share immutable prepared column metadata where identity/invalidation permits;
  preserve declared types and typmods for NULL and empty results.

**Acceptance:** typed SQLx round trips and ownership tests pass, and an allocation
trace confirms the row-vector replacement is removed. Initial budget: 10–25%
improvement on 100-row SQLx results; point reads may barely change.

The first O6 step transfers native row vectors into `PgFakeRow`, reads type
information from shared columns, and reserves the combined result vector without
per-statement intermediate vectors. An alternating 100-row SQLx allocation probe
measured 424→322 allocations per query in both pairs. Alternating release probes
improve 100-row and 10,000-row SQLx reads by 17.76% and 24.50%; the cached
point read is flat. The full Tier 1 comparison improves the 100-row select
27.559→22.951 µs, ordering 39.993→33.548 µs, and many-match join
26.210→22.869 µs. The small indexed-select difference reversed direction in
exact repeats. Adapter tests pass apart from a PostgreSQL corpus threshold that
also fails on clean HEAD (820/850), and the workspace gate has the same
pre-existing mixed-case collation failure on clean HEAD. Raw runs remain under
`target/o6-*`. Sharing prepared column metadata across executions remains pending.

### O7 — Reduce per-statement SQLx scheduling cost safely [PENDING]

**Effort:** 3–6 days for a feasibility experiment and validated implementation.
**Dependency:** O1, O6; re-measure after O3/O5. **Confidence:** medium.

Every `run` and `run_control` currently submits a fresh `spawn_blocking` task.
This is necessary for real row-lock waits but costly for tiny cached reads and
basic transactions. Cheap cache-key borrowing and avoiding redundant SQL/type
vector cloning are low-risk first steps; they alone will not remove handoffs.

- Prototype a bounded nonblocking attempt for eligible prepared reads. It must
  use `try_lock` for every potentially contended internal mutex, handle relation
  lock waits, and bound scan/result work so a large query cannot occupy a Tokio
  worker indefinitely. Fall back to blocking execution before observable work,
  or continue from an explicit state machine; never replay side effects.
- Preserve pending rollback ordering, cancellation/drop behavior, session
  serialization and public SQLx behavior. Test a blocked relation/row lock and
  a busy database mutex on a single-worker runtime to prove other tasks progress.
- If these semantics make the read attempt too complex, measure a persistent
  connection worker instead. It may reduce task allocation but still pays
  wakeups; do not assume it wins or ship it without data.

**Acceptance:** demonstrate a meaningful reduction on matched cached point
queries, with a provisional SQLx budget ≤4 us. Read/write concurrency and lock
tests must pass. Keep the current scheduler if the experiment does not improve
latency or requires broad concurrency redesign. Extend to control statements
only after equivalent guarantees are established.

### O8 — Remove read bookkeeping that has no effect at this isolation [PENDING]

**Effort:** 1–3 days. **Dependency:** O1. **Confidence:** high for unnecessary
bookkeeping; medium for its end-to-end significance.

[DatabaseState::record_read](../crates/pg_fake/src/database/state.rs) locks the
SSI graph on every visited row. `DependencyGraph::read` then discards the read
unless that reader is serializable. Point lookup also constructs owned access
keys before they can be discarded. Source profiles attribute 8.1% of full-read
samples to this machinery.

- Decide reader tracking once per statement under the existing synchronization
  contract, then avoid constructing/locking read accesses for nonserializable
  readers. Continue tracking writes from nonserializable transactions whenever
  they can affect active serializable readers.
- Profile remaining autocommit start/commit overhead before changes: point-read
  query execution is only about 19% of its native samples. Reduce unchanged
  settings snapshots, metadata duplication and redundant empty-GC work only
  while preserving snapshot horizons and catalog/lock lifecycle.

**Acceptance:** SSI differential histories and snapshot/locking regressions
pass. Expect roughly 5–15% native scan improvement from read bookkeeping;
absolute savings on a 1.3-us point read will be small. Do not bypass MVCC or
stop tracking conflicting writes to meet a target.

The first O8 step remembers whether the current statement's transaction needs
SSI read tracking, skipping graph locks for ordinary reads. If a blocked
statement resumes after another transaction replaced that hint, it consults the
graph. Prepared point and join probes also avoid constructing unique read keys
when tracking is unnecessary. Three alternating 0.5-second native prepared
probes improve the 10,000-row full read 932.641→890.690 µs (4.50%), the
100-row selective join 3.816→2.754 µs (27.83%), and the many-match join
7.106→6.053 µs (14.81%); the insert cycle is flat. Both full ten-case Tier 1
Criterion sweeps improve the selective join by 11.66% and 7.57%, and the
many-match join by 5.69% and 7.72%. An apparent indexed-select slowdown
reversed in exact alternating repeats; no Tier 1 slowdown was confirmed.
The focused hint-interleaving test, four serializable differential tests, and
all 11 extended property cases pass. The workspace gate still reaches the
pre-existing mixed-case `min`/`max` collation failure, and strict Clippy still
stops on four existing warnings; Clippy passes with those four lint categories
allowed. Raw runs are under `target/o8-read-tracking-*`. The saved point-read
profile attributes 15.8% of active samples to transaction start, 30.39% to
commit, and 7.79% to GC, so the remaining start/commit work stays pending.

An O8 follow-up tried skipping version pruning when its table and catalog
bookkeeping showed no pending work. Alternating 0.5-second native probes
improved prepared point reads from 1.221 to 1.116 µs (8.60%) in the revised
read-only variant, with large scans effectively flat. The first broader
variant slowed the official transaction insert from 41.490 to 44.901 µs in
the first full sweep and from 42.068 to 43.440 µs in reverse. Restricting
the shortcut to read-only commits did not resolve the concern: exact
alternating transaction-insert repeats gave 41.929/41.948 µs for baseline
and 42.381/42.996 µs for the candidate. The full-suite many-match readings
also varied substantially, although isolated repeats favored the candidate.
Both implementations were discarded under the
no-Tier-1-slowdown rule. Raw probes, Criterion logs and the rejected patch are
retained locally under `target/o8-prune-*`; start/commit work remains pending.

Another O8 experiment shared the time zone, search path, and search-path text
across `SessionSettings` snapshots. Three alternating 0.5-second native probes
improved the prepared point read from 1.216 to 1.032 µs (15.10%), but exact
official transaction-insert repeats favored baseline at 42.104/41.742 µs
versus 43.094/42.834 µs. Sharing only the search-path vector still improved
the native point read from 1.220 to 1.099 µs (9.87%), while the exact SQLx
transaction insert measured 41.981→43.081 µs (2.62% slower). Both variants
had full Tier 1 sweeps, but were discarded under the no-slowdown rule. Raw
probes, Criterion logs, and rejected patches remain under
`target/o8-shared-settings-*` and `target/o8-search-path-share-*`.

### O9 — Reduce ordered-row copying and comparator overhead [PENDING]

**Effort:** 2–4 days. **Dependency:** O1, O8. **Confidence:** high for extra copies;
medium for speedup.

Prepared paging already uses a bounded heap with `K = LIMIT + OFFSET`, and
prepared ordering already defers projection. Adding Top-K from scratch is not
an opportunity. However, `execute_prepared_query` calls `row.to_vec()` before
heap admission, stores all source columns, then projects retained rows before
discarding OFFSET rows. Full ordering similarly clones source rows and projects
them into a second result representation.

- Retain row handles/borrowed rows or compact order keys plus necessary values;
  clone text/blob payload only when selected for output. Project after OFFSET
  where supported expression evaluation semantics permit it.
- Bind comparator data types once and benchmark typed key comparisons. Keep
  stable tie behavior, NULL placement, direction, float/NaN semantics and the
  existing collation contract. Test volatile/error expressions on excluded rows
  before changing when any expression executes.
- Benchmark a small-N collect/sort route versus a heap when K approaches N;
  use an evidence-based threshold. Ordered index traversal is later work,
  because the current indexes are not an already-available ordered-query path.

**Acceptance:** prepared_ordering_differential plus generated ordering/paging
cases pass. Initial official SQLx budgets: paging ≤22 us, ordering ≤28 us;
also demonstrate reductions on wide 10k-row inputs. Heap work and final sorting
are real costs, so copying alone cannot make paging constant time.

The first O9 step compares each borrowed source row with the bounded heap's
worst row before cloning it; the shared heap helper avoids repeating the
admission comparison. Three alternating 0.5-second native pairs improve
10,000-row paging 1,623.556→1,473.997 us (9.21%); the 100-row median improves
17.332→16.767 us (3.26%) for the initial candidate. A final exact 20-sample,
1-second warmup, 2-second measurement SQLx paging pair improves
28.148→26.856 us (4.59%). A temporary 10,000-row fixture with 1 KiB text
payloads improves from 2,084/2,306 us to 1,799/1,942 us in alternating
512-iteration runs of the final candidate (14.77% by pair medians).
The full Tier 1 sweeps under `target/o9-lazy-topk-tier1-*` changed direction
with run order, and one sweep had a transient 2–4x rise across unrelated
cases. The final full sweep's apparent many-match join slowdown reversed in
exact alternating repeats (16.759→16.887 us, then 17.047→16.713 us); no Tier 1
slowdown was confirmed. The focused prepared ordering test, PostgreSQL
differential, 256 generated SQL cases, workspace library tests, formatting,
and scoped Clippy pass. Full workspace tests exhausted disk during linking;
strict Clippy still stops on three unrelated existing warnings. O9 stays
pending because the official 22 us paging budget and the remaining comparator
and OFFSET work are not yet met.

A direct comparator experiment improved isolated 100-row SQLx full ordering
28.070→27.124 us (3.37%) and 28.302→27.442 us (3.04%), but its large-row
paging readings were inconsistent and sometimes slower. Restricting the direct
comparator to full ordering improved exact SQLx ordering 28.293→27.053 us
(4.38%) and 27.876→27.576 us (1.08%). Both full Tier 1 sweeps and an isolated
repeat then found confirmed slowdowns outside ordering: selective read
10.862→11.069 us (1.91%), selective join 9.966→10.289 us (3.24%), and
many-match join 17.302→17.511 us (1.21%). Both variants were discarded under
the no-slowdown rule. Raw logs and
the rejected patch remain under `target/o9-direct-compare-*` and
`target/o9-direct-sort-*`.

An OFFSET projection experiment skipped projection of discarded rows only when
every output was a direct column, retaining the original evaluation order for
expression projections. Exact 20-sample SQLx paging pairs improved
26.624→22.793 us (14.39%) and 26.624→23.849 us (10.42%). A 10,000-row native
prepared probe improved 1,385.266→1,349.818 us (2.56%) and
1,456.471→1,374.676 us (5.62%); longer 1-KiB-text-row probes improved
2,007.841→1,922.145 us (4.27%) and 1,984.177→1,870.159 us (5.75%).
The ordering differential, generated SQL property
test, workspace library tests, formatting, and scoped Clippy passed. However,
exact SQLx full-read repeats confirmed a Tier 1 slowdown in both run orders:
22.460→23.482 us (4.55%) and 19.992→20.476 us (2.42%). The candidate was
discarded under the no-slowdown rule. Raw results and the rejected patch remain
locally under `target/o9-offset-projection-*`; O9's paging and ordering budgets
remain pending.

A small-table collect/sort experiment replaced the bounded heap when
`LIMIT + OFFSET` covered at least half of at most 256 stored table rows. The
revised variant reused the existing collect path and truncated after sorting;
the 10,000-row path kept its heap. Two exact, profile-matched 20-sample SQLx
paging pairs improved 26.342→19.764 us (24.97%) and 26.529→19.747 us
(25.56%). The full Tier 1 sweep improved paging 29.335→22.330 us (23.88%).
The prepared-ordering differential and generated SQL property test passed.
However, exact selective-join repeats in both run orders confirmed a slowdown:
10.099→10.258 us (1.57%) and 10.084→10.298 us (2.12%). Both prototype
variants were discarded under the no-slowdown rule. Full Tier 1 results,
isolated probes, build logs, and rejected patches remain locally under
`target/o9-collect-sort-*`.

## Milestones and stopping rules

1. Establish trustworthy gates and bound cache retention: O1–O2.
2. Make both Tier 1 joins decisively faster than PostgreSQL: O3–O4.
3. Improve writes and remove adapter row copies: O5–O6.
4. Measure scheduler feasibility and remaining read/ordering cost: O7–O9.

For each task, run focused differential/property/concurrency tests for the
affected semantics, formatting and strict Clippy. Before accepting an engine
change, run the workspace regression gate and remeasure all Tier 1 groups;
exercise affected Tier 2/3 fallback paths as well. Use the existing extended
property gate for broad executor, transaction or scheduling changes. Apply the
timing and separate-commit rules above to every optimization attempt; accept no
Tier 1 slowdown. Update the official recorded baseline only after a change has
been accepted.

Several targets overlap. An overall 2–4x improvement in the currently slow
Tier 1 operations is a reasonable investigation objective; a universal 10–100x
SQLx advantage is not established by this evidence. Fast native prepared point
reads are already near a practical small-operation floor. A final milestone
should require a fresh PostgreSQL comparison and report absolute latency,
memory and fidelity results, rather than claiming success from aggregate ratios.

## Deferred ideas

- **New hash join algorithm:** hash joins already exist, and their hashing is
  not the leading observed cost. Compile filters and reduce materialization first.
- **Persistent storage maps / full Value redesign / fine-grained locks:** these
  are larger changes without sufficient Tier 1 evidence. Profile their remaining
  cost after the localized work; retain deterministic iteration and MVCC.
- **Transaction-status pruning as the first project:** the saved diagnostic
  point read is essentially flat from 1 to 100,000 completed transactions.
  History memory can matter, but that report does not support a Tier 1 latency
  priority ahead of joins, writes and SQLx.
- **Broad parse cache or literal SQL normalization in the core:** native explicit
  preparation already exists; the specification deliberately excludes a core
  SQL-text cache. SQLx cache retention/reuse can be fixed in its existing layer.
- **Disable locking, constraints, GC or SSI:** none is an acceptable shortcut.
  Eliminate irrelevant work using proven statement/isolation conditions.
- **Tier 2/3 regex, temporal, windows and recursive-query tuning:** some saved
  cases are slow, but they do not outrank the requested Tier 1 work.

## Reproducing the research

From the repository root:

```sh
cargo bench --offline -p pg_fake_benchmarks --bench workloads -- tier1_ --sample-size 20 --warm-up-time 1 --measurement-time 2 --noplot
CARGO_PROFILE_RELEASE_DEBUG=true cargo build --offline --release -p pg_fake_benchmarks --example research_tier1
python3 scripts/research-tier1.py
python3 scripts/research-tier1.py --profiles
python3 scripts/research-tier1.py --summarize
python3 scripts/research-tier1.py --memory
```

The comparison uses the configured local PostgreSQL instance and manages only
its reserved `pgfake_benchmark` schema. Profiles/memory metrics require macOS
process access; profiles require FlameGraph tools and Perl. Generated flame
graphs and raw reports live in `target/tier1-research`. Local evidence includes
the collected raw samples and folded stacks. The fixed-count
memory option was added after timing/profile collection. Review subsequently
changed the untimed preflight to validate the selected API rather than always
validating native `execute` / cached SQLx. Neither change alters fixture SQL or
timed database operations. All selected routes were subsequently checked again.
Each evidence environment records the exact binary/source fingerprints used.

Research validation passed the probe build and all 49 timing configurations,
eight profile collections, five memory configurations, Python syntax,
rustfmt, evidence-count/archive checks, local document links and scoped
Clippy (`--no-deps`, `-D warnings`). Full dependency linting remains blocked by
four existing engine warnings in `executor/mod.rs:33`,
`executor/prepared.rs:1006`, `executor/query/select.rs:577` and
`executor/query/windows.rs:336`. Production code was left unchanged. These
checks validate the research artifacts; they are not a new full conformance run.

Single probes can be run directly:

```sh
target/release/examples/research_tier1 join prepared 100 5
target/release/examples/research_tier1 join_indexed prepared 10000 5
target/release/examples/research_tier1 point sqlx 100 5
PG_FAKE_RESEARCH_ITERATIONS=32768 target/release/examples/research_tier1 insert_growing sqlx_bound 100 1
```

No new SQL features are proposed or claimed as implemented. Representative
already-supported workloads targeted by this plan include:

```sql
INSERT INTO items VALUES ($1, 'benchmark');
UPDATE items SET amount = amount + 1 WHERE id = $1;
SELECT id, name FROM items WHERE id = $1;
SELECT id, name FROM items ORDER BY id DESC LIMIT 10 OFFSET 40;
SELECT l.id FROM left_items l INNER JOIN right_items r ON l.id = r.id WHERE l.id = 50;
SELECT l.id FROM left_items l INNER JOIN right_items r ON l.bucket = r.bucket WHERE l.bucket = 0;
```
