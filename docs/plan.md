# pg_fake — Phase 1 Implementation Plan

Phase 1 delivers the MVP scope from `spec.md` §9: core DDL, full CRUD, basic
expressions, Phase-1 types, constraints, transactions with READ COMMITTED /
REPEATABLE READ, row locking with deadlock detection, and a `sqlx` driver.

The plan is a linear sequence of small, self-contained tasks. Each task lists a
**Goal**, a **Definition of Done (DoD)** with concrete/testable criteria, and
**Notes** (dependencies, scope boundaries, spec references). Tasks are ordered so
that a runnable end-to-end path appears early (Milestone C) and every later task
is continuously verifiable against real Postgres 18 via the differential harness.

Conventions:
- "Differential test" = same SQL run against real Postgres 18 and `pg_fake`,
  results compared (see task 13).
- Spec references like (§5.3) point into `docs/spec.md`.
- Unless stated otherwise, everything before Milestone G runs single-threaded in
  autocommit mode.
- Every SQL feature task must extend the property-based differential generator
  to cover the feature and its interactions with existing features. If property
  testing is not applicable, the completion handoff must explain why and provide
  equivalent focused differential coverage.
- Every feature task must add or update a representative benchmark workload when
  applicable. If benchmarking is not applicable, the completion handoff must
  explain why.
- Before a task can be marked complete, the property test suite must pass at
  least 10,000 `chaos_theory` iterations, run with
  `CHAOS_THEORY_CHECK_ITERS=10000 cargo test -p pg_fake --test property_tests`.

---

## Milestone A — Foundations

### Task 1 — Workspace & crate scaffolding [COMPLETE]
**Goal:** Establish the Cargo workspace and core crate skeleton so later tasks
have a place to live.

**DoD:**
- Cargo workspace with the core crate `pg_fake` (the `sqlx` crate is added in
  task 33).
- Module skeleton matching the architecture (§2): `value`, `error`, `catalog`,
  `storage`, `txn`, `parser`, `analyzer`, `executor`, `api`.
- Chosen dependencies declared (e.g. `sqlparser`, `bigdecimal`); no unused deps.
- `cargo build` and `cargo test` succeed with a trivial placeholder test.
- Lints configured (`clippy` clean, `rustfmt` applied); basic CI-style check
  script or `just`/`make` target documented in the README.

**Notes:** No behavior yet. Keep module boundaries aligned with the spec so
future tasks slot in cleanly.

### Task 2 — Error model: `SqlState` + `PgError` [COMPLETE]
**Goal:** A single error type carrying a Postgres `SQLSTATE` code (§11).

**DoD:**
- `SqlState` type (enum or newtype over the 5-char code) covering the codes
  Phase 1 will emit (e.g. `0A000`, `22003`, `22012`, `22P02`, `23502`, `23505`,
  `23514`, `25P02`, `40001`, `40P01`, `55P03`, syntax/undefined-object codes).
- `PgError { sqlstate, message, detail?, hint?, position? }` with constructors
  and `Display`/`Error` impls.
- `Result<T> = std::result::Result<T, PgError>` alias used across the crate.
- Unit tests: code round-trips to/from its string form; a couple of
  constructor helpers produce the expected code.

**Notes:** `sqlstate` is Tier A; message text is Tier B and not asserted on in
differential tests. Codes are added incrementally as later tasks need them.

### Task 3 — `Value` type & Phase-1 type system [COMPLETE]
**Goal:** The `Value` enum for Phase-1 types and a companion type descriptor
(§3.1).

**DoD:**
- `Value` variants for Phase-1 types: `Null`, `Bool`, `Int2/4/8`, `Float4/8`,
  `Numeric` (bigdecimal), `Text`, `Bytea`.
- A `PgType` descriptor (base type + typmod slot) with the correct **type OID**
  for each Phase-1 type, plus a name↔OID mapping.
- Text I/O per type: parse a literal into a `Value` and render a `Value` to its
  Postgres text form, for the Phase-1 types (used by INSERT literals and result
  rendering).
- Unit tests for parse/render round-trips and OID lookups; numeric parsing
  covers precision beyond `i64`/`f64`.

**Notes:** `Value` is self-describing; typmod lives in the catalog, not in
`Value`. Coercion between types is deferred to task 21. Temporal/uuid/json are
out of Phase 1.

### Task 4 — `sqlparser` wrapper & statement dispatch [COMPLETE]
**Goal:** A thin parsing layer that produces owned ASTs and a dispatch skeleton
routing statements to (stub) handlers.

**DoD:**
- A `parse(sql) -> Result<Vec<Statement>>` function using the PostgreSQL dialect,
  serialized behind the global mutex and releasing it promptly (§2.1).
- Parse errors map to a `PgError` with a syntax-error `SqlState`.
- A statement-dispatch enum/function that classifies each parsed statement
  (DDL / DML / SELECT / transaction-control / SET / unsupported) and routes to
  handler stubs returning "not implemented" for now.
- Unit tests: valid SQL parses; invalid SQL yields a syntax-error `PgError`;
  dispatch classifies representative statements correctly.

**Notes:** No execution yet. The unsupported-feature policy (§10) is wired in
later; for now unknown statements return a clear "not implemented" error.

---

## Milestone B — Storage & MVCC core

### Task 5 — Catalog structures [COMPLETE]
**Goal:** In-memory catalog data structures for schemas, tables, and columns
(§4).

**DoD:**
- Types for `Schema`, `TableSchema` (name, columns, constraints placeholder,
  `TableId`), and `ColumnDef` (name, `PgType`, nullability, default placeholder).
- A `Catalog` supporting create/drop/lookup of tables by name (single default
  schema `public` is sufficient for Phase 1).
- Duplicate-table and missing-table operations return the correct `PgError`
  codes (`42P07` duplicate_table, `42P01` undefined_table).
- Unit tests for create/lookup/drop and the error cases.

**Notes:** The catalog is a simple mutable structure for Phase 1; transactional
DDL (MVCC-versioned catalog) is Phase 3 (§4). Constraint enforcement is added in
Milestone F; here the structures just hold constraint metadata.

### Task 6 — MVCC table storage (`RowId`, version chains) [COMPLETE]
**Goal:** The per-table row store with version chains (§3.2, §3.3, §5.1).

**DoD:**
- `RowId` (monotonic) and `Version { xmin, xmax, row }` types; a `VersionChain`
  per row.
- `Table { schema, rows: BTreeMap<RowId, VersionChain>, next_rowid }` behind a
  storage abstraction (so the map can be swapped later).
- Low-level operations independent of visibility: append an insert version,
  set `xmax` (tombstone), append an update version — each returning the affected
  `RowId`.
- Unit tests constructing chains directly and asserting their shape after
  insert/update/delete primitives.

**Notes:** No transaction/visibility logic yet — this task is the raw store.
`Xid` values used here are provided by the caller (task 7 supplies real ones).

### Task 7 — Transaction manager (`Xid`, `CommitSeq`, commit log) [COMPLETE]
**Goal:** Transaction identity, commit ordering, and status tracking (§5.2).

**DoD:**
- `Xid` allocation (monotonic) and a `CommitSeq` counter.
- A commit log / status map recording per-`Xid` state: in-flight, committed
  (with `CommitSeq`), or aborted.
- Operations: begin (allocate `Xid`), commit (assign `CommitSeq`, mark
  committed), abort (mark aborted).
- Unit tests for state transitions and commit-order assignment.

**Notes:** Wiring to the storage engine (stamping versions on commit) is refined
in Milestone G; here the manager is a standalone bookkeeping unit.

### Task 8 — Snapshot & visibility function [COMPLETE]
**Goal:** The single visibility function that all reads use (§5.3).

**DoD:**
- A `Snapshot` type (commit-order boundary + set of in-flight `Xid`s at snapshot
  time) with a constructor that captures the current state from the transaction
  manager.
- `is_visible(version, snapshot, current_xid) -> bool` implementing the two-rule
  test (xmin visible/mine; xmax absent or not-visible/not-mine).
- A helper that, given a `VersionChain` and a snapshot, returns the single
  visible `Version` (or none).
- Unit tests covering: own uncommitted insert visible to self, invisible to
  others; committed-before-snapshot visible; committed-after-snapshot invisible;
  deleted-by-committed invisible; deleted-by-in-flight still visible.

**Notes:** This is the correctness heart of MVCC; test it exhaustively with
hand-built chains and snapshots before it is used by the executor.

---

## Milestone C — Walking skeleton & measurement

### Task 9 — `Db` / `Session` API skeleton (autocommit) [COMPLETE]
**Goal:** The public entry points wired end-to-end, running each statement in an
implicit autocommit transaction (§8).

**DoD:**
- `Db::new()`, `db.session() -> Session`.
- `Session::execute(sql) -> Result<u64>` (affected rows) and
  `Session::query(sql, params) -> Result<QueryResult>` — params accepted but may
  be empty for now (binding lands in task 31).
- `QueryResult { columns: Vec<ColumnMeta>, rows: Vec<Vec<Value>> }` with a
  minimal `ColumnMeta { name, type_oid, typmod }`.
- Autocommit path: begin a transaction (task 7), take a snapshot (task 8),
  execute via dispatch (task 4), commit on success / abort on error.
- Storage guarded by the single `Mutex` (the `Condvar` is added in Milestone G).
- Unit test: a not-yet-implemented statement returns a clean error through the
  public API.

**Notes:** No statements execute for real yet (tasks 10–12 fill them in). The
one big lock is introduced here as a plain `Mutex`; task 29 upgrades usage to the
`Mutex`+`Condvar` monitor (§6).

### Task 10 — `CREATE TABLE` / `DROP TABLE` [COMPLETE]
**Goal:** Execute basic DDL against the catalog.

**DoD:**
- `CREATE TABLE name (col type [NOT NULL] [PRIMARY KEY] [UNIQUE] [DEFAULT ...]
  [CHECK ...], ...)` parses and records full column + constraint **metadata**
  (enforcement of constraints arrives in Milestone F; DEFAULT stored but not yet
  applied).
- `DROP TABLE [IF EXISTS] name`.
- Supported Phase-1 column types accepted; unknown types error (`42704`
  undefined_object / feature-not-supported as appropriate).
- Duplicate/missing table errors match Postgres codes.
- Differential harness not yet available; cover with unit tests asserting the
  resulting catalog state and error codes.

**Notes:** Only the default `public` schema. `IF NOT EXISTS` handling included.

### Task 11 — `INSERT` (literal rows, exact types) [COMPLETE]
**Goal:** Insert literal rows into a table.

**DoD:**
- `INSERT INTO t (cols...) VALUES (...), (...)` and `INSERT INTO t VALUES (...)`.
- Column/value count and (exact) type checks; mismatches error with the right
  code. All non-defaulted columns must be provided (DEFAULT/omitted columns land
  in task 24).
- Rows are inserted as new versions under the statement's `Xid` (tasks 6–8),
  visible after the autocommit commit.
- Returns the affected-row count.
- Unit tests: insert then read back via a direct visibility query; type-mismatch
  and arity errors.

**Notes:** Coercion is exact-only for now (task 21 relaxes). No `RETURNING`
(Phase 2). No constraint enforcement yet.

### Task 12 — `SELECT *` full scan + projection [COMPLETE]
**Goal:** Read rows back with column projection.

**DoD:**
- `SELECT * FROM t` and `SELECT col_a, col_b FROM t` (bare column references,
  no expressions/WHERE yet).
- Full scan of the table applying the visibility function against the
  statement's snapshot.
- `QueryResult` populated with correct `ColumnMeta` (names, OIDs) and rows in
  `BTreeMap`/`RowId` order.
- Unknown column/table errors match Postgres codes.
- Unit tests: projection order and column metadata; visibility (inserted rows
  appear, others don't).

**Notes:** This completes the first end-to-end path: `CREATE` → `INSERT` →
`SELECT`. Expressions, `WHERE`, ordering, and limits arrive in Milestone D.

### Task 13 — Differential test harness (vs real Postgres 18) [COMPLETE]
**Goal:** The primary correctness oracle: run SQL scripts against real Postgres
18 and `pg_fake` and compare (§12).

**DoD:**
- A test helper that spins up Postgres 18 (via `testcontainers` or a documented
  local instance) and executes a script against both engines.
- Result comparison as **multisets** by default (order-independent), with an
  opt-in ordered comparison for `ORDER BY` cases; comparison respects the
  fidelity tiers (compare values + `SQLSTATE` on error; ignore message text).
- A concise way to author cases (SQL script + expectations) and run them under
  `cargo test`.
- The Milestone C path (`CREATE`/`INSERT`/`SELECT *`) is covered by at least a
  few differential cases that pass.
- Documentation on running the harness (and how to skip it when Postgres is
  unavailable, e.g. behind a feature flag / ignored-by-default).

**Notes:** From here on, every feature task adds differential cases to its DoD.

### Task 14 — Benchmark harness (vs real Postgres 18) [COMPLETE]
**Goal:** Validate the core premise — `pg_fake` is dramatically faster than real
Postgres for the same workload — and keep it measurable over time.

**DoD:**
- A benchmark suite (e.g. `criterion`) measuring representative workloads
  available so far (`CREATE`/`INSERT`/`SELECT`), comparing `pg_fake` in-process
  calls against the same queries over a real Postgres 18 connection.
- Reports per-operation latency/throughput for both, with a clear speedup ratio.
- Documented how to run it and interpret results; results captured in the README
  or a `benches/README`.
- A guard/threshold or at least a documented expectation that `pg_fake` is at
  least an order of magnitude faster on these workloads.

**Notes:** The harness is infrastructure; later feature tasks extend it with new
workloads (notably mutations after tasks 18–19 and explicit transactions after
task 27).

---

## Milestone D — Expressions, filtering, mutations & basic transactions

### Task 15 — Expression evaluator: literals, column refs, arithmetic, comparisons [COMPLETE]
**Goal:** Evaluate scalar expressions over a row (§3.1).

**DoD:**
- Evaluate literals, column references, arithmetic (`+ - * / %` with Phase-1
  numeric types), and comparison operators (`= <> < <= > >=`).
- Arithmetic errors match Postgres: division by zero (`22012`), integer overflow
  (`22003`).
- Used in `SELECT` projection lists (e.g. `SELECT a + 1, a > b FROM t`).
- Differential cases for arithmetic/comparison results and error codes.

**Notes:** Cross-type arithmetic uses exact/native combinations only until the
coercion module (task 21). Boolean logic and NULL propagation are refined in
task 16.

### Task 16 — Three-valued logic & NULL semantics [COMPLETE]
**Goal:** Correct SQL NULL behavior across operators (§1.3, §3.1).

**DoD:**
- `NULL` propagation through arithmetic and comparisons (result `NULL`).
- `AND` / `OR` / `NOT` implement three-valued logic (true/false/unknown).
- `IS NULL` / `IS NOT NULL`, `IS TRUE/FALSE/UNKNOWN`, and `IS DISTINCT FROM`.
- Differential cases covering the truth tables and NULL comparisons.

**Notes:** This is Tier A behavior; be exhaustive. Depends on task 15's operator
set.

### Task 17 — `WHERE` filtering [COMPLETE]
**Goal:** Filter rows in `SELECT` by a boolean predicate.

**DoD:**
- `SELECT ... FROM t WHERE <predicate>` keeps only rows where the predicate is
  true (unknown/false excluded), using tasks 15–16.
- Works together with projection and visibility.
- Differential cases combining predicates, NULLs, and arithmetic.

**Notes:** No joins/subqueries (Phase 2). Prepares the ground for
`UPDATE`/`DELETE` targeting.

### Task 18 — `UPDATE` [COMPLETE]
**Goal:** Modify existing rows (§5.1).

**DoD:**
- `UPDATE t SET col = expr [, ...] [WHERE <predicate>]`.
- Matching rows get their current version's `xmax` set and a new version
  appended, under the statement's `Xid` (MVCC update).
- `SET` expressions evaluated per matched row (tasks 15–16); returns
  affected-row count.
- Differential cases (with/without `WHERE`, expressions in `SET`), plus a
  benchmark workload added to task 14's suite.

**Notes:** Exact types only until task 21. No `FROM`/join in `UPDATE` (Phase 2).
Constraint checks arrive in Milestone F. Task 27 validates updates inside
explicit transactions.

### Task 27 — Explicit transactions + aborted-state machine [COMPLETE]
**Goal:** User-driven transaction boundaries for the available `INSERT`,
`SELECT`, and `UPDATE` operations, with API-level correctness coverage (§5.7).

**DoD:**
- `BEGIN` / `START TRANSACTION`, `COMMIT`, and `ROLLBACK` via SQL, plus
  `Session::begin()` returning a `Transaction` RAII guard (drop = rollback);
  both drive one shared session state machine (§8).
- A statement issued while the session has an active explicit transaction reuses
  its `Xid`; it does not enter the implicit-autocommit path. `BEGIN` itself also
  bypasses that path.
- An active transaction reads its own inserted and updated versions. A separate
  session cannot read them before commit, and can read them after commit.
- `ROLLBACK` marks the transaction aborted so its inserted and updated versions
  are invisible; physical cleanup remains deferred to garbage collection.
- An execution error inside a transaction poisons it: subsequent ordinary
  statements raise `25P02`; `ROLLBACK` restores the session, and `COMMIT` of a
  poisoned transaction aborts it.
- DDL inside an explicit transaction is rejected as unsupported until
  transactional catalog changes are implemented in Phase 3; it must not leak an
  unrollbackable catalog change.
- Public-API integration tests use only `Db`, `Session`, `Transaction`,
  `execute`, and `query` to verify own-write visibility, cross-session
  invisibility before commit, commit visibility, rollback invisibility,
  multi-statement transactions, unchanged autocommit behavior, poisoned state,
  and RAII rollback on drop.
- The differential harness supports ordered operations on named sessions. A
  two-session Postgres-18 case covers an uncommitted insert and update,
  visibility after commit, and invisibility after rollback.
- A transaction benchmark workload is added to task 14's suite.

**Notes:** This task uses the existing default snapshot policy only. Isolation
level selection, READ COMMITTED versus REPEATABLE READ behavior, row locking,
and deadlock detection remain tasks 28–30.

### Task 19 — `DELETE` [COMPLETE]
**Goal:** Remove rows (§5.1).

**DoD:**
- `DELETE FROM t [WHERE <predicate>]`.
- Matching rows get their current version tombstoned (`xmax` set); returns
  affected-row count.
- Deleted rows disappear from subsequent snapshots; visibility for concurrent
  readers is correct (validated more in Milestone G).
- Differential cases and a benchmark workload added to task 14's suite.

**Notes:** Completes CRUD. `TRUNCATE` is out of Phase 1.

---

## Milestone E — Query features

### Task 20 — `CASE` + common scalar functions [COMPLETE]
**Goal:** Conditional expressions and a starter set of scalar functions.

**DoD:**
- Searched and simple `CASE` expressions with correct type/NULL handling.
- `COALESCE`, `NULLIF`, `GREATEST`, `LEAST`, `length(text)`, `lower(text)`,
  `upper(text)`, and `abs(int2/int4/int8/float4/float8/numeric)`.
- Differential cases for each function/`CASE` including NULL inputs.

**Notes:** The function set grows over phases; Phase 1 covers the common ones.
Unknown functions error via the unsupported-feature policy (§10).

### Task 21 — Type coercion module (implicit / assignment / explicit) [COMPLETE]
**Goal:** Central coercion mirroring Postgres cast rules (§11).

**DoD:**
- One coercion module implementing the three cast contexts: implicit (in
  expressions), assignment (INSERT/UPDATE into a column), explicit (`CAST(x AS
  t)` and `x::t`), for Phase-1 types.
- Coercion table modeled on Postgres type categories/preferred types for the
  Phase-1 type set.
- Executor/INSERT/UPDATE updated to route through this module (relaxing the
  exact-type restrictions from tasks 11/18).
- Invalid casts error with the right code (`22P02`, `42846` cannot_cast, etc.).
- Differential cases covering implicit promotions (e.g. `int + numeric`),
  assignment casts, explicit casts, and rejected coercions.

**Notes:** Rules are mirrored from Postgres, not invented. Retrofitting here is
intentional so earlier tasks stay small.

### Task 22 — `ORDER BY` [COMPLETE]
**Goal:** Deterministic result ordering on request.

**DoD:**
- `ORDER BY expr [ASC|DESC] [NULLS FIRST|LAST] [, ...]`, using Postgres default
  null ordering (NULLS LAST for ASC, FIRST for DESC) unless specified.
- Ordering by column, expression, and output position; correct type-aware
  comparison for Phase-1 types.
- Differential cases using **ordered** comparison (task 13) including NULL
  placement and multi-key sorts.

**Notes:** Collation is default/byte-ish for Phase 1; locale-aware collation is
out of scope.

### Task 23 — `LIMIT` / `OFFSET` [COMPLETE]
**Goal:** Row-count limiting and skipping.

**DoD:**
- `LIMIT n` and `OFFSET m` (and combined), applied after `ORDER BY`.
- Correct interaction with `ORDER BY` (deterministic) and without it (documented
  as Tier C order).
- Differential cases with and without `ORDER BY`.

**Notes:** `FETCH ... ROWS` syntax optional; `LIMIT/OFFSET` is the Phase-1
target.

---

## Milestone F — Constraints

### Task 24 — `NOT NULL` + column `DEFAULT` [COMPLETE]
**Goal:** Enforce not-null and apply defaults (§4).

**DoD:**
- `NOT NULL` violations on INSERT/UPDATE raise `23502` (not_null_violation).
- `DEFAULT <expr>` applied when a column is omitted from INSERT or set to
  `DEFAULT`; constant and simple-expression defaults evaluated correctly.
- INSERT relaxed so omitted defaulted/nullable columns are allowed (completing
  task 11's deferral).
- Differential cases for not-null errors and default application.

**Notes:** Sequence-backed defaults (`SERIAL`) are Phase 2.

### Task 25 — `PRIMARY KEY` / `UNIQUE` (+ index structures)
**Goal:** Enforce uniqueness via index structures (§3.2).

**DoD:**
- Unique/primary-key indexes as `value(s) -> RowId` structures maintained on
  INSERT/UPDATE/DELETE and respecting MVCC visibility (a value is "taken" only by
  a version visible/committed per Postgres rules).
- Duplicate-key violations raise `23505` (unique_violation); primary key implies
  NOT NULL + unique.
- Multi-column keys supported.
- Differential cases: duplicate inserts, PK updates that (a) conflict and (b)
  succeed, and uniqueness across delete+reinsert.

**Notes:** Uniqueness must interact correctly with concurrent transactions; the
concurrency edge cases are further validated in Milestone G.

### Task 26 — `CHECK`
**Goal:** Enforce check constraints.

**DoD:**
- `CHECK (<predicate>)` (column- and table-level) evaluated on INSERT/UPDATE;
  violations raise `23514` (check_violation).
- Postgres semantics: a CHECK that evaluates to unknown (NULL) passes.
- Differential cases including NULL-yielding checks.

**Notes:** Uses the expression evaluator (tasks 15–16). Foreign keys are Phase 2.

---

## Milestone G — Transactions & concurrency

### Task 28 — Isolation levels (READ COMMITTED + REPEATABLE READ)
**Goal:** Per-statement vs per-transaction snapshots and level selection (§5.4).

**DoD:**
- READ COMMITTED (default): a fresh snapshot per statement. REPEATABLE READ:
  snapshot frozen at the transaction's first statement.
- Level selection resolution order: `BEGIN TRANSACTION ISOLATION LEVEL` /
  `SET TRANSACTION`, session default (`SET SESSION CHARACTERISTICS`), DB default;
  level fixed after the first statement (changing it after errors).
- `begin_with(IsolationLevel)` on the API.
- Differential cases demonstrating the RC/RR read difference across a concurrent
  commit (driven single-threaded via interleaved sessions).

**Notes:** Write-conflict serialization errors (`40001`) are exercised together
with task 29. SERIALIZABLE is Phase 3.

### Task 29 — Row locking + write-write blocking + lock timeout
**Goal:** True blocking on concurrent writes, with a lock timeout (§5.5, §6).

**DoD:**
- The storage lock is used as a `Mutex` + `Condvar` monitor: waiting for a row
  lock releases the big lock and blocks; commit/abort notifies waiters.
- A row-lock manager keyed by `(TableId, RowId)` with wait queues; `UPDATE` /
  `DELETE` / `SELECT ... FOR UPDATE` / `FOR SHARE` acquire row locks.
- Post-wait behavior matches Postgres: after the holder commits, READ COMMITTED
  re-reads and proceeds (or row gone); REPEATABLE READ raises `40001`.
- Configurable lock timeout (default 1s; `SET lock_timeout`, `0` = wait forever)
  raising `55P03` on expiry.
- Multi-threaded tests: one thread blocks on another's locked row and proceeds
  after commit; a lock timeout fires as expected.

**Notes:** Deadlock detection is the next task; until then, cyclic waits rely on
the timeout as a backstop.

### Task 30 — Deadlock detection
**Goal:** Detect cyclic waits and abort a victim (§5.5).

**DoD:**
- A wait-for graph over blocked transactions; cycles are detected (on wait
  and/or periodically).
- On a cycle, a victim is chosen and its statement raises `40P01`
  (deadlock_detected); the victim's transaction becomes abortable.
- Multi-threaded test constructing a two-transaction deadlock reliably produces
  `40P01` for exactly one transaction; the other proceeds.

**Notes:** Victim-selection policy is documented (Tier C which transaction is
chosen). Completes the concurrency milestone.

---

## Milestone H — API completeness

### Task 31 — Parameters & prepared statements
**Goal:** First-class parameter binding and prepared statements (§8).

**DoD:**
- `$1`-style placeholders bound from `&[Value]` in `query`/`execute`.
- `Session::prepare(sql) -> Statement` and `query_prepared`/`execute_prepared`,
  parsing/analyzing once and executing many with different params.
- Parameterized calls are single-statement (mirroring the extended protocol);
  parameter type inference/checking matches Postgres where feasible.
- Differential cases: parameterized queries produce identical results to the
  inlined-literal form.

**Notes:** No parse cache (spec §2.1); `prepare()` is the parse-once path.

### Task 32 — Multi-statement `execute` + `QueryResult` metadata
**Goal:** Simple-query multi-statement support and complete result metadata
(§8).

**DoD:**
- `execute` accepts multiple `;`-separated statements, running them in order and
  returning a result per statement (mirrors the simple query protocol);
  parameterized calls remain single-statement.
- `ColumnMeta` fully populated (name, type OID, typmod) for all Phase-1 types.
- Differential cases: a multi-statement batch (e.g. a small migration script) and
  column-metadata checks.

**Notes:** Finalizes the native API surface that the driver crate builds on.

---

## Milestone I — Driver

### Task 33 — `sqlx` driver crate (`pg_fake_sqlx`)
**Goal:** Let existing `sqlx` application code run against the in-process fake
(§8.1).

**DoD:**
- A separate crate implementing the `sqlx` `Database`/driver traits on top of the
  core native API; depends on core, core does not depend on it.
- Async adaptation of the synchronous core (e.g. `spawn_blocking`), correctly
  handling blocking row-lock waits.
- Type mapping between `Value`/`PgType` and `sqlx` Postgres types for Phase-1
  types; `PgError` mapped to `sqlx::Error` preserving `SQLSTATE`.
- Transactions and prepared statements usable through the `sqlx` API.
- An example test using `sqlx` queries against `pg_fake`, plus a differential
  case run through the `sqlx` layer to confirm error categories match.

**Notes:** Other drivers or a wire-protocol server are future work (spec §13).

---

## Phase 1 Exit Criteria
- All 33 tasks meet their DoD.
- The differential suite passes for the full Phase-1 SQL surface against
  Postgres 18.
- The benchmark suite shows `pg_fake` at least an order of magnitude faster than
  real Postgres on the covered workloads.
- `sqlx` application code can run CRUD + transactions against `pg_fake` with
  matching results and error categories.
