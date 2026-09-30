# Recovery commit series

The user explicitly authorized Git writes after the earlier automatic-review
block. The seven experiment commits below now exist; the eighth commit records
this audit. Each experiment record contains its full timing and validation scope.
The ordered patches were replayed on the original base, checked by content hash,
and compiled/tested at each intermediate state before committing. Independent
review found no remaining split or evidence issue.

1. `8310db2` — **Share immutable session defaults** — E11 session/settings changes and probe.
   Native INSERT 23.273 → 14.775 µs; UPDATE 31.556 → 23.408 µs;
   transaction INSERT 24.053 → 15.256 µs. E11-only session source is saved at
   `/tmp/e11-session-mod.rs`. Record: [E11](e11_defaults.md).
2. `e569753` — **Bind temporal and numeric scalar expressions for prepared queries** — E7
   prepared scalar nodes, shared evaluators, timezone plumbing, regression/probe.
   Native prepared 100-row temporal query 600.890 → 271.622 µs (54.8%).
   Query example: `SELECT to_char(date_trunc('minute', to_timestamp(id)),
   'YYYY-MM-DD HH24:MI:SS'), floor(id::numeric / 7) FROM t`.
   E7-only prepared/query snapshots are `/tmp/e13-*-before.rs`.
   Record: [E7](e7_temporal.md).
3. `971f0cd` — **Reuse bounded ordering for prepared reads and pages** — E13 bound ordering,
   shared heap comparator, fallback rules, tests and probes.
   Native prepared 100-row page 42.149 → 9.727 µs (76.9%);
   SQLx ordered page 137.040 → 25.010 µs (81.8%).
   Query example: `SELECT id, price FROM t ORDER BY price DESC, id LIMIT 5 OFFSET 3`.
   Record: [E13 ordering](e13_ordering.md).
4. `c072977` — **Measure bound INSERT and UPDATE companions** — E13 standalone diagnostic,
   with original inline SQL kept visible; no write-executor implementation change.
   SQLx INSERT 39.646 → 28.609 µs (27.8%); UPDATE 47.229 → 37.472 µs (20.7%).
   Query examples: `INSERT INTO t VALUES ($1, $1)` and
   `UPDATE t SET value = $1 WHERE id = 0`.
   Record: [E13 DML](e13_dml_bindings.md).
5. `64c30e9` — **Reuse window-expression templates across output rows** — E14 window
   templates and owner correction, grouped-window integration, tests and probes.
   Native prepared 100-row five-window query 1,013.527 → 526.760 µs (48.0%);
   SQLx 1,012.202 → 556.467 µs (45.0%).
   E14-only grouped executor is `/tmp/grouping-mod-before.rs`.
   Query example: `SELECT lag(id) OVER (ORDER BY id), lead(id) OVER (ORDER BY id)
   FROM t`. Record: [E14](e14_windows.md).
6. `5247ad1` — **Use hash lookup for high-cardinality grouping** — normalized internal keys,
   lookup above 128 groups, fallback/resumption tests and native/SQLx probe.
   Native prepared 1,000 groups 4,208.240 → 2,769.666 µs (34.2%);
   10,000 groups 191,862.945 → 35,195.179 µs (81.7%). Disclose the final
   small-group repeat's approximately 0.5–3.2% cost.
   Query example: `SELECT bucket, count(*) FROM t GROUP BY bucket ORDER BY bucket`.
   Record: [grouping](grouping.md).
7. `287ca65` — **Borrow queries when restoring projection names** — small E8 follow-up;
   remove the full-query clone used only to wrap a query in a temporary statement.
   Exact nested-view SQLx workload: 384.681 → 366.427 µs (4.7%).
   Retain as a shared simplification; 1,000-row probe means are small/noisy,
   with unprepared execution 2.1% higher.
   Record: [projection names](projection_names.md). Pre-change snapshots:
   `/tmp/projection-{query,projection,session}-before.rs`; keep this change out
   of earlier overlapping executor/preparation commits.
8. **Record the recovered optimization audit** — final plan/recovery status,
   environment/profile diagnostics and fresh benchmark assessment. Include no
   claim that every workload meets the PostgreSQL/10 target.

Keep raw Criterion output and temporary rejected source candidates out of these
commits. Preserve unrelated user content, including the pre-existing `todo.md`.
