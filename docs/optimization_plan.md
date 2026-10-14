# Optimization experiments

Status: research completed; E01 accepted, E02–E16 pending. This is separate
from the SQL compatibility roadmap in `plan.md`.

Research revision: `2b5243fc043adef6d713de71b2f9bc03dd57074a`.
The [research record](optimization_research/2026-10-12/README.md) contains fresh
measurements, profiling evidence, probe sources, reproduction instructions, and
the assessment of the application-agent handoff. Read it before starting.

## Rules for every experiment

- One experiment tests one optimization idea. Add its necessary benchmark cases
  and correctness coverage with that change; do not make a separate speculative
  infrastructure optimization or combine unrelated improvements to pass the gate.
- Prefer algorithmic improvements and removing repeated work. Prefer simpler
  code. If implementation accumulates specialized `if` branches, look for a
  general representation or algorithm that handles the cases together. Extend
  the existing access, expression, or equality machinery before adding another
  executor path. Never recognize application table names, SQL strings, particular
  parameter values, or benchmark sizes.
- A semantic fallback is acceptable when the general algorithm cannot safely
  handle a case. Document its invariant and test both paths. Do not mistake
  query-specific shortcuts for unavoidable semantic boundaries.
- Each experiment ends as **accepted**, **rejected**, or **inconclusive**. A
  plausible profile, reduced instruction count, or faster helper alone is not a
  successful experiment. Re-profile after each accepted change: later costs and
  priorities may change, and projected savings are not additive.
- Do not add a global cache. Keep the specification's explicit prepare model,
  single storage monitor, deterministic scans, transactional semantics, and
  PostgreSQL fidelity. Preserve stack-depth protection, timeouts, cancellation,
  and exactly-once volatile evaluation across waits.

### Acceptance gate agreed with the user

For the **same operation**, let `B` be the parent revision's mean latency and
`A` the candidate's. Record `B - A` in microseconds and `100 * (B - A) / B`.

1. Predeclare a primary workload and representative sizes before changing code.
   Accept only if it is **at least 10% faster AND saves at least 1.0 µs per
   operation**. Throughput elements are not operations: a 100-row query is one
   query, not 100 independent successes. A 0.5 µs lookup cannot qualify by itself.
2. Establish the improvement in at least three independently warmed parent /
   candidate pairs, alternating their order. Use Criterion confidence intervals
   and raw samples; require a 95% confidence interval excluding no improvement
   as well as both practical thresholds in each pair. Borderline or conflicting
   measurements are inconclusive; improve the measurement rather than relaxing
   the thresholds or choosing the most favorable run.
3. **Any repeatable slowdown above 5% on another supported workload blocks
   acceptance**, even if the absolute regression is below 1 µs. The 1 µs floor
   applies to gains, not permission to regress fast operations. Inspect individual
   cases; an aggregate/geometric mean must not hide a regression.
4. Run all Tier 1 controls, directly affected Tier 2/3 cases, and the small-input
   and fallback cases listed below against both revisions. Before acceptance run
   the full pg_fake benchmark suite, including native diagnostics and concurrency. Repeat
   suspected regressions in three paired runs. A partial suite is a screening
   result, not a final acceptance gate.
5. Correctness checks must pass: relevant property/differential tests against
   PostgreSQL 18, relevant concurrency schedules, then workspace checks. Compare
   actual results/metadata/errors, including empty results, not just row counts.
   Profiling counters and experimental bypasses must not ship accidentally.
6. Check memory and displaced work. Include preparation for cold-use workloads,
   commit/rollback and reclamation for storage changes, and fork creation **plus
   first writes and disposal** for snapshot changes. A gain caused by an
   unbounded cache or by moving cost outside the timer is rejected.

### Measurement and commit procedure

Use release/bench builds, one benchmark process at a time, a quiet machine, the
same Rust/dependency versions, and identical fixtures and parameters. Record CPU,
OS, build flags, revision, timer boundary, warmup, sample count, run order, result
cardinality and fixture size. Keep setup out of steady-state timings, but retain
a separate cold prepare/execute benchmark. Vary keys across warm executions.
Keep mutation fixtures bounded and include explicit commit and rollback cases.

Use fresh parent baselines, never the October 9 committed results as the sole
before measurement. The research's short runs are prioritization evidence, not
acceptance baselines. Start acceptance runs with at least 100 samples, 3 seconds
warmup and 5 seconds measurement; inspect workload-local overrides because some
existing groups force 20 samples / 2 seconds. Lengthen genuinely noisy runs.
Profile separately from timing, after setup and warmup. Compare work counts
(rows visited, candidates fetched, AST walks, frame inputs) when sampling cannot
separate competing explanations. Keep counters local to the diagnostic build.

Example Criterion workflow using the fake-only runner in each revision:

```sh
python3 docs/optimization_research/2026-10-12/prepare.py --full-fake --sqlx
target/release/research-workloads --bench --noplot --sample-size 100 --warm-up-time 3 --measurement-time 5 FILTER --save-baseline exp01-parent
```

Run the corresponding binary from the candidate revision with the same settings.
Use unique baseline names per pair and preserve raw results outside directories
that a subsequent run overwrites. Build both revisions before timing; do not run
compilation or profilers alongside a timed run. `cargo x bench` restores the
repository baseline, so it is not a substitute for this paired comparison.

The regular harness runs PostgreSQL benchmarks even for a pg_fake-only filter and
drops its reserved `pgfake_benchmark` schema. Use the research's isolated fake-only
runner for all performance measurements, including the full suite. PostgreSQL 18
remains the reference for differential correctness checks. Do not overwrite
committed results during an experiment.

Record the hypothesis, exact commands, timing summary, correctness results,
memory observations, and acceptance decision in an experiment record beside this
plan. Keep raw samples, profiles, logs, and captured diffs in ignored local storage.
On failure, remove only that experiment's engine changes
and retain the negative result; do not commit a failed optimization. Do not reset
unrelated user changes. On success, request subagent review, resolve findings,
and repeat review until clean, as required by `AGENTS.md`. Commit only after all
gates pass. Keep generated benchmark results, logs, profiles, and archives in
ignored local storage such as `target/optimization-research/results/`; do not
commit them or run `cargo x bench record` to update tracked results. Commit the
experiment summary and required timings in the commit message, together with
source changes and relevant tests.

Commit message template (replace every placeholder with measured facts):

```text
perf(executor): <optimization idea>

<Why the algorithm does less work and which semantics it preserves.>
<benchmark, size, API>: <before> us -> <after> us
  <percent>% faster; <absolute> us saved; <confidence / paired-run summary>.
Controls: <largest repeatable slowdown and affected workload>; full suite <result>.
Validation: <differential/property/concurrency checks>.
Evidence: <experiment record>; parent <revision>; <CPU, build profile>.
```

These experiments optimize already-supported SQL. If an implementation also
adds SQL support, split that scope where practical and include query examples in
its commit message as required by the repository instructions.

## Ordered experiments

Each numbered item is one code experiment, including its own measurements and
decision. Dependencies refer to reusable mechanisms, not a requirement to keep
an unsuccessful earlier change. If a prerequisite is rejected, revise the
dependent design and record that decision before implementing it.

### E01 — Extract indexed mutation candidates from conjunctions [ACCEPTED]

Decision and measurements: [E01 experiment record](optimization_e01.md).

**Idea:** share a predicate-constraint extractor between scans, mutation target
selection, and lock discovery. Find usable equalities inside nested `AND`, use
them to fetch candidates, and preserve the full residual predicate and its
concurrent-update recheck. This changes candidate discovery from a table scan to
an indexed lookup when only one key can match.

**Evidence/source:** the handoff reproduction contrasts bare-key and guarded
UPDATE/DELETE. `executor/from/scans.rs::resolve_unique_point_lookup` accepts only
a top-level equality; its callers include `executor/locks/mod.rs` and
`executor/writes/targets.rs`. The prepared SELECT access finder already handles
more conjunctions: consolidate the interpretation rather than creating another.
The guarded-write profile places 97.9% of retained samples under row-lock
acquisition, confirming candidate discovery as the first target.

**Primary:** handoff guarded UPDATE and DELETE at 100/1,000/10,000 rows with one
matching row. Controls: bare-key writes, equivalent SELECT, missing key, false
guard, and SQLx indexed writes. Compare timed mutation alone and full transaction
latency; profile lock discovery separately from execution. High potential if the
candidate scan disappears; no claim that it removes all mutation overhead.

**Correctness/rejection:** permute/nest conjuncts; contradictory equalities, NULL,
casts, parameters and OR; preserve errors/volatile evaluation, RETURNING, affected
counts, SSI read tracking, RC rechecks, RR conflicts, waits and rollback. A failed
guard modifies nothing. Retain a correct fallback when extraction is unsafe.

### E02 — Execute selective outer joins through parameterized index probes [PENDING]

**Idea:** extend the existing join access representation to bind a filtered
preserved-side input and probe indexed matches on the other side. Carry join
preservation and residual ON/WHERE conditions in the same plan representation as
inner joins. Target indexed lookup plus matching rows, instead of building a
whole-table join for one selected account. Reuse E01's constraints where useful.

**Evidence/source:** handoff `left_roles` grows 37.07 -> 1,189.53 µs from 10 to
1,000 accounts for one output row; the account-only control stays about 1.5 µs.
`executor/from/mod.rs::visit_query_source_rows` removes selection pushdown for
any outer join. Inspect `from/joins.rs`, `prepared.rs`, `prepared_joins.rs`.
The profile already runs through the hash-join chain (96.6% inclusive); WHERE
evaluation accounts for 52.6%. The gap is selective access, not missing hashing.

**Primary:** both handoff LEFT JOIN shapes, varying keys at 10/100/500/1,000/10,000
rows. Add zero/one/many right matches and absent preserved rows. Controls: all
existing inner joins, full-output LEFT JOIN, unindexed joins and tiny inputs.
Count scans/probes to prove the operation no longer visits unrelated rows.

**Correctness/rejection:** keep null extension and multiplicity. Only move
predicates proven safe for the preserved side; right-side `IS NULL`, mixed-side
predicates, ON filters, RIGHT/FULL joins and OR need explicit semantic treatment.
Reject unconditional outer-join pushdown. Test nullable keys, duplicate matches,
ordering ties, old snapshots and locking reads; do not weaken inner-join plans.

### E03 — Use complete composite equality keys through one access descriptor [PENDING]

**Idea:** replace single-column execution assumptions with ordered vectors of
bound equality keys, usable by reads, joins and mutation candidates. Storage
already represents composite keys; bind constraints to index-column order once.

**Evidence/source:** the handoff's complete `(group_id, account_id)` lookup falls
back despite indexed storage. `prepared.rs::find_index_access` requires one
column; `storage.rs` has exact-column-vector lookups. Partial predicates are a
separate eligibility issue, addressed in E05.

**Primary:** add an **unfiltered composite-only** control to the handoff complete
key query and benchmark 100/1,000/10,000 rows. Also measure parameterized joins
using complete keys. Keep original partial-index and diagnostic-single-index
variants to distinguish the causes. Controls: single-column lookups and writes.

**Correctness/rejection:** column/conjunct order, mixed key types, NULL semantics,
casts, duplicate nonunique keys, changed keys and old versions. Use existing
equality normalization and MVCC rechecks. Do not call a composite key unique when
only some components are bound. Reject per-key-width specialized executors.

### E04 — Enumerate composite-index equality prefixes without scanning all keys [PENDING]

**Idea:** add a bounded leading-key range to E03's access descriptor and index
iteration, using the existing ordered key map. Return all matching RowIds and
recheck visible values; deduplicate stale-version index candidates. A partial
unique key is a multirow access path, not a unique lookup.

**Evidence/source:** the handoff selects by account from `(account_id, group_id)`
and by group from `(group_id, account_id)`. Current exact-column index eligibility
cannot use these prefixes. First measure nonpartial composite controls to isolate
prefix access; combine with E05 only after separate acceptance.

**Primary:** prefix reads and membership join chains at fixed match counts as
unrelated rows grow. Compare direct single-column indexes, complete-key E03,
10/100/1,000 matching prefixes, and missing prefixes. Controls: index maintenance,
full scans and deterministic ordering. Require counters showing bounded key
enumeration; filtering every index key is not an algorithmic improvement.

**Correctness/rejection:** do not infer SQL range ordering from equality-key
normalization. Preserve RowId ordering where needed, ORDER BY, multiple versions,
NULLs, multiple matches and SSI predicate coverage. Writes and rollbacks must
maintain the same physical index rather than a second redundant prefix index.

### E05 — Prove partial-index eligibility from bound predicates [PENDING]

**Idea:** make access-path selection prove that query constraints imply an index
predicate, then use the common access descriptor. Start with a principled
conjunction/typed-expression implication rule that covers `deleted_at IS NULL`;
unknown implication means fallback. Reuse E03/E04 for composite accesses.

**Evidence/source:** the handoff original indexes are partial and composite.
Adding diagnostic plain indexes changes both scanned rows and prepared-plan
eligibility; that control is evidence, not the proposed solution.
`storage.rs::{has_unique_index,has_nonunique_index}` and prepared access selection
currently require `predicate.is_none()`.

**Primary:** all original handoff membership queries without diagnostic indexes.
Measure predicate-only single-column, nonpartial composite, and original partial
composite variants separately. Require the original schema to improve and track
whether the prepared plan is selected. Controls: queries including deleted rows,
unfiltered reads, inserts, updates and soft deletes.

**Correctness/rejection:** no implication from an arbitrary OR or nullable
comparison. Check reordered conjuncts, parameter-dependent predicates, cached
plans with changed values, DDL/invalidation, savepoints and old snapshots. An
index candidate's visible version must satisfy the predicate; historical entries
cannot substitute for that check. Do not assume unique prefixes or weaken SSI.

### E06 — Compute structural expression properties once [PENDING]

**Idea:** compute reusable expression/statement properties in one traversal:
subqueries, advisory calls, volatility, views and other required analysis flags.
Use them instead of repeatedly walking unchanged AST subtrees for each row and
each nested expression. Share the metadata across prepared and ordinary execution
at their appropriate lifetimes, with stable occurrence identity.

**Evidence/source:** the 1,000-row VALUES UNION ALL profile spends 51.4% of retained
samples under `stacker::remaining_stack`; repeated read visitors dominate.
Pattern-query read expression visits cover 32.6%, while regex matching is only
4.4%. Inspect `subqueries.rs::contains_subquery`, `advisory.rs`, query feature
discovery, view expansion and `expressions/resume.rs`.

**Primary:** VALUES UNION ALL at 100/1,000 rows and patterns at 100/1,000 rows.
Controls: cold prepare, simple indexed reads, DML, scalar subqueries, nested views,
recursive CTEs and deep/wide ASTs. Count traversed nodes before/after. High
potential; optimizing only SQL formatting has a much smaller measured ceiling.

**Correctness/rejection:** recompute metadata for rewritten/bound ASTs as needed;
source spans alone do not identify repeated correlated/CTE invocations. Preserve
lazy errors, volatility and wait resumptions. Do not disable recursive stack
protection or cache flags by SQL string. Avoid a new independently traversing
cache for every expression feature.

### E07 — Borrow source rows until ownership is required [PENDING]

**Idea:** give general query consumers a borrowed row view and materialize only
outputs or rows that must survive a wait, source mutation, grouping or window
retention. Reuse the existing borrowed scan/join approach rather than adding a
wide-row or aggregate-specific path.

**Evidence/source:** `from/scans.rs::visit_table_factor_rows` clones complete rows
into a scratch row. With the same keys, replacing unused payloads with NULL lowers
the 1,000-row grouped query from 1,058.62 to 853.94 µs and ranking from 2,785.49 to
2,409.78 µs; prepared scans remain about 44 µs. These are diagnostic fixture
contrasts, not an optimization result. Profiles confirm Value/JSONB cloning.

**Primary:** grouped queries and filtered scans over narrow and wide rows at
100/1,000 rows, including large unused text/JSON/arrays. Controls: scan/projection
that actually returns the payload, joins, all aggregates, windows and CTEs.

**Correctness/rejection:** no references may outlive the locked/source snapshot;
retain owned values across lock waits and mutations. Preserve joined column slots,
row provenance, SSI reads, group representative rows and projection order. Reject
a design that copies everything at a later boundary or slows tiny queries >5%.

### E08 — Replace pairwise duplicate/membership searches with SQL equality keys [PENDING]

**Idea:** use one typed SQL not-distinct key/membership mechanism for DISTINCT and
set operations, retaining first-seen output order and multiplicity counts for ALL.
Replace O(n*d) / O(n*m) pairwise comparison with expected O(n+m) lookup. Reuse
`executor/equality.rs`; retain a documented fallback for types whose equivalent
key cannot be proven yet, rather than duplicating equality semantics.

**Evidence/source:** 1,000 distinct integers take 1,912.97 µs; ten distinct values
take 367.62 µs. `query/distinct.rs::remove_duplicate_rows` accounts for 60.3% of
its profile. `set_operations.rs` also scans selected/right rows pairwise.
Grouping already switches to hashing at 128 groups; do not propose it as new.

**Primary:** high-cardinality DISTINCT and table UNION at 100/1,000/10,000 rows;
add INTERSECT/EXCEPT, ALL, and aggregate DISTINCT diagnostics. Controls: ten-key
and all-equal inputs, empty/tiny results, UNION ALL, recursive UNION and DISTINCT
ON. Evaluate gains separately per operator; do not claim a win for unmeasured ones.

**Correctness/rejection:** NULL-equality differs from joins; preserve cross-type
coercion, bpchar behavior, numeric normalization, NaN/signed zero, arrays/JSONB,
collisions, first-row selection after ordering, and duplicate counts. The helper
must not expose HashMap iteration order as row order. Reject an always-hash design
if small-input regressions cannot be solved simply.

### E09 — Maintain window aggregate state across overlapping frames [PENDING]

**Idea:** advance aggregate state with monotone frame boundaries rather than
rebuilding it for every output row. First support append-only cumulative frames
and aggregates with a sound transition model; use the same state interface for
removable/sliding frames where semantics permit. Keep a correct recomputation
fallback for noninvertible cases.

**Evidence/source:** cumulative integer SUM rises 710.15 -> 56,765.90 µs from
100 to 1,000 rows. `query/windows.rs::calculate_window_values` occupies 95.9% of
its profile; aggregate input preparation occupies 64.6%. Current code constructs
a new AggregateState and reevaluates every frame member for every output row.

**Primary:** cumulative SUM/count at 100/1,000 rows and fixed-width moving frames;
also existing `tier3_window_moving_agg_100`. Controls: singleton/tiny partitions,
ordinary aggregates, wide sliding frames, full-partition and empty frames.

**Correctness/rejection:** ROWS/RANGE/GROUPS peers, NULL/filter behavior, offsets,
empty frames, overflow and output types. Floating-point addition/removal is not
algebraically exact; DISTINCT/order-sensitive/volatile inputs cannot be blindly
reused. Differential/property tests must establish legal reuse and evaluation
counts, including lock waits. Do not change evaluation order to hide errors.

### E10 — Allocate resumable expression journals only where suspension is possible [PENDING]

**Idea:** use one execution capability derived from bound metadata to choose
ordinary evaluation or resumable journaling. The generic resume abstraction
should own this decision; avoid per-function fast paths. E06 supplies reusable
structural information, but a structural flag is not itself a proof that an
operation or its nested queries cannot suspend.

**Evidence/source:** grouping calls `expressions/resume.rs::resume_evaluation`
per source row; it allocates Arc/Mutex state, clones StatementContext and clones
the journal even for the measured ordinary aggregates. Group profiles show
context/value cloning and allocation rather than group-key comparisons (0.9%).

**Primary:** grouped aggregates with ten groups, narrow/wide rows at 100/1,000,
and ordinary aggregate inputs. Controls: DML defaults/RETURNING, procedural and
CTE paths, advisory functions, scalar subqueries and deep nested expressions.

**Correctness/rejection:** force suspension after earlier volatile operands,
after multiple waits, and during cancellation/savepoint rollback. Sequence,
random and timestamp behavior must be unchanged. Reject any capability analysis
that misses indirect waits; preserve the journal fallback without accumulating
query-shape exceptions. Re-profile after E06/E07 before committing to this scope.

### E11 — Reuse typed expression evaluation across general query consumers [PENDING]

**Idea:** extend the existing bound-expression representation so generic filters,
aggregate inputs and window projections use resolved types, column slots and
literal values. Resolve metadata once; evaluate values at the correct runtime
scope. This is a shared expression change, not separate JSON/window fast paths.

**Evidence/source:** the containment profile includes 40.7% in type inference and
43.9% in unknown coercion (overlapping stacks). Ranking repeatedly substitutes
window results into cloned ASTs. Inspect `executor/prepared.rs`, `expressions`,
`scope`, `query/grouping` and `query/windows`. Bind once per statement where
cross-execution reuse is unsafe.

**Primary:** JSONB containment/extraction and computed grouped/window outputs at
100/1,000 rows; contrast prepared and one-shot calls. Controls: simple prepared
reads, parameter types, array/temporal casts, cold preparation and DML predicates.

**Correctness/rejection:** prepared-plan invalidation, search_path, TimeZone,
typed NULLs, typmods, polymorphic parameters, CASE laziness, errors and volatility.
Do not cache `now()`, sequence calls or other runtime values across executions.
Reject a second incompatible expression interpreter; reuse the existing enum and
central coercion, with explicit unsupported-expression fallback.

### E12 — Share window partition/order layouts across compatible functions [PENDING]

**Idea:** prepare one layout per semantically identical partition/order definition,
then evaluate window functions over its ordered row indices and peer boundaries.
Keep function-specific frames and volatile occurrences separate. This removes
repeated partition-key evaluation and sorting as function count grows.

**Evidence/source:** the rank profile spends 31.1% in window calculation; partition
and order expression loops repeat per function. `window_rank_100` computes three
functions on the same named window; the offset benchmark computes five.

**Primary:** rank/offset queries with 1/3/5 functions, 100/1,000 rows, and
10/high-cardinality partitions. Controls: one function, different windows,
computed/volatile keys and moving aggregates after E09/E11.

**Correctness/rejection:** sharing must respect SQL expression identity, peers,
NULL ordering, frame differences, grouped input and evaluation frequency. Do not
assume equal SQL text is a shareable volatile occurrence. This has medium
potential: projection materialization is a separate measured cost, so layout
sharing alone may fail the 10% gate after earlier changes.

### E13 — Resolve session time zones once per setting value [PENDING]

**Idea:** carry a resolved Zone with its setting value/revision and reuse it for
temporal execution and coercion. Preserve distinct explicit-zone and session-zone
resolution rules; do not add a special `if UTC` workaround or a global cache.

**Evidence/source:** temporal profile includes 13.3% in `chrono_tz::Tz::name`.
`coercion/time_zones.rs::parse_session_zone` linearly searches TZ_VARIANTS on each
call. The native 1,000-row workload takes 2,674.59 µs; timestamp formatting and
numeric division remain other costs, so the possible gain is limited.

**Primary:** temporal workload plus a pure timestamptz conversion workload at
100/1,000 rows, for UTC and named DST zones. Controls: one-row conversions,
OffsetDateTime/SQLx codecs, casts, setting changes and snapshot/session creation.

**Correctness/rejection:** SET/SET LOCAL/RESET, savepoint rollback, prepared reuse,
`set_config` during evaluation, aliases, offset signs, DST gaps/overlaps, infinity
and BC dates. Read the current semantic setting, not a stale statement-start
copy when changes are visible. Measure after E11; reject if savings fail either
acceptance threshold rather than adding unrelated formatting tweaks.

### E14 — Evaluate JSONB containment on its existing structured representation [PENDING]

**Idea:** traverse normalized JSONB structure directly and compare only relevant
subtrees, replacing text reparsing and the all-node-pairs boolean matrix. Share
structural navigation with other JSONB consumers where useful, preserving raw
JSON's distinct representation and semantics.

**Evidence/source:** `jsonb.rs` stores normalized tokens and text, but
`executor/json/operators.rs::check_containment` reparses nodes, allocates an
O(L*R) matrix and reparses scalar JSONB pairs. It accounts for 39.3% of the
containment profile. E11 may first remove other repeated parsing/coercion costs.

**Primary:** existing JSONB containment plus nested/wide objects and arrays,
constant and bound RHS, varying document/RHS sizes and hit positions. Controls:
JSONB insert/RETURNING, extraction, concatenation, equality/hashing, JSON and tiny
documents. Measure peak memory and allocation traffic as well as latency.

**Correctness/rejection:** numeric equivalence, duplicate object keys, nested
arrays, array/scalar containment, order/duplicate insensitivity where PostgreSQL
requires it, JSON null vs SQL NULL, Unicode and deep nesting. Arrays may still
require many comparisons; do not promise universally linear containment. Avoid
storing several redundant trees per value without accounting for write/memory cost.

### E15 — Share immutable stored row payloads across database forks [PENDING]

**Idea:** make immutable MVCC row payloads shareable so a fixture fork and captured
query source can reuse values while owning independent version metadata. Updates
create new payloads. Start with this existing immutability boundary, rather than
replacing every map or adding whole-table copy-on-write that penalizes the first
write to a large fixture.

**Evidence/source:** `database/mod.rs::snapshot` deep-clones state. Wide-row
snapshot/drop takes 338.20 µs at 1,000 rows versus 135.90 µs for NULL payloads;
profiles are dominated by row/index cloning and allocator work. The benchmark
includes dropping the returned fork; it is not creation latency alone.

**Primary:** snapshot/create-use-drop lifecycles at 100/1,000/10,000 rows and
different payload widths. Measure read-only forks, first updates/inserts/deletes,
multiple simultaneously live forks and final disposal. Controls: all Tier 1
writes, scans, joins, DDL and query-source capture across waits.

**Correctness/rejection:** fork isolation for rows/catalog/sequences/RNG/time;
omit uncommitted and temporary state as today; maintain RowId/Xid counters and
transactional truncate/rollback. Record retained memory with long-lived forks.
Reject shifted first-write costs or >5% reference-counting regressions. Index
and metadata cloning remain; this is not a promise of O(1) snapshots.

### E16 — Search retained MVCC history by committed visibility boundary [PENDING]

**Idea:** maintain searchable committed-version boundaries per row so an old
snapshot does not walk every later version. Keep uncommitted/current-transaction
versions and command visibility explicit; verify a candidate with the existing
visibility predicate. Choose the simplest representation justified by tests.

**Evidence/source:** one-row old-snapshot lookup takes 0.573 / 1.848 / 671.349 µs
after 1 / 100 / 10,000 updates; 99.8% of engine-profile samples are in
`txn.rs::find_visible_version`, which reverse-scans the version vector.
This is a real but Tier 3 hotspot, hence its late priority.

**Primary:** old/middle/latest snapshot lookups at fixed table size and varying
history depth, plus many concurrently retained snapshots. Controls: ordinary
one-version lookup, updates/commit/rollback, GC, transactional DDL/truncate and
snapshot forks. Measure metadata memory per live row/version.

**Correctness/rejection:** Xid order is not commit order; aborted versions and
savepoints break naive assumptions. Never binary-search the raw vector assuming
all visibility is monotone. Validate current Xid/CommandId, deletes, reinserted
keys, RC/RR/SSI and concurrency. Reject a gain that makes ordinary writes >5%
slower or needs a sprawling cache-invalidation system.

## Ideas deliberately deferred

- Another parsed-SQL cache: the specification excludes it; SQLx already caches
  prepared statements. Literal-varying Tier 1 writes spend substantial sampled
  time in preparation/parsing, but hiding that by rewriting the benchmark to
  bound SQL changes the workload. Keep both as controls.
- Another generic hash join or top-k sorter: both exist. Target the measured
  eligibility/outer-join gaps instead. Transaction-history point lookups are also
  already effectively flat in the saved diagnostic suite.
- Regex compilation cache: constant regexes are already statement-cached. In the
  measured pattern workload matching occupies only 4.4% of retained samples and
  LIKE evaluation 8.8%; these alone cannot justify the composite workload's 10%
  gate. Revisit only with a separately representative measured target.
- Format strings as cache keys / fewer tiny allocations: VALUES profiles assign
  about 5.3% to formatting, substantially below repeated AST traversal. Reassess
  after E06 rather than bundling micro-optimizations into that experiment.
- Removing SQLx `spawn_blocking`, global-lock sharding or parallel execution:
  scheduling is visible, but current measurements do not isolate a safe dominant
  improvement. Blocking locks must still progress on single-thread runtimes.
- Fully compiled mutation plans and cached DML relation-lock templates: bound
  UPDATE profiles show lock discovery/row preparation, and bound INSERT shows
  relation-lock discovery. E01/E06/E10/E11 address shared causes first. Re-profile
  before adding another prepared executor, and never cache dynamic row locks.
- General optimizer search, arbitrary predicate implication, persistent-map
  replacement, and every possible window inverse transition: defer these larger
  designs unless the bounded experiments demonstrate a need. Simplicity is part
  of the decision, not a cleanup task after performance work.
