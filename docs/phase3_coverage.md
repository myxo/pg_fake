# Phase 3 feature registry

This registry describes the SQL surface exercised by the Phase 3 conformance
manifest. PostgreSQL 18 is the comparison target. A supported family includes
the forms listed here and in the [implementation plan](plan.md); the family
name does not imply every PostgreSQL form is implemented.

| Family | Supported forms | Later or outside the current surface |
| --- | --- | --- |
| Queries | `UNION`, `INTERSECT`, `EXCEPT`; ordinary, recursive, and data-modifying CTEs; bounded `LATERAL`; ranking, value, and aggregate windows with `ROWS`, `RANGE`, and `GROUPS` frames | Grouping sets, general table functions, window-frame `EXCLUDE` syntax absent from the parser AST |
| Mutations | `INSERT`, `UPDATE`, `DELETE`, `RETURNING`, and `ON CONFLICT DO NOTHING` / `DO UPDATE` with supported arbiters | `MERGE`, general-purpose rules, generated columns |
| Types | Supported scalar types plus JSON, JSONB, and one-dimensional arrays of supported scalar types; SQLx `time::OffsetDateTime` for `timestamptz` | JSONPath, SQL/JSON, multidimensional or non-default-bound arrays, numeric `NaN`, new type families such as PostgreSQL `name` (cast system-catalog names to `text` for SQLx metadata) |
| Relations and DDL | Ordinary read-only views; supported transactional table, sequence, index, view, function, trigger, schema, and bounded `ALTER TABLE` operations | Materialized, recursive, or updatable views; unrestricted PL/pgSQL, triggers, schemas, indexes, and `ALTER TABLE` |
| Transactions | READ COMMITTED, REPEATABLE READ, SERIALIZABLE, savepoints, row-lock variants, transaction advisory locks, transactional DDL | Prepared transactions, unmodeled transaction access modes, server-wide administration |
| Settings | Typed session and transaction-local settings listed in the [README](../README.md#session-settings) | Full PostgreSQL GUC catalog, non-UTF-8 client encodings, POSIX time-zone rules |

Unsupported statements normally return an error. Two bounded forms have a
`Tolerate` policy for driver plumbing: `ANALYZE` succeeds as a no-op, and known
planner settings are validated and retained without changing execution. Use
`Db::create_builder().set_strict_mode_enabled(true).build()` to reject these
tolerated forms. Unknown setting names and unsupported user SQL are rejected in
either mode.

The [Phase 3 release audit](phase3_release_audit.md) records the manifest,
upstream corpus, property gate, application workload, and benchmark status.
