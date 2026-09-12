# Transactional migration coverage

| Required form | Scenario and version |
|---|---|
| Qualified `public` relations and all required scalar column types | `schema_evolution/001` |
| Transactional table and sequence creation | `schema_evolution/001`, `schema_evolution/002` |
| Add, drop, rename, rewrite, default, nullability, and constraint table changes | `schema_evolution/002` |
| Sequence backfill with `row_number`, `max`, `setval`, and `nextval` | `schema_evolution/002` |
| Partial, unique, covering, renamed, and dropped indexes | `schema_evolution/002` |
| `NOT VALID` foreign key and validation | `schema_evolution/002`, `data_reconciliation/003` |
| View creation, comment, read behavior, and conditional drop | `schema_evolution/002` |
| Function and row-level trigger creation | `procedural_triggers/001` |
| Standalone `BEFORE INSERT`, standalone `BEFORE UPDATE`, and combined `BEFORE INSERT OR UPDATE` | `procedural_triggers/001`, `procedural_triggers/002` |
| Trigger rename and trigger/function drop | `procedural_triggers/003` |
| `DO`, locals, multi-expression `SELECT INTO`, diagnostics, branches, formatted errors, and hints | `procedural_triggers/002`, `procedural_triggers/003`, `data_reconciliation/002` |
| Qualified permanent and `ON COMMIT DROP` temporary tables | `data_reconciliation/001`, `data_reconciliation/002` |
| `SET LOCAL`, multi-table `EXCLUSIVE`, and one-table `ACCESS EXCLUSIVE` locks | `data_reconciliation/002`, `data_reconciliation/003` |
| Ordinary and materialized CTEs, insert-select, conflict handling, joins, and limiting | `schema_evolution/002`, `data_reconciliation/002` |
| JSONB `#>`/`#>>` paths, type inspection, numeric casts, missing paths, JSON null, and SQL NULL | `schema_evolution/001`, `data_reconciliation/001`, `data_reconciliation/002` |
| `EXISTS`, `NOT EXISTS`, `IS DISTINCT FROM`, regex, searched `CASE`, and `coalesce` | `data_reconciliation/002` |
| Window counts and row numbering, ordered `string_agg`, and aggregates | `schema_evolution/002`, `data_reconciliation/002` |
| `UPDATE FROM` a CTE or derived table and correlated queries | `schema_evolution/002`, `data_reconciliation/002` |
| SQLx migration bookkeeping, reapplication, and atomic failure | Migration-chain integration tests |
| SQLx typed parameter and result encoding for every required scalar family | Migration-chain integration tests over `schema_evolution` |
