# Remaining workload profiles — 2026-09-30

The release `workload_profile` example runs representative 100-row native
workloads. It prepares single statements outside the loop and warms once.
DDL and advisory locking use multi-statement `Session::execute`, including
parsing. These are family probes, not exact Criterion fixtures or SQLx timings.
Each process started fresh, ran for one second, then was sampled for two seconds
at a requested 1 ms interval. No build, test or other profile overlapped sampling.
All 13 cases reached their ready marker without error.

Build with `cargo build -p pg_fake_sqlx --release --example workload_profile`.
Run `target/release/examples/workload_profile NAME`, then
`sample PID 2 1 -file PROFILE.txt`. The harness exits after 60 seconds; the
sampling controller terminated its own process after sampling. Raw local logs:
`/tmp/recovery-profiles/NAME-{run.log,profile.txt,sample.log}`.

The counts below are exclusive/self samples from “Sort by top of stack, same
collapsed,” against main-thread totals. They are observations, not predicted
speedups. Allocator symbols alone do not identify which allocation to remove.

| Probe | Main samples | Selected self samples | Next investigation |
| --- | ---: | --- | --- |
| view | 1,425 | memmove 184; remaining_stack 125 | Trace AST cloning/visitor ownership through nested views; highest full-sweep priority. |
| jsonb | 1,520 | nanov2_free 302; Jsonb::parse 78; format_postgres_text 29 | Trace typed JSONB conversion and repeated constant evaluation before considering indexes. |
| array | 1,523 | remaining_stack 146; resolve_bound_column 96; infer_expression_type 48 | Extend shared binding only where it removes repeated row-loop resolution. |
| grouped | 1,521 | nanov2_free 138; memmove 72; Value::clone 35 | Inspect ordered-aggregate input ownership; high-cardinality lookup does not address this ten-group case. |
| subquery | 1,513 | resolve_bound_column 456; remaining_stack 80; infer_expression_type 59 | Correlated scope resolution is a concrete residual cost (30.1% self); measure binding changes separately. |
| cte | 1,523 | remaining_stack 106; resolve_bound_column 72; Value::clone 37 | Inspect materialized-row ownership and output expression binding. |
| write | 1,530 | nanov2_free 151; memmove 84; Value::clone 28 | Attribute snapshot/context/value copies in the existing write path before another write-specific path. |
| ddl | 1,532 | memmove 163; nanov2_free 123; keyword_lookup 54 | Separate parsing, schema mutation and retained history with scaling controls. |
| lock | 1,527 | memmove 131; remaining_stack 118; nanov2_free 93 | Shared statement overhead remains visible in uncontended advisory lock/unlock; this does not measure contention. |
| union | 1,437 | compare_values 172; remove_set_duplicates 90 | Measure comparison/dedup scaling with supported equality keys and fallback controls. UNION ALL needs its own attribution. |
| distinct | 1,530 | nanov2_free 154; Value::clone 44; remove_duplicate_rows 40 | Keep current algorithm until a scaling experiment separates materialization from deduplication. |
| temporal | 1,530 | format_timestamp 253; evaluate_prepared_runtime_function 233; Tz::name 162 | Inspect formatting and timezone conversion within the retained prepared path. |
| window | 1,525 | remaining_stack 162; AST ReadVisitor 42; calculate_window_values 33 | Repeated visitor/recursion overhead remains; retain the rejected identical-spec cache decision until stronger evidence. |

Prioritize nested views, then tier-1 joins and measured expression/scope costs.
The fresh sweep measures the joins but these 13 probes do not separately profile
them. Profile those exact shapes before selecting another join change.
Do not remove recursion protection based solely on stack-check samples.
Do not infer universal improvements from allocator counts or add a transparent
cross-statement cache.

The full sweep covers additional writes, schema operations, snapshots and
concurrency shapes. These family profiles do not establish individual causes for
FK/default/identity/upsert/savepoint costs, old-version chains, SERIALIZABLE,
forced contention, recursive queries, or every specialized combination.
Those remain candidates for targeted profiling, not unexplained gains claimed
by this recovery. Deliberate contention sleeps remain part of their workload.
