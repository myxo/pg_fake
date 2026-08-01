# Known bugs

## Text collation

`pg_fake` compares text using Rust's binary string ordering and does not model
PostgreSQL collations. PostgreSQL may use libc or ICU (International Components
for Unicode), so text comparison and `ORDER BY` can differ by database locale.
For example, PostgreSQL may sort `fallback` before `MiXeD`, while `pg_fake`
sorts `MiXeD` first.

Differential properties should use collation-independent order keys until the
project defines and implements a collation contract.
