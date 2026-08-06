# pg_fake — Design Specification

## 1. Goal & Non-Goals

### 1.1 Goal
`pg_fake` is an in-memory, embeddable fake of PostgreSQL for use as a test
double in automated tests. Given the same sequence of SQL statements, `pg_fake`
produces the same observable results as a real PostgreSQL server, so tests
written against it behave as they would against Postgres — but orders of
magnitude faster.

- Pure Rust library; no external process, no networking. The API is plain
  in-process function calls.
- SQL is parsed with `sqlparser-rs`.
- Multiple queries / transactions can be in flight concurrently.
- Transactional semantics match PostgreSQL.
- Database snapshots are cheap, for use as test fixtures.

The reference implementation target is **PostgreSQL 18**. All fidelity claims
and differential tests are measured against Postgres 18 behavior.

### 1.2 Non-Goals
- Not a production database: no durability, crash recovery, on-disk WAL, or
  replication.
- Not a wire-protocol server. The core is function-call only. (A wire server is
  possible future work; see §12.)
- Not optimized for large datasets; test datasets are assumed small.

### 1.3 Fidelity Contract
Full bit-for-bit fidelity with Postgres is unbounded work, so observable
behavior is grouped into three tiers.

**Tier A — guaranteed to match** (tests may rely on this):
- result *sets*, compared as multisets, for supported SQL;
- NULL / three-valued logic;
- supported type coercions;
- whether a constraint violation occurs (unique, not-null, foreign key, check);
- transaction visibility and isolation outcomes;
- sequence value allocation;
- **`SQLSTATE` error codes**: for any error reproducible in Postgres and in
  scope (i.e. not caused by the filesystem, OS, or other out-of-scope
  subsystems), `pg_fake` returns the same error class/code.

**Tier B — best effort** (may match; tests should not depend on it):
- exact error *message text*;
- numeric and floating-point text formatting;
- `now()` / clock values (subject to the time control in §1.4).

**Tier C — explicitly not guaranteed:**
- row order in the absence of `ORDER BY` (Postgres does not guarantee this
  either);
- physical planning, `EXPLAIN` output, and performance;
- `ctid` values.

`sqlx` is the expected primary driver, and error **category** fidelity is
verified through it. Error-code fidelity is nonetheless defined at the
SQL/`SQLSTATE` level, independent of any specific driver.

### 1.4 Time Control
Time-dependent behavior can be made deterministic:
- A DB-initialization flag selects mock time. With mock time off, the DB uses
  the real system clock.
- The public API for time is exactly `db.set_time(t)` and
  `db.advance_time(duration)`, valid when mock time is enabled. There is no
  public clock type; the clock abstraction is internal.
- With mock time on, the clock is frozen and moves only when `set_time` /
  `advance_time` is called.
- The Postgres timestamp hierarchy is honored: `now()` /
  `transaction_timestamp()` are captured at transaction start and stay constant
  for the transaction; `statement_timestamp()` is captured at statement start;
  `clock_timestamp()` reads the clock live.

---

## 2. High-Level Architecture

```
                +-----------------------------------------------+
                |                  pg_fake DB                   |
                |                                               |
   caller  ---> |  Session / Connection  (one per "client")    |
 (test code)    |     - current transaction state              |
                |     - snapshot(s)                             |
                +----------------------+------------------------+
                                       |
                +----------------------v------------------------+
                |  Parser (sqlparser-rs)                        |
                +----------------------+------------------------+
                                       |
                +----------------------v------------------------+
                |  Analyzer / Binder  (name & type resolution)  |
                +----------------------+------------------------+
                                       |
                +----------------------v------------------------+
                |  Executor  (tree-walking interpreter)         |
                +----------------------+------------------------+
                                       |
        +------------------------------+------------------------------+
        |                Storage Engine                               |
        |   - Catalog (schemas, tables, columns, constraints, seqs)   |
        |   - Tables: RowId -> version chain (MVCC)                    |
        |   - Commit log / CommitSeq (commit ordering)                |
        |   - Visibility / snapshot logic                             |
        |   - Row-lock manager + deadlock detection                   |
        |   - Version garbage collection                              |
        +-------------------------------------------------------------+
```

### 2.1 Components
- **Session** — the unit a caller interacts with; analogous to a Postgres
  connection/backend. Holds the current transaction (if any) and its snapshot.
  Statements issued outside an explicit transaction run as implicit
  (autocommit) single-statement transactions.
- **Parser** — a thin wrapper over `sqlparser-rs`. Each parse creates an
  independent parser and returns owned ASTs. No parse/plan cache: the explicit
  `prepare()` path (§8) covers parse-once/execute-many, and a plan cache would
  carry invalidation complexity under transactional DDL.
- **Analyzer/Binder** — resolves identifiers against the catalog, checks and
  infers types, applies coercions, and produces a bound logical plan.
- **Executor** — interprets the bound plan against the storage engine.
- **Storage engine** — owns all data and the MVCC machinery.

---

## 3. Data Model

### 3.1 Value Type
A single Rust enum `Value` represents any cell value.

| Postgres type(s) | `Value` variant | Backing | Phase |
|---|---|---|---|
| `null` (any type) | `Null` | — | 1 |
| `boolean` | `Bool(bool)` | std | 1 |
| `smallint`/`int2` | `Int2(i16)` | std | 1 |
| `integer`/`int4` | `Int4(i32)` | std | 1 |
| `bigint`/`int8` | `Int8(i64)` | std | 1 |
| `real`/`float4` | `Float4(f32)` | std | 1 |
| `double precision`/`float8` | `Float8(f64)` | std | 1 |
| `numeric`/`decimal` | `Numeric(...)` | `bigdecimal` (arbitrary precision) | 1 |
| `text`/`varchar(n)`/`char(n)`/`bpchar` | `Text(String)` | std (typmod in catalog) | 1 |
| `bytea` | `Bytea(Vec<u8>)` | std | 1 |
| `uuid` | `Uuid(...)` | `uuid` | 2 |
| `date` | `Date(...)` | `chrono` (wrapped) | 2 |
| `time` | `Time(...)` | `chrono` (wrapped) | 2 |
| `timestamp` | `Timestamp(...)` | `chrono` (wrapped) | 2 |
| `timestamptz` | `TimestampTz(...)` | `chrono` (wrapped) | 2 |
| `interval` | `Interval(...)` | custom (months/days/micros) | 2 |
| `json` | `Json(...)` | `serde_json` (keeps raw text) | 3 |
| `jsonb` | `Jsonb(...)` | `serde_json` (normalized) | 3 |
| arrays `T[]` | `Array { elem_type, Vec<Value> }` | std | 3 |

- `numeric` uses `bigdecimal` (arbitrary precision), matching Postgres `numeric`
  rather than a fixed-width decimal.
- Temporal types use `chrono`, **wrapped** in an internal layer so PG-specific
  behavior (microsecond truncation, `infinity`, BC dates, session `TimeZone` for
  `timestamptz`) is handled here rather than delegated. `interval` is a custom
  type (separate months / days / microseconds fields; a PG interval is not a
  single duration).
- `Value` is self-describing (each variant carries its base type). The declared
  column type, including typmod (`varchar(n)`, `numeric(p,s)`), lives in the
  catalog; coercion and validation against the declared type happen at write
  time and within expressions. `Value` does not store typmod.
- `NULL` is a single variant, not per-type. Three-valued logic is applied
  consistently across all operators.
- Phasing: integers/floats/numeric/text/bytea/bool in Phase 1; uuid and
  temporal in Phase 2; json/jsonb/arrays in Phase 3 (see §9).

### 3.2 Row & RowId
- A **Row** is a `Vec<Value>` positionally aligned with the table's column list.
- Rows are identified by an internal, monotonically increasing **`RowId`** (an
  analog of Postgres `ctid` / tuple identity), assigned at insert and stable for
  the row's lifetime, including across updates.
- Primary and unique keys are enforced by **separate index structures**
  (`value(s) -> RowId`), never by using key values as the map key. This keeps
  tables without a primary key, duplicate rows, and primary-key updates correct,
  and gives each row an identity independent of its column values (required by
  MVCC). Point lookups by key go through an index (`value -> RowId -> Row`).

### 3.3 Table Storage
```
Table {
    schema:      TableSchema,                 // columns, types, constraints
    rows:        BTreeMap<RowId, VersionChain>,
    indexes:     Vec<Index>,                  // unique enforcement / lookup
    next_rowid:  RowId,
}
```
The row map is a `BTreeMap<RowId, VersionChain>`. Because `RowId` is monotonic,
iteration is in insertion order, which is deterministic and reduces test
flakiness for queries without `ORDER BY`. The map type sits behind an internal
abstraction so it can be swapped for a persistent map later if snapshot cost
warrants it (see §7).

---

## 4. Catalog

The catalog holds metadata: schemas, tables, columns (name, type, nullability,
default expression), constraints (primary key, unique, foreign key, check,
not-null), sequences, and (later) views. DDL statements mutate the catalog.

- DDL is **transactional**, as in Postgres: `CREATE` / `ALTER` / `DROP` inside a
  transaction roll back on `ROLLBACK`, and a transaction sees its own uncommitted
  schema changes. The catalog is modeled as an MVCC-versioned structure — catalog
  entries carry the same `xmin`/`xmax` visibility as row versions (§5) — so
  transactional DDL reuses the row-version machinery rather than a separate
  rollback mechanism. (Implementation is staged to a later phase; see §9.)
- Sequences (`SERIAL`, `GENERATED ... AS IDENTITY`) allocate values that are
  **not** rolled back on abort, matching Postgres. Sequence state lives outside
  transactional visibility rules.

---

## 5. Transactions & MVCC

Concurrency and transactional correctness are built on multi-version concurrency
control (MVCC) with per-row version chains.

### 5.1 Version Model
Rows are never modified in place; each change appends or retires versions. Each
table maps `RowId -> version chain`, where a version is:
```
Version {
    xmin: Xid,             // transaction that CREATED this version
    xmax: Option<Xid>,     // transaction that deleted/superseded it (None = live)
    row:  Row,
}
```
A version's lifetime is `[xmin committed, xmax committed)`.

Operations map to versions as follows:
- `INSERT` — a new `RowId` with one version `{ xmin = me, xmax = None }`.
- `DELETE` — set the visible version's `xmax = me` (a tombstone; no new version).
- `UPDATE` — set the old version's `xmax = me` and append a new version
  `{ xmin = me, xmax = None }` with the new values.

### 5.2 Commit Ordering
Each transaction has an `Xid`. A global `CommitSeq` counter increments on each
commit; committing a transaction stamps all its versions as committed at a
single `CommitSeq`, giving an atomic visibility flip. A commit log / status map
records, per `Xid`, whether it is in-flight, committed (with its `CommitSeq`), or
aborted.

On commit, a transaction's in-flight versions (including tombstone `xmax`
stamps) become visible atomically at the assigned `CommitSeq`. On rollback, the
transaction is marked aborted; its versions never pass visibility and are
reclaimed by garbage collection.

### 5.3 Snapshots & Visibility
A **snapshot** is a commit-order boundary plus the set of `Xid`s still in-flight
at snapshot time.

A version is visible to a snapshot if and only if:
1. `xmin` is the current transaction, or `xmin` is committed and within the
   snapshot; **and**
2. `xmax` is `None`, or `xmax` is neither the current transaction nor
   committed-within-the-snapshot.

For any `RowId`, at most one version passes for a given snapshot — that is the
row read. This single visibility function underlies all isolation levels.

### 5.4 Isolation Levels
All three standard Postgres isolation levels are supported by design; the
visibility mechanism accepts a snapshot taken at an arbitrary commit point and
is reusable for both per-statement and per-transaction snapshots. Data
structures retain enough information (including what a transaction read) to
attach read/write predicate tracking for SERIALIZABLE without redesign.

- **READ COMMITTED** (default) — each statement sees a fresh snapshot of data
  committed before that statement started; a snapshot is taken **per statement**.
- **REPEATABLE READ** — the snapshot is frozen at the transaction's first
  statement; a snapshot is taken **per transaction**. Write conflicts raise
  `40001` serialization failures.
- **SERIALIZABLE** — as REPEATABLE READ, plus SSI-style predicate / read-write
  dependency tracking to catch write-skew.

**Level selection** follows Postgres resolution order:
1. Explicit on the transaction: `BEGIN TRANSACTION ISOLATION LEVEL ...` or
   `SET TRANSACTION ISOLATION LEVEL ...` (valid only before the first statement).
2. Session default: `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION
   LEVEL ...`.
3. DB/server default (`default_transaction_isolation`), defaulting to
   READ COMMITTED.

The level is fixed once the transaction's first statement runs; changing it
afterward is an error. Autocommit statements run as single-statement
transactions at the session/server default level.

### 5.5 Write Conflicts & Locking
Concurrent writes to the same row are resolved by **true blocking**, matching
Postgres:
- `UPDATE` / `DELETE` / `SELECT ... FOR UPDATE` / `FOR SHARE` on a row locked by
  another transaction blocks the calling thread until that transaction commits
  or aborts.
- When the holder ends:
  - if it **aborted**, the waiter proceeds normally;
  - if it **committed**, under READ COMMITTED the waiter re-reads the new version
    and proceeds (or finds the row gone); under REPEATABLE READ / SERIALIZABLE it
    raises `40001 could not serialize access`.
- Cyclic waits are resolved by a **deadlock detector** (wait-for graph); a
  victim is chosen and raises `40P01 deadlock detected`.

Supporting machinery:
- A **row-lock manager** keyed by `(TableId, RowId)` with per-lock wait queues.
- A **wait-for graph** with deadlock detection.
- A **lock timeout** so a stuck or mis-written test fails fast rather than
  hanging forever. The default is **1 second** (the exact value is Tier C);
  it is configurable at DB init and via `SET lock_timeout` (including `0` for
  Postgres's wait-forever behavior). On expiry it raises `55P03`
  (`lock_not_available`). The timeout is the backstop for non-cyclic stuck
  waits; cyclic waits are caught faster by the deadlock detector (`40P01`).

### 5.6 Garbage Collection
Dead versions are reclaimed synchronously on commit:
- The **GC horizon** is the oldest snapshot boundary any active transaction
  still needs (the xmin horizon); if no transactions are active, it is the
  latest commit.
- For each `RowId`, versions invisible to every snapshot at or after the horizon
  are dropped (dead tombstones removed, superseded versions collapsed). When a
  single live version remains, it is effectively the base row.

GC runs while the storage lock is already held at commit time. It never affects
correctness (visibility already ignores dead versions); it is pure memory
reclamation and is entirely internal — there is no public vacuum API.

### 5.7 Statement & Transaction Control
Supported: `BEGIN` / `START TRANSACTION`, `COMMIT`, `ROLLBACK`, `SAVEPOINT` /
`ROLLBACK TO SAVEPOINT` / `RELEASE`, `SET TRANSACTION ISOLATION LEVEL`, and
implicit autocommit.

- **Savepoints / subtransactions** are supported by design and implemented in
  Phase 3. Version/transaction identity carries a subtransaction level, and the
  abort path operates per-subtransaction. A savepoint is a sub-snapshot marker;
  `ROLLBACK TO` marks sub-versions aborted (invisible) while keeping the outer
  transaction alive, reusing the visibility/abort machinery at finer
  granularity. (Required eventually because `sqlx` emits savepoints for nested
  transactions.)
- **Aborted-transaction state**: an error inside a transaction poisons it
  (`25P02`, "current transaction is aborted, commands ignored until end of
  transaction block"); this state machine is replicated. Rolling back to a
  savepoint recovers from the aborted state.

---

## 6. Concurrency Model

- The DB is `Send + Sync` and shared via `Arc<Db>`; each caller thread holds a
  `Session`.
- The storage engine is guarded by **one lock**, implemented as a `Mutex` paired
  with a `Condvar` (monitor pattern). When a statement must wait for a row lock
  held by another transaction, it calls `condvar.wait()`, which atomically
  releases the lock and blocks; on commit/abort a transaction releases its row
  locks and notifies the condvar, waking waiters to re-acquire and re-check.
  Holding the lock while waiting would deadlock, since the awaited transaction
  could never run.
- **Concurrency semantics:** multiple transactions may be in flight
  simultaneously (each session can hold an open transaction), but statement
  execution is serialized — one statement runs at a time. This provides
  concurrent transactions (needed for correct MVCC and blocking behavior) but
  not parallel execution. The engine sits behind an abstraction so fine-grained
  locking could replace the single lock if parallel throughput ever matters.
- The deadlock detector (§5.5) operates on the user-transaction wait-for graph;
  with a single lock there is no separate internal lock-ordering concern.

### 6.1 Determinism
There is no special deterministic execution mode or cooperative scheduler.
Instead the ingredients of determinism are guaranteed:
- a mockable clock (§1.4) makes time controllable;
- a seedable RNG makes `random()`, random UUIDs, etc. reproducible;
- `BTreeMap` storage gives deterministic scan order even without `ORDER BY`.

A driver that uses one thread / one session in a fixed order is fully
deterministic. Genuine multi-threaded transaction interleavings are
non-deterministic by design, matching real Postgres.

---

## 7. Snapshots (Fixtures)

A database can be built once and cheaply forked per test.

- **API:** `db.snapshot() -> Db` returns an independent, owned fork. With
  `BTreeMap` storage this is a deep clone, which is adequate for small fixtures;
  a persistent-map swap (see §3.3) is possible future work if snapshot cost
  matters.
- **Semantics:** a snapshot captures committed rows, the catalog/schema, and
  current sequence values. In-flight/uncommitted transactions are not captured;
  the fork starts with no open transactions.
- **Session state:** the fork carries no session state from the source (no open
  transactions, no session GUCs); its future sessions start clean.
- **Counters:** `RowId`, `CommitSeq`, and `Xid` counters carry over as-is; they
  only need to remain monotonic within the fork.

---

## 8. Public API

```rust
// Construction & fixtures
let db  = Db::new();                        // empty database
let db2 = db.snapshot();                    // independent fork (§7)

// A session ≈ a connection/backend.
let mut sess = db.session();

// One-shot (autocommit):
let results: Vec<StatementResult> =
    sess.execute("INSERT INTO t VALUES (1,'a')")?;
let rows: QueryResult =
    sess.query("SELECT id, name FROM t WHERE id = $1", &[Value::Int4(1)])?;

// Explicit transactions:
let mut tx = sess.begin()?;                 // or begin_with(IsolationLevel::RepeatableRead)
tx.execute("UPDATE t SET name='b' WHERE id=1")?;
tx.commit()?;                               // or tx.rollback(); drop = rollback

// Prepared / parameterized:
let stmt = sess.prepare("SELECT * FROM t WHERE id = $1")?;
let rows = sess.query_prepared(&stmt, &[Value::Int4(1)])?;
```

- **Parameters and prepared statements** are first-class. Parameterized calls
  (`$1`) are single-statement only, mirroring the Postgres extended protocol.
- **Result representation:** `StatementResult::Affected(u64)` represents a
  non-query result. `StatementResult::Query(QueryResult)` carries
  `QueryResult { columns: Vec<ColumnMeta>, rows:
  Vec<Row> }`, where `ColumnMeta` carries the column name, Postgres type OID, and
  typmod. `RETURNING` returns rows.
- **Multi-statement strings** (`"INSERT ...; INSERT ...;"`) are supported in
  `execute`, returning a result per statement, mirroring the Postgres simple
  query protocol.
- **Errors** use a single `PgError { sqlstate, message, detail?, hint?, position? }`.
  `sqlstate` is Tier A; `message` is Tier B.
- **Transaction control** is available through both `sess.begin()` (returning a
  `Transaction` RAII guard where drop without commit rolls back) and SQL
  (`sess.execute("BEGIN")` / `COMMIT` / `ROLLBACK`). Both drive the same session
  transaction state machine.
- **The core is synchronous.** The only blocking is row-lock waits via condvar.

### 8.1 Crate Layering
- **Core crate (`pg_fake`)** — the native API above. No wire protocol and no
  driver dependency. This is the engine used directly by unit and differential
  tests.
- **`sqlx` driver crate** (e.g. `pg_fake_sqlx`) — implements the `sqlx`
  `Database`/driver traits on top of the core API, so existing `sqlx`
  application code can run against the in-process fake without a socket. It
  depends on the core; the core never depends on it. Async lives entirely in
  this crate, which adapts the synchronous core (e.g. via `spawn_blocking`,
  since row-lock waits block the thread).

Support for other drivers would each require a similar adapter crate, or a
wire-protocol server (§12).

---

## 9. SQL Feature Scope

Features are delivered in ordered phases. Before building each phase it is
broken into smaller, trackable chunks with explicit acceptance criteria, and the
differential test suite (§11) — ideally seeded from a real corpus of application
SQL/migrations — drives prioritization within the phase, so work follows what
tests actually need in dependency order.

**Phase 1 (MVP):**
- DDL: `CREATE TABLE` (common types, `PRIMARY KEY` / `NOT NULL` / `UNIQUE` /
  `DEFAULT` / `CHECK`), `DROP TABLE`.
- DML: `INSERT`, `SELECT` (projection, `WHERE`, `ORDER BY`, `LIMIT`/`OFFSET`),
  `UPDATE`, `DELETE`.
- Expressions: comparisons, boolean logic (three-valued), arithmetic, common
  scalar functions, `CASE`.
- Transactions: `BEGIN` / `COMMIT` / `ROLLBACK`, READ COMMITTED and REPEATABLE
  READ, row locking, deadlock detection.
- Constraints enforced: NOT NULL, UNIQUE/PK, CHECK.

**Phase 2:**
- Joins (inner/left/right/full/cross), subqueries, `GROUP BY` and aggregates,
  `DISTINCT`, `HAVING`.
- Sequences / `SERIAL`, `RETURNING`.
- Foreign keys.
- Phase-2 types (uuid, temporal).

**Phase 3:**
- CTEs (including recursive), `INSERT ... ON CONFLICT`, window functions, views.
- Phase-3 types (json/jsonb, arrays).
- Savepoints, `SET` / session GUCs, `SELECT ... FOR UPDATE` variants.
- SERIALIZABLE isolation (SSI), transactional-DDL implementation.

Each phase ships with a conformance test suite run against real Postgres (§11).

---

## 10. Unsupported-Feature Policy

Because not all of Postgres is implemented at once, behavior for anything not
yet implemented is explicit and predictable, and distinguishes features a *user*
wrote from features a *driver* emits internally.

An explicit **registry** of unsupported / not-yet-supported features assigns
each entry a handling policy:
- **`Error`** — reject with an appropriate error, using the same `SQLSTATE`
  Postgres would use where applicable (e.g. `0A000 feature_not_supported`, or a
  syntax/undefined error where that is what Postgres returns). This is the
  default for user-facing features, so tests fail loudly rather than silently
  misbehaving.
- **`Tolerate`** (accept-and-ignore / no-op) — for driver-internal plumbing that
  a driver such as `sqlx` emits and that is safe to treat as a no-op or benign
  default (e.g. certain `SET`/session parameters, protocol-level statements). The
  statement succeeds so the driver keeps working, even though the feature is not
  fully modeled.

A **`restrict` (strict) flag**, off by default, turns tolerance off: when
enabled, every unsupported feature — including `Tolerate` ones — raises an error.
This is used to audit exactly what a driver/app relies on and to surface silent
gaps (e.g. in CI). With the flag off (the normal test-run default),
driver-internal features are tolerated while genuinely unsupported user-level
features still error.

---

## 11. Error & Type Fidelity

Fidelity is built incrementally and test-driven (against Postgres 18), with two
structural commitments so it lives in one place and grows coherently:
- a single `PgError` with a mandatory `SqlState` (§8), where `SqlState` is an
  enum/newtype over the 5-character code, covering the codes emitted and growing
  per feature. Every raised error picks a specific code; differential tests
  assert the code matches Postgres (Tier A), while message text resembles
  Postgres (Tier B) and is not asserted on;
- one central coercion module — no ad-hoc casts scattered through the executor.

Details:
1. **Error catalog** — `SqlState` codes are added as features land; each
   operation raises the specific Postgres code for its condition (e.g. `0A000`,
   `22003` numeric overflow, `22012` division by zero, `22P02` invalid text
   input, `23xxx` constraint violations).
2. **Type coercion / implicit casts** — a coercion table modeled on Postgres
   (`pg_cast` plus type-category / preferred-type rules) is applied in the three
   Postgres cast contexts: implicit (expressions), assignment (INSERT/UPDATE into
   a column), and explicit (`CAST`). Postgres's rules are mirrored rather than
   reinvented; the table is built incrementally per type and validated
   differentially.
3. **Runtime errors** — overflow, division by zero, invalid input syntax, length
   overflow, and constraint violations fire in the same situations as Postgres.

---

## 12. Testing Strategy

- **Differential testing** — the same SQL scripts are run against real Postgres
  (via `testcontainers`) and against `pg_fake`, and results are diffed. This is
  the primary correctness oracle and directly validates the fidelity contract.
- **Unit tests** — for the storage engine and visibility rules.
- **Concurrency tests** — scripted, controlled interleavings validate
  transaction semantics.
- **Property-based / fuzz testing** — for expressions and coercions, compared
  against Postgres.

---

## 13. Future Work

- **Wire-protocol server** — an optional layer that speaks the Postgres wire
  protocol so arbitrary drivers (`tokio-postgres`, `diesel`, ...) can connect,
  instead of one adapter crate per driver.
- **Cooperative scheduler** — controlled interleaving of concurrent transactions
  for fully reproducible concurrency tests.
- **Unsupported-feature catalog detail** — exact registry contents, per-feature
  `SQLSTATE` choices, and whether `restrict` is set per-`Db` or per-`Session`
  (leaning toward both, with session overriding DB), to be filled in as features
  land.
- **Persistent-map storage** — swap `BTreeMap` for a structurally-shared
  persistent map to make snapshots near-O(1), if snapshot cost becomes
  significant.
