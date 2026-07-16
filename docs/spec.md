# pg_fake — Design Specification (Draft v0.1)

> Status: **DRAFT for review**. This document captures the intended design and
> deliberately marks open questions with **[OPEN]** so we can iterate before
> committing to implementation.

---

## 1. Goal & Non-Goals

### 1.1 Goal
`pg_fake` is an in-memory, embeddable fake of PostgreSQL intended for use as a
**test double** (fake object) in automated tests. Given the same sequence of SQL
statements, `pg_fake` should produce the same observable results as a real
PostgreSQL server, so that tests written against it behave as they would against
Postgres — but orders of magnitude faster.

Concretely:
- Pure Rust library, no external process, no networking. The API is plain
  function calls (embedded, in-process).
- SQL is parsed with `sqlparser-rs`.
- Concurrency: multiple queries / transactions can be in flight at the same
  time (even though `sqlparser-rs` parsing is serialized behind a global mutex).
- Correct transactional semantics matching PostgreSQL.
- Cheap database snapshots for test fixtures.

### 1.2 Non-Goals
- Not a production database. No durability, no crash recovery guarantees, no
  real WAL-to-disk, no replication.
- Not a drop-in wire-protocol server (at least not in the core). See **[OPEN
  Q-WIRE]**.
- Not aiming for performance on large datasets; test datasets are assumed small.

### 1.3 What "mimic in exact detail" means — **[DECIDED Q-EXACT]**
Full bit-for-bit fidelity with Postgres is effectively unbounded work. We define
a **fidelity contract**: three buckets of promises about how closely observable
behavior matches Postgres.

- **Tier A — guaranteed to match** (tests may rely on this):
  - result *sets* (as multisets) for supported SQL,
  - NULL / three-valued logic,
  - type coercions we support,
  - constraint violations *occurring or not* (unique / not-null / FK / check),
  - transaction visibility and isolation outcomes,
  - sequence value allocation,
  - **`SQLSTATE` error codes**: for any error that is reproducible in Postgres
    and in scope (i.e. not caused by the filesystem, OS, or other out-of-scope
    subsystems), `pg_fake` must return the **same error class/code**. Error
    *category* matching is a hard requirement.
- **Tier B — best effort** (may match; do not build tests around it):
  - exact error *message text* (Postgres changes wording across versions),
  - numeric and float text formatting,
  - `now()` / clock semantics — but see the time-mocking requirement below.
- **Tier C — explicitly NOT guaranteed:** row order when no `ORDER BY` is given
  (Postgres itself does not guarantee this), physical planning, performance,
  `ctid` values, exact bytes of `EXPLAIN`.

**Driver assumption:** `sqlx` is the expected primary driver, and we will verify
that the error **category** it surfaces is an exact match. We must not assume
`sqlx` is the *only* driver in the future, so error-code fidelity is defined at
the SQL/`SQLSTATE` level, independent of any specific driver.

**Time mocking (requirement):** although `now()`/clock is Tier B for exact
values, `pg_fake` **must** provide a way to control time explicitly — e.g. set
and manually advance the database's notion of "now" — so tests can make
time-dependent behavior deterministic. Tracked as **[Q-TIME]** for API details.

**[DECIDED Q-PGVERSION]** The reference is **PostgreSQL 18**. All fidelity
claims and differential tests are measured against Postgres 18 behavior.

---

## 2. High-Level Architecture

```
                +-----------------------------------------------+
                |                  pg_fake DB                   |
                |                                               |
   caller  ---> |  Session / Connection  (one per "client")    |
 (test code)    |     - current transaction state              |
                |     - transaction-local WAL                   |
                +----------------------+------------------------+
                                       |
                +----------------------v------------------------+
                |  Parser (sqlparser-rs, behind global mutex)   |
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
        |   - Tables: RowId -> Row (tuple of Value)                    |
        |   - Global commit WAL (committed, not-yet-synced changes)    |
        |   - Version visibility / snapshot logic                     |
        |   - Sync/GC ("vacuum") of WAL into base tables              |
        +-------------------------------------------------------------+
```

### 2.1 Component responsibilities
- **Session:** the unit a caller interacts with. Analogous to a Postgres
  connection/backend. Holds current transaction (if any), its snapshot, and its
  transaction-local WAL. Statements outside an explicit transaction run in
  implicit (autocommit) single-statement transactions.
- **Parser:** thin wrapper over `sqlparser-rs`. Because parsing is serialized by
  a global mutex, we parse-and-release quickly, producing an owned AST. Parsing
  is expected to be cheap relative to execution. **[OPEN Q-PARSECACHE]** consider
  caching parsed statements by SQL string.
- **Analyzer/Binder:** resolves identifiers against the catalog, infers/checks
  types, applies implicit casts, and produces a bound logical plan.
- **Executor:** interprets the bound plan against the storage engine.
- **Storage engine:** owns all data and the MVCC/WAL machinery.

---

## 3. Data Model

### 3.1 Value type
A single Rust enum `Value` represents any cell value. Proposed initial set
(**[OPEN Q-TYPES]** for the exact list & backing crates):

| Postgres type(s)              | Rust representation                    |
|-------------------------------|----------------------------------------|
| `null` (any type)             | `Value::Null`                          |
| `boolean`                     | `bool`                                 |
| `smallint`/`int2`             | `i16`                                  |
| `integer`/`int4`              | `i32`                                  |
| `bigint`/`int8`               | `i64`                                  |
| `real`/`float4`               | `f32`                                  |
| `double precision`/`float8`   | `f64`                                  |
| `numeric`/`decimal`           | arbitrary precision (`rust_decimal` or `bigdecimal`) |
| `text`/`varchar`/`char`/`bpchar` | `String` (+ length metadata in schema) |
| `bytea`                       | `Vec<u8>`                              |
| `date`,`time`,`timestamp`,`timestamptz`,`interval` | `chrono`/`time` types |
| `uuid`                        | `uuid::Uuid`                           |
| `json`,`jsonb`                | parsed JSON value (+ raw for `json`)   |
| arrays                        | `Vec<Value>` (+ element type)          |

Notes / concerns:
- **Type vs value:** a column's *declared type* lives in the catalog; the
  `Value` only needs to carry enough to reconstruct it. But some types need
  in-band metadata (e.g. `numeric` precision/scale, `varchar(n)`). Decide
  whether `Value` is type-tagged or the schema is the sole source of type.
- **NULL is a value, not a variant per type** — three-valued logic must be
  implemented consistently across all operators.

### 3.2 Row & RowId
- A **Row** is a tuple/`Vec<Value>` positionally aligned with the table's column
  list.
- **[DECIDED Q-KEY]** Rows are identified by an internal, monotonically
  increasing **`RowId`** (an analog of Postgres `ctid`/tuple identity), assigned
  at insert and stable for the row's lifetime even across updates. The table map
  is `RowId -> Row`. Primary/unique keys are enforced via **separate index
  structures** (`value(s) -> RowId`), never by using a key as the map key.

  Rationale (all must remain correct): tables may have **no primary key**;
  tables may contain **duplicate rows**; `UPDATE` may **change the primary key**;
  and MVCC requires a row identity that is **independent of its column values**.
  Trade-off: point lookups by PK go through an index (`value -> RowId -> Row`),
  one extra indirection — negligible at test data sizes.

### 3.3 Table storage
```
Table {
    schema:      TableSchema,      // columns, types, constraints
    rows:        Map<RowId, Row>,  // base (synced) committed data
    indexes:     Vec<Index>,       // for unique enforcement / (later) speed
    next_rowid:  RowId,
}
```
Map choice **[OPEN Q-MAP]**: `HashMap`, `BTreeMap` (stable iteration order), or a
persistent map (`im::HashMap`) to make snapshots O(1). See §7.

---

## 4. Catalog

The catalog holds metadata: schemas, tables, columns (name, type, nullability,
default expr), constraints (PK, unique, FK, check, not-null), sequences, and
(later) views. DDL statements mutate the catalog.

- **[OPEN Q-DDL-TX]** In Postgres, DDL is transactional (can be rolled back).
  Do we support transactional DDL in v1, or treat DDL as auto-committing? This
  significantly affects the WAL/snapshot design.
- Sequences (`SERIAL`, `GENERATED ... AS IDENTITY`) allocate values that are
  **not** rolled back on abort (matching Postgres). Sequence state must live
  outside transactional visibility rules.

---

## 5. Transactions, WAL & Visibility

This is the core and the riskiest part. Below is the original design, restated
precisely, followed by the semantic gaps we must close.

### 5.1 Original design (restated)
1. Each table is a map of rows.
2. Row values are tuples of `Value`.
3. There is a single **global WAL** of changes that are committed but not yet
   synced (merged) into the base tables.
4. Each transaction has its **own (transaction-local) WAL** for its uncommitted
   changes.
5. On `SELECT`, search transaction WAL → global WAL → base tables.
6. When a transaction starts, it records the current global WAL index and only
   reads global-WAL entries at or before that index (its snapshot).
7. When no active transaction still needs a given WAL prefix, **sync** it:
   apply those entries into the base tables and truncate the WAL.

### 5.2 What this design is, in DB terms
This is **snapshot isolation** implemented as a multi-version log:
- base tables = fully-merged committed history,
- global WAL = recent committed versions not yet merged,
- transaction WAL = the transaction's own uncommitted versions,
- the recorded start index = the transaction's **snapshot**,
- the sync/GC step = **vacuum / checkpoint**, and the "minimum index still
  needed" is the **xmin horizon**.

The design is basically sound as *snapshot isolation*. The gaps are about
matching Postgres exactly:

### 5.3 Gap 1 — Isolation levels **[DECIDED Q-ISO]**
**Decision:** the *design* must accommodate all three standard Postgres
isolation levels (READ COMMITTED, REPEATABLE READ, SERIALIZABLE). Implementation
*effort* focuses on the default (READ COMMITTED) first; the others follow, but
the architecture must not preclude any of them (in particular, must leave room
for SSI).

The three levels, in Postgres terms:
- **READ COMMITTED (default):** each *statement* sees a fresh snapshot of data
  committed *before that statement started*. ⇒ executor takes a snapshot **per
  statement**.
- **REPEATABLE READ:** snapshot frozen at the transaction's first statement —
  matches the original rule 6 — plus must raise `40001` serialization failures on
  write conflicts. ⇒ snapshot **per transaction**.
- **SERIALIZABLE:** as REPEATABLE READ, plus SSI-style predicate/read-write
  dependency tracking to catch write-skew. ⇒ requires additional conflict
  tracking machinery.

So the recorded "start index" is really a **snapshot handle** whose lifetime
depends on the level (per-statement vs per-transaction). Design implications:
- The visibility mechanism must accept a snapshot taken at an arbitrary commit
  point and be reusable for both per-statement and per-transaction snapshots.
- Data structures must let us later attach **read/write predicate tracking** for
  SSI without redesign (e.g. we do not discard which rows/ranges a transaction
  read).

### 5.3.1 How the level is chosen (must replicate)
Resolution order, matching Postgres:
1. Explicit on the transaction: `BEGIN TRANSACTION ISOLATION LEVEL ...` or
   `SET TRANSACTION ISOLATION LEVEL ...` (only valid before the first statement).
2. Session default: `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION
   LEVEL ...`.
3. DB/server default (`default_transaction_isolation`), defaulting to
   **READ COMMITTED**.

The level is **fixed once the transaction's first statement runs**; attempting
to change it afterward is an error. Autocommit statements run as a
single-statement transaction at the session/server default level.

### 5.4 Gap 2 — Write/write conflicts & blocking **[OPEN Q-BLOCK]**
The WAL/visibility rules cover *reads*. They do **not** define what happens when
two concurrent transactions write the same row. Postgres behavior:
- The second writer **blocks** until the first transaction commits or aborts.
- Then, under READ COMMITTED it re-reads and proceeds (or the row is gone);
  under REPEATABLE READ it raises `40001 could not serialize access`.

Two possible strategies for the fake:
- **(a) True blocking:** an `UPDATE`/`DELETE`/`SELECT FOR UPDATE` that hits a
  row locked by another transaction blocks (condvar) until that transaction
  ends. This matches Postgres, including deadlock potential (which we'd need to
  detect and error with `40P01`). Requires that caller threads can block.
- **(b) Immediate conflict error:** never block; raise a serialization error
  right away. Simpler, but *diverges* from Postgres READ COMMITTED and can break
  tests that rely on blocking semantics.

Recommendation: **(a) true blocking**, because "match Postgres exactly" implies
it, and the concurrency count is tiny. We need a **row-lock table** keyed by
`(table, RowId)` and a **deadlock detector** (wait-for graph). **[OPEN]**

### 5.5 Gap 3 — WAL entry contents
Each WAL entry must be enough to (a) determine visibility and (b) apply on sync.
Proposed entry:
```
WalEntry {
    xid:        Xid,                 // transaction id
    table:      TableId,
    op:         Insert(RowId, Row)
              | Update(RowId, old: Row, new: Row)
              | Delete(RowId, old: Row),
    commit_seq: Option<CommitSeq>,   // set at commit; None while uncommitted
}
```
- Visibility of a version to a snapshot is decided by `commit_seq` vs the
  snapshot boundary, plus "my own uncommitted writes are visible to me".
- Tombstones (Delete) are required so a snapshot can still see a row that a newer
  transaction deleted.

**[CONCERN]** "search transaction WAL → global WAL → base tables" is O(WAL) per
lookup. For point lookups and to enforce visibility correctly, we likely want a
**per-row version chain** (like Postgres) rather than a flat scan of the WAL.
i.e. `Map<RowId, Vec<Version>>` where each version carries `xmin_commit_seq` /
`xmax_commit_seq`. The "global WAL" then becomes an ordering/GC device rather
than the primary read path. We should decide between:
- **Flat WAL scan** (simple, matches original wording, slower), vs
- **Per-row version chains** (Postgres-like, faster reads, more code).

### 5.6 Gap 4 — Sync / GC horizon
- Maintain the set of active transactions and their snapshot indices.
- The **GC horizon** = min snapshot index over all active transactions (and the
  latest commit if none active).
- WAL entries fully older than the horizon can be merged into base tables and
  removed; superseded/tombstoned versions can be dropped.
- **[OPEN Q-GC-TRIGGER]** When does sync run? Options: lazily on commit, on a
  background thread, or on read when WAL gets long. For a test fake, lazy /
  synchronous-on-commit is probably simplest and deterministic.

### 5.7 Statement & transaction control
Support: `BEGIN`/`START TRANSACTION`, `COMMIT`, `ROLLBACK`, `SAVEPOINT` /
`ROLLBACK TO SAVEPOINT` / `RELEASE`, `SET TRANSACTION ISOLATION LEVEL`,
implicit autocommit.
- **[OPEN Q-SAVEPOINT]** Savepoints require nested rollback of the
  transaction-local WAL (subtransactions). Do we need them in v1?
- Error handling: in Postgres, an error inside a transaction poisons it
  ("current transaction is aborted, commands ignored until end of transaction
  block", `25P02`). We must replicate this state machine.

---

## 6. Concurrency Model

- The DB is `Send + Sync` and shared via `Arc<Db>`; each caller thread holds a
  `Session`.
- **[OPEN Q-LOCKING]** Global locking strategy. Options:
  - Single `RwLock<StorageEngine>` — trivially correct, but serializes writers
    and complicates blocking semantics.
  - Fine-grained: `RwLock` per table + a separate lock manager for row locks +
    an `Arc`/atomic-based version store. More concurrency, more complexity.
- Parsing is serialized behind `sqlparser`'s global mutex; we minimize time
  held by parsing into owned ASTs immediately.
- Blocking semantics (§5.4) require a lock manager with wait queues and deadlock
  detection **[OPEN]**.
- Determinism concern **[OPEN Q-DETERMINISM]**: tests often want *deterministic*
  behavior. Under true concurrency, interleavings are nondeterministic just like
  real Postgres. That is arguably "correct", but we should offer a
  **single-threaded / deterministic mode** for reproducible tests.

---

## 7. Snapshots (Fixtures)

Goal: build a DB in a fixture once, then cheaply fork it per test.

Options:
- **Deep clone** of all tables + catalog. Simple; cost scales with data size.
  Fine for small fixtures.
- **Structural sharing** via persistent data structures (`im`/`rpds`) or
  `Arc`-per-table copy-on-write, so a snapshot is O(1)/O(#tables) and pages are
  shared until mutated. Better if fixtures are large or forked many times.

Constraints:
- A snapshot should be taken from a **quiescent** state (no in-flight
  transactions) or must define what happens to uncommitted data (drop it).
  Recommendation: snapshot = current committed state, uncommitted transactions
  are not captured.
- Sequences: snapshot must capture current sequence values.
- **[OPEN Q-SNAPSHOT-API]** API shape: `db.snapshot() -> Db` (owned fork) vs a
  builder that produces fresh `Db`s.

---

## 8. Public API (sketch) — **[OPEN Q-API]**

```rust
let db = Db::new();                       // empty database

// A session is like a connection.
let mut sess = db.session();

// One-shot execution (autocommit):
let result = sess.execute("CREATE TABLE t (id int primary key, name text)")?;
let rows   = sess.query("SELECT * FROM t WHERE id = $1", &[Value::Int4(1)])?;

// Explicit transaction:
let mut tx = sess.begin()?;               // or sess.execute("BEGIN")
tx.execute("INSERT INTO t VALUES (1, 'a')")?;
tx.commit()?;                             // or tx.rollback()

// Fixtures / snapshots:
let base = build_fixture();               // Db
let db1  = base.snapshot();               // cheap fork for one test
```

Questions:
- Parameter binding (`$1`) and prepared statements — support in v1?
- Result representation: typed rows, column metadata, `RowsAffected`.
- Do we expose a `sqlx`/`tokio-postgres`-compatible surface so existing code can
  swap the fake in without changing call sites? **[OPEN Q-COMPAT]**

---

## 9. SQL Feature Scope (phased) — **[OPEN Q-SCOPE]**

Trying to support all of Postgres SQL at once is unrealistic. Proposed phases:

**Phase 1 (MVP):**
- DDL: `CREATE TABLE` (common types, PK/NOT NULL/UNIQUE/DEFAULT), `DROP TABLE`.
- DML: `INSERT`, `SELECT` (projection, `WHERE`, `ORDER BY`, `LIMIT/OFFSET`),
  `UPDATE`, `DELETE`.
- Expressions: comparisons, boolean logic (3-valued), arithmetic, common
  functions, `CASE`.
- Transactions: `BEGIN/COMMIT/ROLLBACK`, READ COMMITTED + REPEATABLE READ.
- Constraints enforced: NOT NULL, UNIQUE/PK, CHECK.

**Phase 2:**
- Joins (inner/left/right/full/cross), subqueries, `GROUP BY`/aggregates,
  `DISTINCT`, `HAVING`.
- Sequences/`SERIAL`, `RETURNING`.
- Foreign keys.

**Phase 3:**
- CTEs, `INSERT ... ON CONFLICT`, window functions, views, more types
  (json/jsonb, arrays), `SET`/session GUCs, savepoints.

Each phase should ship with a **conformance test suite run against real
Postgres** (see §11).

---

## 10. Error & Type Fidelity — **[OPEN Q-ERRORS]**

- Errors should carry a `SQLSTATE` code and message. We match codes (Tier B)
  where reasonable; exact message text is best-effort.
- Type coercion / implicit cast rules must follow Postgres (e.g. `int + numeric`
  → numeric). This is a large, detail-heavy area; we encode a coercion table.
- Overflow, division by zero, string-length overflow, invalid input syntax must
  raise the corresponding errors.

---

## 11. Testing Strategy

- **Differential testing:** run the same SQL scripts against real Postgres
  (via `testcontainers`) and against `pg_fake`, diff results. This is the
  primary correctness oracle and directly validates the fidelity contract.
- Unit tests for the storage engine / visibility rules.
- Concurrency tests using scripted, controlled interleavings (deterministic
  mode) to validate transaction semantics.
- Property-based / fuzz testing of expressions and coercions vs Postgres.

---

## 12. Open Questions Summary

| ID | Topic | Question |
|----|-------|----------|
| ~~Q-EXACT~~ | Fidelity | **DECIDED** §1.3: Tier A/B/C; SQLSTATE codes are Tier A, message text Tier B. |
| Q-TIME | API | How to expose time mocking (set / advance "now"). |
| ~~Q-PGVERSION~~ | Fidelity | **DECIDED**: reference is **PostgreSQL 18**. |
| ~~Q-ISO~~ | Isolation | **DECIDED** §5.3: design for all 3 levels; implement READ COMMITTED first. |
| Q-BLOCK | Concurrency | True blocking + deadlock detection, or immediate conflict error? |
| ~~Q-KEY~~ | Storage | **DECIDED** §3.2: internal `RowId` map key + separate indexes for PK/unique. |
| Q-WALMODEL | Storage | Flat WAL scan vs per-row version chains? |
| Q-DDL-TX | Catalog | Transactional DDL in v1? |
| Q-MAP | Storage | HashMap / BTreeMap / persistent map? |
| Q-LOCKING | Concurrency | Global RwLock vs fine-grained locking. |
| Q-DETERMINISM | Concurrency | Provide a deterministic single-thread mode? |
| Q-SNAPSHOT-API | API | Snapshot API shape & semantics. |
| Q-TYPES | Data | Exact type list + backing crates (numeric, dates, uuid, json). |
| Q-COMPAT | API | Provide sqlx/tokio-postgres-compatible surface? |
| Q-SCOPE | SQL | Confirm phase boundaries. |
| Q-SAVEPOINT | Tx | Savepoints/subtransactions in v1? |
| Q-GC-TRIGGER | Storage | When does sync/GC run? |
| Q-WIRE | API | Any need for wire-protocol server later? |
| Q-PARSECACHE | Parser | Cache parsed statements? |
| Q-ERRORS | Fidelity | How closely to match SQLSTATE / messages? |

---

## 13. My Main Recommendations (for discussion)

1. **Replace "PK as hashmap key" with an internal `RowId`.** Postgres identifies
   tuples physically; PK-as-key breaks on no-PK tables, duplicates, and PK
   updates.
2. **Adopt per-row version chains** rather than scanning the flat WAL on every
   read. Keep the "global WAL" concept as the commit-ordering + GC device. This
   is closer to Postgres and keeps reads fast.
3. **Pin down isolation levels first** — implement READ COMMITTED as the default
   (per-statement snapshot), because that is Postgres's default and what most
   tests exercise. REPEATABLE READ maps directly to your rule 6.
4. **Decide blocking vs immediate-error early**, because it shapes the lock
   manager. To truly "match Postgres", true blocking + deadlock detection is
   needed, but it's the biggest complexity driver.
5. **Lock the fidelity contract (Tier A/B/C).** Without it, "exactly like
   Postgres" is unbounded.
6. **Differential testing against real Postgres from day one** — it is the only
   practical way to verify "same output as Postgres".
