# Benchmark tier assignments

Tiers set this project's performance priorities. They are judgments about the
workloads we want to optimize, not measured statistics about PostgreSQL users.
Classify the operation being timed, including its SQL clauses and workflow,
rather than only the leading statement or benchmark name. Incidental fixture
values do not determine a tier.

- Tier 1 is a small set of representative basic inserts, updates, default
  transactions, reads, sorting/paging, and simple equality joins.
- Tier 2 covers additional application features and supporting test/driver
  workflows. An insert variant does not inherit Tier 1 merely because it inserts
  rows: `RETURNING`, default evaluation, identity allocation, foreign-key writes,
  and upserts belong here.
- Tier 3 covers specialized query/workflow combinations and deliberate stress
  scenarios. Plain concurrent reads are Tier 2; forced contention and very long
  transaction/version histories are Tier 3.

All variants/backends within a benchmark group share its tier. Tier 1 already
contains paired indexed and heap reads; their prepared scaling diagnostic is
Tier 2. Every current group is assessed below. The executable assignments live
in [`src/lib.rs`](src/lib.rs); update this assessment when adding or moving one.

## Tier 1: essential operations (10 groups)

| Benchmark | Reason |
| --- | --- |
| `tier1_insert_row` | Inserts an explicitly supplied row through the ordinary insert path. |
| `tier1_update_row` | Updates a row by primary key using an ordinary UPDATE and WHERE predicate. |
| `tier1_transaction_insert` | Wraps an ordinary insert in BEGIN/COMMIT, covering the basic explicit transaction path. |
| `tier1_select_100` | Fetches a small table with a plain SELECT. |
| `tier1_select_where_100` | Performs a simple equality lookup without an index. |
| `tier1_select_where_indexed_100` | Performs the corresponding equality lookup with a primary-key index. |
| `tier1_limit_offset_ordered_100` | Fetches a page using a single sort key, LIMIT, and OFFSET. |
| `tier1_order_by_100` | Sorts a small result by scalar columns; explicit null placement remains part of ordinary ordering. |
| `tier1_selective_inner_join` | Performs a two-table equality join with a simple filter and one matching result. |
| `tier1_many_match_inner_join` | Performs the same basic equality-join shape with multiple matches; no lateral, recursive, or aggregate operation. |

## Tier 2: important operations (66 groups)

| Benchmark | Reason |
| --- | --- |
| `tier2_insert_bound_row` | Inserts a row through a bound SQLx parameter, exposing adapter and parameter handling costs. |
| `tier2_update_bound_row` | Updates a row through a bound SQLx parameter, separate from the literal-value Tier 1 update. |
| `tier2_core_snapshot_100` | Forks a database fixture; useful test setup, outside the basic SQL operations prioritized in Tier 1. |
| `tier2_core_snapshot_1k` | Checks how fixture-fork cost changes with a larger populated database. |
| `tier2_session_settings_roundtrip` | Sets, reads, and resets connection settings used by applications and drivers. |
| `tier2_nested_savepoint_release` | Exercises explicit SAVEPOINT/RELEASE statements for nested transactions, which driver transaction nesting also relies on. |
| `tier2_nested_savepoint_rollback` | Exercises partial rollback of nested transactions, beyond a basic transaction. |
| `tier2_runtime_temporal_100` | Exercises timestamp conversion, truncation, formatting, and numeric expressions used in data transformations. |
| `tier2_runtime_patterns_100` | Exercises ILIKE and regular-expression matching for application text searches and validation. |
| `tier2_create_table` | Measures schema creation and removal for fixtures and migrations, outside the agreed Tier 1 operations. |
| `tier2_ddl_create_rollback` | Measures transactional schema rollback, relevant to migration and fixture isolation. |
| `tier2_sqlx_migration_chain` | Runs migrations and their reversal through the primary driver, an important test-double integration workflow. |
| `tier2_alter_table_rewrite_100` | Adds a defaulted non-null column and drops it again, representing schema evolution. |
| `tier2_insert_row_returning` | Adds result production and fetching to INSERT through RETURNING. |
| `tier2_insert_row_with_defaults` | Omits a column and evaluates its default expression, a separate insert variant. |
| `tier2_on_conflict_nothing` | Exercises duplicate handling through ON CONFLICT DO NOTHING. |
| `tier2_on_conflict_no_conflict` | Exercises the no-conflict branch of an upsert, with cleanup included in the measurement. |
| `tier2_on_conflict_update` | Exercises the conflict/update branch of an upsert. |
| `tier2_update_from_row` | Sources an update value from another table through UPDATE FROM. |
| `tier2_delete_row` | Measures ordinary row deletion; kept in Tier 2 under the agreed focus on inserts, reads, updates, and transactions. |
| `tier2_sequence_nextval` | Allocates a value through an explicit sequence call, supporting generated-key workflows. |
| `tier2_serial_identity_insert` | Generates an identity value and fetches it with RETURNING; includes more than the basic insert path. |
| `tier2_uuid_temporal_select` | Combines UUID key lookup with timestamp-plus-interval arithmetic, exercising additional types and expressions. |
| `tier2_offset_datetime_roundtrip` | Exercises SQLx timestamp binding, upsert storage, RETURNING, and decoding. |
| `tier2_int8_uuid_array_roundtrip` | Binds bigint and UUID arrays into an upsert, then decodes the returned bigint array. |
| `tier2_uuid_any_100` | Looks up a batch of identifiers using a bound UUID array and ANY. |
| `tier2_array_containment_100` | Filters records by array containment, an additional application data-model operation. |
| `tier2_json_insert_returning` | Stores and returns a JSON document, exercising the JSON representation and result path. |
| `tier2_jsonb_insert_returning` | Stores and returns a JSONB document, exercising JSONB conversion and result production. |
| `tier2_jsonb_extraction` | Reads a field from JSONB documents, supporting document-valued application columns. |
| `tier2_jsonb_containment` | Filters JSONB documents by containment, beyond scalar predicates. |
| `tier2_window_row_number_100` | Assigns ordered row numbers, a useful query and data-migration feature beyond basic reads. |
| `tier2_ordered_string_agg_100` | Builds ordered per-group strings, an additional reporting/aggregation operation. |
| `tier2_rr_select_for_update` | Combines repeatable-read isolation with row-lock acquisition, beyond the default transaction path. |
| `tier2_adapter_select_100` | Compares core and SQLx overhead for an ordinary read; diagnoses implementation cost rather than adding a Tier 1 workflow. |
| `tier2_core_parse_vs_prepare` | Compares one-shot and prepared execution, a targeted optimization diagnostic. |
| `tier2_lookup_index_vs_scan` | Compares prepared core lookups at 100 and 10,000 rows; Tier 1 already covers the basic indexed and heap lookup paths. |
| `tier2_lookup_unique_100` | Measures a selective unique index lookup over 100 rows through each SQLx backend. |
| `tier2_lookup_unique_1k` | Measures a selective unique index lookup over 1,000 rows through each SQLx backend. |
| `tier2_lookup_nonuniq_100` | Measures a selective nonunique index lookup over 100 rows through each SQLx backend. |
| `tier2_lookup_nonuniq_1k` | Measures a selective nonunique index lookup over 1,000 rows through each SQLx backend. |
| `tier2_lookup_nonuniq_filter_100` | Measures a selective nonunique filtered lookup over 100 rows through each SQLx backend. |
| `tier2_lookup_nonuniq_filter_1k` | Measures a selective nonunique filtered lookup over 1,000 rows through each SQLx backend. |
| `tier2_lookup_heap_scan_100` | Measures a selective heap scan lookup over 100 rows through each SQLx backend. |
| `tier2_lookup_heap_scan_1k` | Measures a selective heap scan lookup over 1,000 rows through each SQLx backend. |
| `tier2_update_indexed_100` | Measures indexed UPDATE over 100 rows, with fixture setup and rollback outside the timed write. |
| `tier2_update_indexed_1k` | Measures indexed UPDATE over 1,000 rows, with fixture setup and rollback outside the timed write. |
| `tier2_delete_indexed_100` | Measures indexed DELETE over 100 rows, with fixture setup and rollback outside the timed write. |
| `tier2_delete_indexed_1k` | Measures indexed DELETE over 1,000 rows, with fixture setup and rollback outside the timed write. |
| `tier2_join_3way_100` | Measures a three-table join chain with a UUID key lookup into an indexed table of 100 rows. |
| `tier2_join_3way_1k` | Measures a three-table join chain with a UUID key lookup into an indexed table of 1,000 rows. |
| `tier2_join_3way_filtered_100` | Measures a filtered three-table join chain with a UUID key lookup into an indexed table of 100 rows. |
| `tier2_join_3way_filtered_1k` | Measures a filtered three-table join chain with a UUID key lookup into an indexed table of 1,000 rows. |
| `tier2_join_2way_filtered_100` | Measures a filtered two-table join with a UUID key lookup into an indexed table of 100 rows. |
| `tier2_join_2way_filtered_1k` | Measures a filtered two-table join with a UUID key lookup into an indexed table of 1,000 rows. |
| `tier2_concurrent_reads` | Compares two independent sessions reading without lock contention; relevant to concurrent application tests. |
| `tier2_foreign_key_insert` | Times both a parent insert and a referencing child insert, exercising referential checks across tables. |
| `tier2_derived_scalar_subq_100` | Combines a derived table, scalar subquery, and subquery membership test. |
| `tier2_materialized_cte_100` | Reuses a non-recursive read CTE from two sides of a join. |
| `tier2_derived_source_join_100` | Joins two derived table sources on an equality key, covering the materialized-source path. |
| `tier2_correlated_exists_100` | Checks related-row existence through a correlated subquery. |
| `tier2_global_aggregate_100` | Computes count, sum, average, minimum, and maximum over a table. |
| `tier2_grouped_aggregate_100` | Summarizes rows with GROUP BY, HAVING, and ordering. |
| `tier2_select_distinct_100` | Deduplicates a result with DISTINCT. |
| `tier2_union_all_100` | Combines two result sets while retaining duplicates. |
| `tier2_union_100` | Combines two result sets while removing duplicates. |

## Tier 3: rare operations and diagnostics (24 groups)

| Benchmark | Reason |
| --- | --- |
| `tier3_tx_local_guc` | Exercises transaction-local configuration through set_config/current_setting and rollback. |
| `tier3_skip_locked_queue_100` | Combines priority ordering, a limit, and FOR UPDATE SKIP LOCKED for a database-backed work queue. |
| `tier3_serializable_read` | Measures an uncontended read transaction at SERIALIZABLE isolation, a diagnostic for SSI tracking overhead. |
| `tier3_serializable_write_skew` | Runs two overlapping SERIALIZABLE transactions whose disjoint writes produce a serialization failure. |
| `tier3_lateral_latest_100` | Runs a correlated LEFT JOIN LATERAL with per-parent ordering and a limit. |
| `tier3_migration_lock_2tables` | Acquires an explicit exclusive table lock across two relations during a transaction. |
| `tier3_trigger_insert_update` | Runs a procedural trigger function on inserts and updates; the trigger is the feature being exercised. |
| `tier3_partial_unique_index_100` | Builds and drops a descending unique index with both INCLUDE and a predicate; does not measure an ordinary indexed read. |
| `tier3_temp_on_commit_drop` | Exercises a temporary relation whose lifetime ends at transaction commit. |
| `tier3_catalog_regclass_lookup` | Introspects pg_attribute using regclass and format_type, a schema-tooling workload. |
| `tier3_array_agg_order_filter_100` | Combines ordered array_agg, FILTER, array subscripting, and max in one aggregation. |
| `tier3_correlated_unnest_100` | Expands each row's array through correlated LATERAL unnest. |
| `tier3_hashed_advisory_lock` | Derives a hashed application lock key and acquires a transaction-scoped advisory lock. |
| `tier3_jsonb_join_group` | Joins, groups, and sorts on whole JSONB documents rather than ordinary scalar keys. |
| `tier3_window_rank_100` | Combines partitioned rank, dense_rank, and ntile in one analytic query. |
| `tier3_window_offset_100` | Combines lag, lead, first_value, last_value, and nth_value across partitions. |
| `tier3_window_moving_agg_100` | Exercises moving aggregates with both ROWS and GROUPS window frames. |
| `tier3_nested_filtered_view_100` | Reads through two filtered view layers and applies another outer predicate. |
| `tier3_tx_history_lookup` | Probes lookup cost after up to 100,000 completed transactions, a history-scaling diagnostic. |
| `tier3_mvcc_old_snapshot_read` | Retains an old snapshot through up to 10,000 updates, a version-chain stress case. |
| `tier3_same_row_contention` | Forces a same-row update wait with an artificial delay and rollback, a contention diagnostic. |
| `tier3_cte_update_100` | Feeds UPDATE RETURNING through a CTE into an aggregate query. |
| `tier3_recursive_series_100` | Generates a numeric series through recursive SQL evaluation. |
| `tier3_recursive_tree_127` | Traverses a branching hierarchy through a recursive join. |
