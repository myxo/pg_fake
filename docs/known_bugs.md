# Known bugs

## Text collation

`pg_fake` compares text using Rust's binary string ordering and does not model
PostgreSQL collations. PostgreSQL may use libc or ICU (International Components
for Unicode), so text comparison and `ORDER BY` can differ by database locale.
For example, PostgreSQL may sort `fallback` before `MiXeD`, while `pg_fake`
sorts `MiXeD` first.

Differential properties should use collation-independent order keys until the
project defines and implements a collation contract.

## INSERT with omitted trailing columns

Without an explicit column list, PostgreSQL allows a VALUES row to omit trailing
columns and supplies their defaults. `pg_fake` instead rejects the value count
with `42601`. For example:

```sql
CREATE TABLE omitted_columns (id INTEGER PRIMARY KEY, value INTEGER, extra INTEGER DEFAULT 1);
INSERT INTO omitted_columns VALUES (1, 0);
```

Using `INSERT INTO omitted_columns (id, value) VALUES (1, 0)` works. This existing
limitation was exposed by the Task 31 savepoint generator after adding a column;
that generator now names its input columns explicitly.

## Timestamptz input with omitted seconds and an explicit offset

`SELECT TIMESTAMPTZ '2024-07-01 12:00+00'` succeeds in PostgreSQL but currently
returns `22007` in `pg_fake`. Including seconds (`'2024-07-01 12:00:00+00'`)
works. This existing timestamp-input limitation was exposed by Task 32's
session-time-zone tests; those tests use the supported full timestamp form.
