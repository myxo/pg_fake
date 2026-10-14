# Optimization research, 2026-10-12

Research for the [optimization plan](../../optimization_plan.md), against commit
`2b5243fc043adef6d713de71b2f9bc03dd57074a`. Engine code was unchanged. Measurements
are observations of the baseline and diagnostic controls, **not demonstrated
optimization gains**. The plan requires fresh paired measurements for acceptance.

## Environment and methods

- Apple M2, ARM64, 16 GiB RAM; macOS 15.8.1 (24H32), as reported by `sample`.
- `rustc 1.98.1 (48a229cea 2026-09-01)`. Release optimization; native probes linked
  to the library built for the benchmark suite. Probe driver compiled with `-O -g`.
- Native probes: preparation/setup excluded, 16 warmup operations, nine batches
  of at least 120 ms, reporting median/minimum/maximum **batch mean**, in µs.
  This is not a p50/p99 latency distribution and does not supply a confidence
  interval. Results are dropped within the timed operation.
- Native general probe: 10/100/1,000 rows; four columns, including deterministic
  text and nested JSONB. Prepared queries, fixed query constants. Contrasts change
  unused payloads to NULL **at insertion**, keeping the same schema and row/key
  counts. Wide/NULL pairs were rerun consecutively. MVCC uses one row and a held
  repeatable-read snapshot while another session commits 1/100/10,000 updates.
- Handoff probe: deterministic UUIDs, prepared queries, rotating bound keys, one
  matching row, 10/100/500/1,000 rows in each table. UPDATE/DELETE are rolled back
  after each iteration. Their timer encloses the prepared mutation and result
  drop; BEGIN/ROLLBACK are outside it. Reads use per-operation timers too. There
  is a small unsubtracted clock overhead, relevant to the fastest controls.
- Criterion: 27 existing benchmark cases through a temporary fake-only entry
  point, 20 samples, 1 s warmup, 2 s measurement. CSVs report Criterion **mean**
  estimates and 95% intervals. Console output may report slope estimates instead;
  do not mix those statistics. The two noisy cases were repeated with requested
  40 samples, 2 s warmup, 5 s measurement. Indexed-write groups override the latter
  two measurement settings with 20 samples / 2 s; the retained log shows this.
- CPU sampling: macOS `sample`, 4 seconds at 1 ms intervals, started only after
  the native probe emitted READY or Criterion entered its profiling phase.
  Timings and profiles were separate; no timed benchmark ran alongside another
  benchmark/profiler/build. Native profiles ran for 7 seconds after warmup.
- `stackcollapse-sample.awk` removes common waiting stacks. Percentages below are
  inclusive fractions of retained samples, not exact wall-time fractions or
  allocation counts; overlapping stacks **must not be summed**. SQLx worker
  sampling cannot directly quantify scheduling latency. Write profiles include
  untimed fixture cleanup/rollback, so are attribution hints rather than a timed
  instruction breakdown. Some optimized functions are inlined/deduplicated.
- Sampling reports showed substantial allocator work. This is evidence of CPU
  spent allocating/freeing, not a measurement of allocated bytes per query.
  Allocation-volume and retained-memory gates remain required for experiments.

The regular benchmark harness was built successfully, but its PostgreSQL
connection failed under the sandbox. Automatic approval review rejected retrying
against the configured destination: the harness drops `pgfake_benchmark` with
CASCADE, and that destination was not verified disposable. The safe replacement
constructs **only PgFake connections**, calling existing workload functions with
a fake-only backend list. It never invokes PostgreSQL setup, schema cleanup, or
the PostgreSQL serializable benchmark. No PostgreSQL differential tests ran for
this planning task, and the 27-case screening run is not a full-suite baseline.

## Reproduction sources and local evidence

Keep generated benchmark results, logs, profiles, and archives out of commits.
The research outputs are stored locally in
`target/optimization-research/results/2026-10-12/`, which Git ignores. They are
not distributed with this document; the findings below summarize the runs.

| File | Contents |
| --- | --- |
| [probe.rs](probe.rs), [handoff_probe.rs](handoff_probe.rs) | SQL, fixtures, timer boundaries and basic pre-timing row-count checks. |
| [fake_entry.rs](fake_entry.rs), [prepare.py](prepare.py) | Rebuild the isolated probes and existing SQLx workloads without a PostgreSQL connection. |

Row-count checks keep a broken/empty workload from producing misleading timings;
they are not a replacement for differential result/SQLSTATE verification. These
small research programs are diagnostic artifacts, not new production benchmarks
or changes to the supported API. Integrate stable regression cases with the
Criterion catalog when executing each experiment.

For implementation experiments, `prepare.py --full-fake --sqlx` builds the full
pg_fake-only Criterion catalog. `--e01 --sqlx` builds only the guarded-mutation
cases, and `--regression --sqlx` builds the E01 regression-control groups. None
of these entry points starts a PostgreSQL benchmark. `--serializable --sqlx`
builds the serializable pg_fake controls. PostgreSQL remains in the differential
correctness tests.

## Feature-by-feature findings

### Selective joins and guarded writes: assessment of the handoff

Input: `/Users/myxo/tmp/pg_fake-agent-handoff.md`, supplied by the user. Its
investigated revision is exactly the current engine revision. I reproduced its
schema and query shapes, including original partial/composite indexes and
separate diagnostic plain indexes. These release-build native measurements are
not directly comparable to its opt-level=1 timings or consuming application's
end-to-end workload.

Median batch means, µs; exactly one row matches in every case:

| Query/control | 10 rows/table | 100 | 500 | 1,000 |
| --- | ---: | ---: | ---: | ---: |
| Account PK SELECT | 1.44 | 1.43 | 1.47 | 1.48 |
| Account LEFT JOIN roles | 37.07 | 141.91 | 604.44 | 1,189.53 |
| Credential LEFT JOIN group, guarded | 56.65 | 218.63 | 940.51 | 1,838.67 |
| Membership profile, original indexes | 66.62 | 133.04 | 397.00 | 756.37 |
| Same + diagnostic plain indexes | 2.81 | 2.79 | 2.97 | 2.90 |
| Group members, original indexes | 74.10 | 136.76 | 409.48 | 757.50 |
| Same + diagnostic plain indexes | 2.80 | 2.87 | 3.00 | 3.02 |
| Complete composite equality | 1.74 | 6.19 | 26.43 | 51.97 |
| Composite leading-key query | 1.70 | 5.41 | 22.38 | 43.42 |
| Bare-key UPDATE | 23.29 | 23.52 | 23.57 | 23.60 |
| Guarded UPDATE | 73.76 | 407.37 | 1,897.23 | 3,758.17 |
| Bare-key DELETE | 11.53 | 11.62 | 11.63 | 11.59 |
| Guarded DELETE | 56.28 | 390.72 | 1,894.88 | 3,707.73 |

The report's principal diagnosis is supported. Source inspection also confirms
outer joins suppress selection pushdown, prepared access requires single-column
nonpartial indexes, and mutation point lookup accepts only a top-level equality.
The fast application-like inner-join benchmarks already in this repository use
eligible indexes; they do not disprove these different query-shape gaps.

The independent 1,000-row profiles reinforce this attribution. Guarded UPDATE
spends 97.9% of retained samples under row-lock acquisition and 57.0% under
expression type inference during that discovery. LEFT JOIN roles spends 96.6%
under the existing hash-join chain and 52.6% evaluating WHERE; it is not missing
a generic hash join. The original membership query spends 83.8% under table-factor
scanning, including 36.6% under type inference. These are inclusive, overlapping
counts, and the write profile includes rollback. They support reducing candidate
rows before expression work, rather than merely making every rejected row cheaper.

Important limits on the report's interpretation:

- This establishes scaling with unrelated rows in **referenced source tables**;
  it does not demonstrate scanning every unreferenced table in a database.
- Adding the diagnostic indexes changes access eligibility **and** whether a
  prepared join plan is selected. It does not isolate index lookup cost alone.
  A schema workaround is not an engine optimization result or proposed fix.
- The claimed 17% application-workload improvement was not independently rerun.
  The report has no raw samples/profile artifacts here; its numbers are context,
  while the findings below come from independent engine measurements.
- The current reproduction has one active role/membership per account. Zero,
  multiple and soft-deleted matches, concurrency and full PostgreSQL comparisons
  are mandatory experiment tests, not coverage established by these timings.
- Outer-join WHERE pushdown is not generally valid. Predicate provenance and
  preserved-side safety must be proved. Likewise a unique composite prefix is
  not a unique key, and a partial index cannot answer queries outside its predicate.

These findings move candidate selection, selective LEFT JOIN execution and index
eligibility to the first five plan experiments. The guarded write is first because
it is both a large measured penalty and a bounded change to shared candidate
selection; this differs from the handoff's ordering without rejecting its thesis.

### Existing ordinary reads/writes and adapter costs

Fresh SQLx means: INSERT 38.32 µs, full 100-row SELECT 22.16 µs, indexed selective
SELECT 8.71 µs, ordered paging 21.87 µs, ordering 32.26 µs. Selective/many-match
inner joins are 15.67/21.83 µs. At 1,000 users, the existing profile/member join
fixtures measure 13.06/33.01 µs. These are valuable regression controls, not
evidence that all join shapes are solved.

Bound INSERT/UPDATE measure 28.76/34.46 µs. Literal-varying INSERT/UPDATE CPU
profiles show roughly 41–43% of **engine-containing** sample stacks in preparation
and 19–20% in parsing. Prepared bound-write profiles shift toward lock discovery,
binding and row preparation. Caching another SQL string is therefore neither a
general explanation nor consistent with the core specification. Changing literal
benchmarks to parameterized SQL would change the workload.

Native vs SQLx adapter SELECT means are 14.14 vs 25.70 µs for the catalog's fixture.
The difference includes scheduling, adaptation and result handling; it is not an
isolated estimate of `spawn_blocking` overhead. No proposal to remove blocking
offload is justified by these measurements.

The initial UPDATE and indexed DELETE have wide intervals. Their longer/requested
repeat still shows outliers, so neither is a trustworthy numerical acceptance
baseline. Keep both raw runs, mark them noisy and re-establish quiet paired
controls when executing an experiment; do not choose the fastest estimate.

### Repeated AST work, expressions and grouping

VALUES UNION ALL at 100/1,000 rows costs 372.68/4,282.14 µs natively even though it
has no duplicate elimination. The 1,000-row profile includes 51.4% under stack
availability checks and 21.9% in view expansion. Repeated recursive traversals,
including lock discovery and lateral-initplan discovery, dominate; do not infer
that the parser is running each time merely because sqlparser symbols appear.
Only about 5.3% is under formatting. This supports reusing structural properties,
not removing stack safety or prioritizing string-format micro-optimizations.

Patterns at 1,000 rows cost 1,738.31 µs. Read-expression AST visits cover 32.6% of
retained samples, LIKE evaluation 8.8%, regex evaluation 4.4%. A constant regex
cache already exists. This redirects effort from a presumed regex bottleneck to
repeated expression discovery.

Ten-group aggregation costs 168.42/1,086.08 µs at 100/1,000 wide rows. Profiles
show cloning and allocation in scanning, input preparation and resumable journals;
value comparison is only 0.9%. High-cardinality grouping already enables hash
lookup at 128 groups. The paired wide/NULL contrast is 1,058.62/853.94 µs, while
the prepared scan control is 43.92/44.20 µs. Thus the payload effect is specific
to paths that materialize/copy it, not a universal storage-scan penalty.

### DISTINCT and set operations

At 1,000 rows, unique-integer DISTINCT is 1,912.97 µs versus 367.62 µs for ten
distinct values. Duplicate elimination occupies 60.3% of its sampled execution.
The implementation compares every row with previously selected rows; set
intersection/difference likewise search right rows. This is strong evidence for
a shared equality-key algorithm. Preserve SQL NULL/type semantics and output
visitation, rather than replacing comparisons with Rust Value equality.

Table UNION ALL/UNION at 1,000 rows measure 787.00/3,669.37 µs. VALUES forms measure
4,282.14/7,026.84 µs: substantial setup/expression work coexists with duplicate
cost. Existing 100-row SQLx VALUES UNION ALL/UNION means are 392.44/426.08 µs, so
the legacy 100-row UNION case alone may understate the algorithmic opportunity.

### Windows

Cumulative integer SUM costs 710.15 µs at 100 rows and 56,765.90 µs at 1,000 rows.
Window calculation occupies 95.9% of the 1,000-row sample; aggregate input
preparation occupies 64.6%. Repeatedly rebuilding each growing frame explains
the scaling and is a strong incremental-aggregation target.

The bounded moving-frame native case is 419.18/3,781.88 µs at 100/1,000 rows;
constant-width frames do not have the same quadratic growth. Rank with three
functions is 325.46/2,849.85 µs. Its layout computation accounts for 31.1%, while
projection/materialization is substantial too. Sharing a named-window layout is
plausible, but cannot claim all of rank's cost. The engine also allocates frame
index vectors for rank functions that do not consume them; measure that residual
after the general changes before creating a separate narrow optimization.

### JSONB and temporal expressions

Native nested containment costs 939.84 µs per 100-row query; its documents differ
from the existing SQLx containment benchmark (264.37 µs). The profile includes
40.7% type inference, 43.9% unknown coercion and 39.3% containment itself; these
overlap. Existing normalized JSONB tokens are converted through text and parsed
again, while containment constructs an all-node-pairs matrix. Both general typed
evaluation and structural containment merit distinct experiments.

Temporal work is 269.00/2,674.59 µs at 100/1,000 native rows. Timestamp formatting
accounts for 33.5%, numeric division for 16.8%, and time-zone naming for 13.3%.
Session-zone parsing linearly searches the zone list repeatedly. Resolving the
setting once is a general repeated-work optimization; its measured ceiling is
smaller than the large query-algorithm opportunities. Formatting changes alone
need a separate representative hypothesis rather than being bundled in.

### Snapshots, MVCC, and already optimized paths

Snapshot plus immediate drop scales 4.86/35.38/348.67 µs at 10/100/1,000 wide
rows. A paired 1,000-row rerun gives 338.20 µs wide vs 135.90 µs NULL payloads.
Profiles show payload/index cloning and allocator work. Sharing immutable rows
is a narrower experiment than replacing the map; benchmark first-write and
retained-memory costs to prevent shifting expense to consumers.

Fixed-one-row MVCC read scales 0.573/1.848/671.349 µs at 1/100/10,000 retained
updates. Visibility search occupies 99.8% of engine-containing samples. This is
an algorithmic hotspot in long retained history, not a reason to complicate
ordinary visibility checks without measuring their cost. Xid and commit order
must not be conflated.

The ordinary prepared PK probe remains about 1.1 µs across 10/100/1,000 wide rows.
The committed transaction-history diagnostic is almost flat across 1–100,000
transactions. Storage already tracks touched/reclaimable rows and tables rather
than always rebuilding every index or scanning the whole database at commit.
Hash joins, prepared inner joins, membership filtering and bounded top-k sorting
also exist. These observations rule out several tempting but redundant proposals.

## Reproduction

From the repository root, with the locked dependencies already available:

```sh
python3 docs/optimization_research/2026-10-12/prepare.py
target/optimization-research/probe distinct 1000
target/optimization-research/probe group 1000 0 null_payload
target/optimization-research/probe mvcc 10000
target/optimization-research/handoff_probe left_roles 1000
target/optimization-research/handoff_probe memberships_plain 1000
target/optimization-research/handoff_probe guard_update 1000
```

`probe CASE N [PROFILE_SECONDS] [wide|null_payload]`; cases appear in `probe.rs`.
`handoff_probe CASE N [PROFILE_SECONDS]`; `_plain` adds the two diagnostic indexes.
Probe defaults measure nine batches. Profile mode prints READY after setup and
warmup, then loops; start `sample PID 4 1 -mayDie -file REPORT` only after READY.
Profiling may need permission to inspect the newly launched child process.

Build the fake-only version of the existing Criterion workloads:

```sh
python3 docs/optimization_research/2026-10-12/prepare.py --sqlx
target/release/research-workloads --bench '(tier1_.*|tier2_(insert_bound_row|update_bound_row|update_indexed_1k|delete_indexed_1k|foreign_key_insert|grouped_aggregate_100|union_all_100|union_100|jsonb_containment|runtime_patterns_100|runtime_temporal_100|adapter_select_100|join_profile_1k|join_members_1k)|tier3_window_(rank|moving_agg)_100)(/|$)' --warm-up-time 1 --measurement-time 2 --sample-size 20 --noplot --save-baseline optimization-research
```

The helper copies the current workload source into `target/optimization-research`
and replaces only its entry point with [fake_entry.rs](fake_entry.rs). No engine
source is modified. It intentionally covers a subset; functions outside that
entry point are not measured even if a filter names them. Unselected functions
inside the entry point still perform fixture setup, as in the regular harness.
Before using on a changed revision, review the generated entry point and fixture
functions for changed setup or side effects. A separate temporary Cargo package
reuses the workspace target directory and lockfile dependencies.

For SQLx CPU profiles, use the same binary with `--profile-time 7`, wait for its
`Profiling for` line, and sample only that process. The native and SQLx profiles
were collapsed with the installed FlameGraph tools; Rust v0 symbols were
demangled using the locally available `rustc-demangle 0.1.27`. Re-render a
locally retained profile (use the Perl interpreter explicitly if the tool's
shebang is unavailable on the current macOS):

```sh
gzip -dc target/optimization-research/results/2026-10-12/distinct-1000.folded.gz | perl /opt/homebrew/bin/flamegraph.pl --title 'DISTINCT, 1000 rows' > target/optimization-research/distinct.svg
```

Keep newly generated evidence under `target/` or another ignored local directory.
The old committed suite results (October 9, an earlier OS version and engine
state) remain untouched; they informed workload selection but are not a valid
paired before/after comparison for this research.
