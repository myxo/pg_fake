# SQLx application-workload conformance gate

Run the focused application gate with PostgreSQL 18 available through
`PG_FAKE_DATABASE_URL`:

```console
cargo test -p pg_fake_sqlx --features time --test application_workload
```

Run the complete Task 30 gate, including formatting, strict workspace Clippy,
workspace tests, every focused replay below, and `_long` property tests at
the extended 10,000-iteration/600-second budget per test:

```console
just task30-gate
```

When `PG_FAKE_DATABASE_URL` is set, it must name a disposable PostgreSQL 18
database using `C` collation. With no configured URL, the test harness starts
PostgreSQL 18 itself.

The application test applies all three Task 20 migration chains and the
application schema to both PostgreSQL 18 and `pg_fake`. It then compares one
typed snapshot containing prepared metadata, affected-row counts, SQLSTATEs,
transaction outcomes, and workflow results.

## Application workflows

| Workflow | Coverage |
|---|---|
| Identity and session data | SQLx pool, prepared UUID lookup, BIGINT[] and UUID[] codecs, JSONB context, `OffsetDateTime`, partial uniqueness, `ON CONFLICT DO UPDATE`, and `23505` |
| Work claiming and accounting | Hashed transaction advisory locks, `FOR UPDATE SKIP LOCKED`, concurrent transactions, row updates, accounting inserts, view reads, table-lock timeout, and `55P03` |
| Payment and promotion state | Transactional data-modifying CTE, NUMERIC parameter, JSONB path extraction, foreign keys, affected state, and commit |
| Request limits | Prepared predicates, ordered and filtered BIGINT `array_agg`, one-based subscripting, and UUID `ANY` membership |
| Thread and execution state | Typed UUID/timestamptz/JSONB rows, correlated `LATERAL`, ordering and limiting, JSONB extraction, recursive CTE, and set operations |
| Append-only logs | Sequence defaults, unique conflict `DO NOTHING`, ordered reads, explicit rollback, and `TRUNCATE ... RESTART IDENTITY` maintenance |
| Migrated trigger state | Task 20 trigger functions execute on insert/update and expose their row mutations through typed reads |
| Startup/runtime compatibility | Temporal formatting, pattern matching, `pg_is_in_recovery`, and `to_regclass` |

## Manifest coverage

Every manifest feature now records its owning plan task. The application gate
directly executes every case owned by Tasks 1–30 against PostgreSQL 18 and
`pg_fake`, including expected-error cases. This prevents a new priority feature
or case from being omitted by a hand-maintained allowlist. The focused replay
column records the deeper differential suite retained for that family.

| Feature | Manifest cases | Classification and replay |
|---|---|---|
| Bounded LATERAL | `lateral_per_parent_limit`, `lateral_aggregate` | Application; `lateral_differential.rs` |
| Runtime expressions | `runtime_epoch_and_floor`, `runtime_timestamp_format_and_truncate`, `runtime_dst_timezone`, `runtime_like_regex` | Application; `runtime_expression_differential.rs` |
| Qualified and temporary relations | `public_qualified_relation`, `temporary_shadowing`, `temporary_on_commit_drop` | Task 20 application migrations; `migration_chains.rs` |
| Set operations | `union_distinct` | Application; `postgres_regress.rs` and `property_tests.rs` |
| Non-recursive CTEs | `named_cte`, `dependency_chain`, `repeated_materialized_reference`, `nested_shadowing` | Task 20 application migrations; `postgres_regress.rs` and `property_tests.rs` |
| Recursive CTEs | `recursive_series`, `recursive_union_cycle`, `recursive_empty_seed`, `recursive_reference_in_seed` | Application; `postgres_regress.rs` and `property_tests.rs` |
| Data-modifying CTEs | `insert_returning_cte`, `statement_snapshot_visibility`, `delete_feeds_insert`, `unreferenced_mutation`, `referenced_without_returning` | Application; `sqlx_driver.rs`, `postgres_regress.rs`, and `property_tests.rs` |
| Conflict handling | `unique_arbiter`, `excluded_row` | Application; `sqlx_driver.rs`, `postgres_regress.rs`, and `property_tests.rs` |
| Window ranking | `row_number` | Task 20 application migrations; `migration_data_transform_differential.rs` |
| Migration transforms | `window_partition_and_order`, `ordered_string_aggregate`, `migration_predicates_and_temporal_expressions`, `insert_select_and_update_from` | Task 20 application migrations; `migration_data_transform_differential.rs` |
| JSON | `json_text`, `json_storage_fidelity`, `json_malformed_input`, `json_equality_rejected` | Focused replay in `json_differential.rs` |
| JSONB | `jsonb_normalization`, `jsonb_migration_storage`, `jsonb_numeric_equality` | Application and Task 20 migrations; `json_differential.rs` |
| JSON operators | `jsonb_extraction`, `jsonb_migration_paths`, `jsonb_expansion` | Application and Task 20 migrations; `json_differential.rs` |
| Array type and I/O | `bigint_uuid_array_literals`, `array_storage_and_text_io` | Application; `array_differential.rs` |
| Required array queries | `ordered_filtered_bigint_array_agg_subscript`, `uuid_any_all_membership` | Application; `array_differential.rs` |
| MVCC catalog | `own_uncommitted_table` | Task 20 migrations; `transactional_ddl.rs` |
| Transactional DDL | `create_then_rollback` | Task 20 migrations; `transactional_ddl.rs` and `migration_chains.rs` |
| ALTER TABLE | `alter_column_rewrite`, `add_not_valid_check`, `validate_foreign_key` | Task 20 migrations; `migration_chains.rs` |
| Index DDL | `covering_partial_index`, `partial_unique_arbiter`, `rename_index_if_exists`, `drop_index_if_exists` | Application and Task 20 migrations; `migration_chains.rs` |
| Ordinary views | `select_from_view`, `nested_view_with_explicit_columns`, `replace_and_comment_view`, `drop_view_if_exists` | Application and Task 20 migrations; `migration_chains.rs` |
| Task 14 local settings | `set_local_lock_timeout`, `set_local_statement_timeout` | Application and Task 20 migrations; `migration_chains.rs` |
| Migration table locks | `access_exclusive_table_lock`, `exclusive_multi_table_lock` | Application and Task 20 migrations; `migration_chains.rs` |
| Advisory coordination | `transaction_advisory_signatures`, `transaction_advisory_try_signatures`, `sqlx_migrator_session_advisory_pair` | Application and migrator execution; `advisory_differential.rs` |
| Procedural migrations | `before_insert_trigger`, `anonymous_do_control_flow` | Task 20 application migrations; `procedural.rs` and `migration_chains.rs` |
| Row locks | `skip_locked_ordered_queue`, `no_key_update_nowait` | Application; `row_lock_differential.rs` |
| Text hashes | `text_hash_values`, `hashed_advisory_lock` | Application; `text_hash_differential.rs` |
| Compatibility utilities | `primary_recovery_state`, `pg_lsn_values_and_arithmetic`, `regclass_catalog_introspection`, `truncate_restart_identity` | Application; `compatibility_utilities_differential.rs` |
| OffsetDateTime codec | No scalar SQL manifest entry because this is a bound Rust type | Application; `time_codec_differential.rs` |

The following priority scenarios have a structured Application or
FocusedReplay classification whose set must exactly equal all manifest
scenarios owned by Tasks 1–30. Focused replay paths are checked to exist:
`schema_evolution`,
`procedural_triggers`, `data_reconciliation`, `concurrent_unique_insert`,
`concurrent_conflict_recheck`, `uncommitted_ddl_visibility`,
`drop_and_recreate_visibility`, `lock_mode_compatibility_matrix`,
`skip_locked_work_queue`, `hashed_advisory_contention`, and
`priority_application_workload`.

Savepoints, the general session GUC registry, general array expressions,
remaining window functions, and SERIALIZABLE scenarios are intentionally not
part of Task 30; they belong to Tasks 31–39.

Task 30 adds a fixed integration gate rather than a new SQL execution family,
so it does not add another property generator or benchmark. Its component SQL
families retain their existing generated tests and benchmarks; the application
gate verifies their composition through SQLx.
