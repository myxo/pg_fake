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
| `tier1_select_100_rows` | Fetches a small table with a plain SELECT. |
| `tier1_select_where_100_rows` | Performs a simple equality lookup without an index. |
| `tier1_select_where_indexed_100_rows` | Performs the corresponding equality lookup with a primary-key index. |
| `tier1_limit_offset_ordered_100_rows` | Fetches a page using a single sort key, LIMIT, and OFFSET. |
| `tier1_order_by_100_rows` | Sorts a small result by scalar columns; explicit null placement remains part of ordinary ordering. |
| `tier1_selective_inner_join` | Performs a two-table equality join with a simple filter and one matching result. |
| `tier1_many_match_inner_join` | Performs the same basic equality-join shape with multiple matches; no lateral, recursive, or aggregate operation. |

## Tier 2: important operations (44 groups)

| Benchmark | Reason |
| --- | --- |
| `tier2_core_snapshot_100_rows` | Forks a database fixture; useful test setup, outside the basic SQL operations prioritized in Tier 1. |
| `tier2_session_settings_roundtrip` | Sets, reads, and resets connection settings used by applications and drivers. |
| `tier2_nested_savepoint_release` | Exercises explicit SAVEPOINT/RELEASE statements for nested transactions, which driver transaction nesting also relies on. |
| `tier2_nested_savepoint_rollback` | Exercises partial rollback of nested transactions, beyond a basic transaction. |
| `tier2_runtime_temporal_100_rows` | Exercises timestamp conversion, truncation, formatting, and numeric expressions used in data transformations. |
| `tier2_runtime_patterns_100_rows` | Exercises ILIKE and regular-expression matching for application text searches and validation. |
| `tier2_create_table` | Measures schema creation and removal for fixtures and migrations, outside the agreed Tier 1 operations. |
| `tier2_transactional_ddl_create_rollback` | Measures transactional schema rollback, relevant to migration and fixture isolation. |
| `tier2_sqlx_migration_chain` | Runs migrations and their reversal through the primary driver, an important test-double integration workflow. |
| `tier2_alter_table_rewrite_100_rows` | Adds a defaulted non-null column and drops it again, representing schema evolution. |
| `tier2_insert_row_returning` | Adds result production and fetching to INSERT through RETURNING. |
| `tier2_insert_row_with_defaults` | Omits a column and evaluates its default expression, a separate insert variant. |
| `tier2_insert_on_conflict_do_nothing` | Exercises duplicate handling through ON CONFLICT DO NOTHING. |
| `tier2_insert_on_conflict_conflict_free` | Exercises the no-conflict branch of an upsert, with cleanup included in the measurement. |
| `tier2_insert_on_conflict_do_update` | Exercises the conflict/update branch of an upsert. |
| `tier2_update_from_row` | Sources an update value from another table through UPDATE FROM. |
| `tier2_delete_row` | Measures ordinary row deletion; kept in Tier 2 under the agreed focus on inserts, reads, updates, and transactions. |
| `tier2_sequence_nextval` | Allocates a value through an explicit sequence call, supporting generated-key workflows. |
| `tier2_serial_identity_insert` | Generates an identity value and fetches it with RETURNING; includes more than the basic insert path. |
| `tier2_uuid_temporal_select` | Combines UUID key lookup with timestamp-plus-interval arithmetic, exercising additional types and expressions. |
| `tier2_offset_datetime_bind_store_fetch` | Exercises SQLx timestamp binding, upsert storage, RETURNING, and decoding. |
| `tier2_bigint_uuid_array_bind_store_fetch` | Binds bigint and UUID arrays into an upsert, then decodes the returned bigint array. |
| `tier2_uuid_any_100_rows` | Looks up a batch of identifiers using a bound UUID array and ANY. |
| `tier2_array_containment_100_rows` | Filters records by array containment, an additional application data-model operation. |
| `tier2_json_insert_returning` | Stores and returns a JSON document, exercising the JSON representation and result path. |
| `tier2_jsonb_insert_returning` | Stores and returns a JSONB document, exercising JSONB conversion and result production. |
| `tier2_jsonb_extraction` | Reads a field from JSONB documents, supporting document-valued application columns. |
| `tier2_jsonb_containment` | Filters JSONB documents by containment, beyond scalar predicates. |
| `tier2_window_row_number_100_rows` | Assigns ordered row numbers, a useful query and data-migration feature beyond basic reads. |
| `tier2_ordered_string_agg_100_rows` | Builds ordered per-group strings, an additional reporting/aggregation operation. |
| `tier2_transaction_repeatable_read_select_for_update` | Combines repeatable-read isolation with row-lock acquisition, beyond the default transaction path. |
| `tier2_adapter_overhead_select_100_rows` | Compares core and SQLx overhead for an ordinary read; diagnoses implementation cost rather than adding a Tier 1 workflow. |
| `tier2_core_parsed_vs_prepared_point_select` | Compares one-shot and prepared execution, a targeted optimization diagnostic. |
| `tier2_point_lookup_index_vs_scan` | Compares prepared core lookups at 100 and 10,000 rows; Tier 1 already covers the basic indexed and heap lookup paths. |
| `tier2_concurrent_uncontended_reads` | Compares two independent sessions reading without lock contention; relevant to concurrent application tests. |
| `tier2_foreign_key_insert` | Times both a parent insert and a referencing child insert, exercising referential checks across tables. |
| `tier2_derived_and_scalar_subquery_100_rows` | Combines a derived table, scalar subquery, and subquery membership test. |
| `tier2_materialized_cte_100_rows` | Reuses a non-recursive read CTE from two sides of a join. |
| `tier2_correlated_exists_100_rows` | Checks related-row existence through a correlated subquery. |
| `tier2_global_aggregate_100_rows` | Computes count, sum, average, minimum, and maximum over a table. |
| `tier2_grouped_aggregate_100_rows` | Summarizes rows with GROUP BY, HAVING, and ordering. |
| `tier2_select_distinct_100_rows` | Deduplicates a result with DISTINCT. |
| `tier2_union_all_100_rows` | Combines two result sets while retaining duplicates. |
| `tier2_union_100_rows` | Combines two result sets while removing duplicates. |

## Tier 3: rare operations and diagnostics (22 groups)

| Benchmark | Reason |
| --- | --- |
| `tier3_transaction_local_guc_roundtrip` | Exercises transaction-local configuration through set_config/current_setting and rollback. |
| `tier3_skip_locked_queue_100_rows` | Combines priority ordering, a limit, and FOR UPDATE SKIP LOCKED for a database-backed work queue. |
| `tier3_lateral_latest_per_parent_100_rows` | Runs a correlated LEFT JOIN LATERAL with per-parent ordering and a limit. |
| `tier3_migration_table_lock_two_relations` | Acquires an explicit exclusive table lock across two relations during a transaction. |
| `tier3_procedural_trigger_insert_update` | Runs a procedural trigger function on inserts and updates; the trigger is the feature being exercised. |
| `tier3_partial_unique_index_100_rows` | Builds and drops a descending unique index with both INCLUDE and a predicate; does not measure an ordinary indexed read. |
| `tier3_temporary_table_on_commit_drop` | Exercises a temporary relation whose lifetime ends at transaction commit. |
| `tier3_catalog_regclass_lookup` | Introspects pg_attribute using regclass and format_type, a schema-tooling workload. |
| `tier3_ordered_filtered_array_agg_100_rows` | Combines ordered array_agg, FILTER, array subscripting, and max in one aggregation. |
| `tier3_correlated_unnest_100_rows` | Expands each row's array through correlated LATERAL unnest. |
| `tier3_hashed_advisory_lock_acquisition` | Derives a hashed application lock key and acquires a transaction-scoped advisory lock. |
| `tier3_jsonb_join_group` | Joins, groups, and sorts on whole JSONB documents rather than ordinary scalar keys. |
| `tier3_window_rank_100_rows` | Combines partitioned rank, dense_rank, and ntile in one analytic query. |
| `tier3_window_offset_100_rows` | Combines lag, lead, first_value, last_value, and nth_value across partitions. |
| `tier3_window_moving_aggregate_100_rows` | Exercises moving aggregates with both ROWS and GROUPS window frames. |
| `tier3_nested_filtered_view_100_rows` | Reads through two filtered view layers and applies another outer predicate. |
| `tier3_transaction_history_point_select` | Probes lookup cost after up to 100,000 completed transactions, a history-scaling diagnostic. |
| `tier3_mvcc_old_snapshot_read` | Retains an old snapshot through up to 10,000 updates, a version-chain stress case. |
| `tier3_concurrent_same_row_contention` | Forces a same-row update wait with an artificial delay and rollback, a contention diagnostic. |
| `tier3_data_modifying_cte_update_100_rows` | Feeds UPDATE RETURNING through a CTE into an aggregate query. |
| `tier3_recursive_cte_numeric_series_100_rows` | Generates a numeric series through recursive SQL evaluation. |
| `tier3_recursive_cte_branching_traversal_127_rows` | Traverses a branching hierarchy through a recursive join. |
