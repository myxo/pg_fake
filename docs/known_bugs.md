# Known bugs

## Text collation

`pg_fake` compares text using Rust's binary string ordering and does not model
PostgreSQL collations. PostgreSQL may use libc or ICU (International Components
for Unicode), so text comparison and `ORDER BY` can differ by database locale.
For example, PostgreSQL may sort `fallback` before `MiXeD`, while `pg_fake`
sorts `MiXeD` first.

Differential properties should use collation-independent order keys until the
project defines and implements a collation contract.

## Deferred SQLx rollback can be lost before polling

Dropping an open SQLx transaction calls `PgFakeTransactionManager::start_rollback`,
which resets the driver's transaction depth and records `pending_rollback` so the
core `Session` can be rolled back by the next asynchronous connection operation.

`PgFakeConnection::run`, `ping`, and `prepare_with` currently consume
`pending_rollback` before constructing their returned future. If that future, or
the stream containing it, is dropped without ever being polled, the captured
rollback flag is dropped too and no blocking task executes `ROLLBACK`.

The driver then reports no active transaction while the core `Session` remains
inside the abandoned transaction. Uncommitted changes and row locks can remain,
and later operations may execute in the wrong transaction or encounter lock
timeouts. Once `spawn_blocking` has been submitted, dropping the outer future is
not this bug because the blocking task continues independently.

The fix must keep the rollback pending until its execution is guaranteed, with
regression coverage for unpolled or cancelled query streams, `ping`, and
statement preparation.
