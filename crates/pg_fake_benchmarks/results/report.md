# Benchmark results

## Environment

| Property | Value |
| --- | --- |
| architecture | aarch64 |
| cpu | Apple M2 |
| criterion | 0.5 |
| logical_cpus | 8 |
| os | macos |
| os_version | Darwin 24.6.0 |
| performance_levels | level 0: 4 physical / 4 logical; level 1: 4 physical / 4 logical |
| physical_cores | 8 |
| postgres_target | 18 |
| recorded_at | 2026-09-22T21:07:01Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| session_settings_roundtrip/pg_fake | 71.08 us | N/A |
| session_settings_roundtrip/postgres_18 | 140.53 us | N/A |
| transaction_local_guc_roundtrip/pg_fake | 109.27 us | N/A |
| transaction_local_guc_roundtrip/postgres_18 | 137.46 us | N/A |
| nested_savepoint_release/pg_fake | 119.43 us | N/A |
| nested_savepoint_release/postgres_18 | 189.14 us | N/A |
| nested_savepoint_rollback/pg_fake | 121.35 us | N/A |
| nested_savepoint_rollback/postgres_18 | 192.10 us | N/A |
| skip_locked_queue_100_rows/pg_fake | 74.68 ms | -82.12% |
| skip_locked_queue_100_rows/postgres_18 | 48.94 us | +0.47% |
| lateral_latest_per_parent_100_rows/pg_fake | 7.08 ms | +13.95% |
| lateral_latest_per_parent_100_rows/postgres_18 | 170.71 us | +46.61% |
| runtime_temporal_100_rows/pg_fake | 1.23 ms | +19.41% |
| runtime_temporal_100_rows/postgres_18 | 117.82 us | -1.06% |
| runtime_patterns_100_rows/pg_fake | 4.22 ms | +0.73% |
| runtime_patterns_100_rows/postgres_18 | 117.98 us | -0.81% |
| create_table/pg_fake | 207.06 us | +426.19% |
| create_table/postgres_18 | 954.56 us | -4.60% |
| transactional_ddl_create_rollback/pg_fake | 56.60 us | +63.02% |
| transactional_ddl_create_rollback/postgres_18 | 1.34 ms | -3.56% |
| migration_table_lock_two_relations/pg_fake | 42.42 us | +64.84% |
| migration_table_lock_two_relations/postgres_18 | 84.27 us | -11.66% |
| sqlx_migration_chain/pg_fake | 71.84 ms | -20.05% |
| sqlx_migration_chain/postgres_18 | 6.56 ms | -1.74% |
| procedural_trigger_insert_update/pg_fake | 17.76 ms | -20.97% |
| procedural_trigger_insert_update/postgres_18 | 122.45 us | -3.87% |
| alter_table_rewrite_100_rows/pg_fake | 341.11 us | -10.98% |
| alter_table_rewrite_100_rows/postgres_18 | 379.64 us | -4.25% |
| partial_unique_index_100_rows/pg_fake | 108.08 us | +50.16% |
| partial_unique_index_100_rows/postgres_18 | 546.53 us | -9.28% |
| temporary_table_on_commit_drop/pg_fake | 37.74 us | +115.52% |
| temporary_table_on_commit_drop/postgres_18 | 336.25 us | +47.38% |
| insert_row/pg_fake | 61.77 us | +31.99% |
| insert_row/postgres_18 | 93.13 us | -3.45% |
| insert_row_returning/pg_fake | 74.55 us | +43.98% |
| insert_row_returning/postgres_18 | 98.19 us | +0.07% |
| insert_row_with_defaults/pg_fake | 63.60 us | +40.36% |
| insert_row_with_defaults/postgres_18 | 95.13 us | -7.87% |
| insert_on_conflict_do_nothing/pg_fake | 42.43 us | +53.83% |
| insert_on_conflict_do_nothing/postgres_18 | 30.44 us | -8.89% |
| insert_on_conflict_conflict_free/pg_fake | 94.68 us | +72.62% |
| insert_on_conflict_conflict_free/postgres_18 | 67.73 us | +8.09% |
| insert_on_conflict_do_update/pg_fake | 54.68 us | +48.61% |
| insert_on_conflict_do_update/postgres_18 | 35.36 us | +1.17% |
| update_row/pg_fake | 53.93 us | +38.85% |
| update_row/postgres_18 | 91.70 us | -2.31% |
| update_from_row/pg_fake | 58.74 us | +35.02% |
| update_from_row/postgres_18 | 101.55 us | -1.60% |
| delete_row/pg_fake | 39.45 us | +85.90% |
| delete_row/postgres_18 | 119.24 us | -1.03% |
| sequence_nextval/pg_fake | 43.73 us | +86.73% |
| sequence_nextval/postgres_18 | 28.86 us | +2.40% |
| catalog_regclass_lookup/pg_fake | 78.59 us | +30.13% |
| catalog_regclass_lookup/postgres_18 | 33.54 us | -1.11% |
| serial_identity_insert/pg_fake | 42.83 us | +82.82% |
| serial_identity_insert/postgres_18 | 32.99 us | -9.71% |
| uuid_temporal_select/pg_fake | 48.69 us | +79.21% |
| uuid_temporal_select/postgres_18 | 32.15 us | +2.76% |
| offset_datetime_bind_store_fetch/pg_fake | 61.19 us | +37.38% |
| offset_datetime_bind_store_fetch/postgres_18 | 36.40 us | -4.65% |
| bigint_uuid_array_bind_store_fetch/pg_fake | 68.22 us | +29.88% |
| bigint_uuid_array_bind_store_fetch/postgres_18 | 37.38 us | -0.14% |
| uuid_any_100_rows/pg_fake | 3.06 ms | +174.37% |
| uuid_any_100_rows/postgres_18 | 53.67 us | -1.38% |
| ordered_filtered_array_agg_100_rows/pg_fake | 215.02 us | +15.05% |
| ordered_filtered_array_agg_100_rows/postgres_18 | 44.02 us | -0.08% |
| array_containment_100_rows/pg_fake | 121.86 us | N/A |
| array_containment_100_rows/postgres_18 | 41.79 us | N/A |
| correlated_unnest_100_rows/pg_fake | 462.29 us | N/A |
| correlated_unnest_100_rows/postgres_18 | 153.40 us | N/A |
| hashed_advisory_lock_acquisition/pg_fake | 37.32 ms | -25.90% |
| hashed_advisory_lock_acquisition/postgres_18 | 79.65 us | +0.44% |
| json_insert_returning/pg_fake | 78.14 us | +95.53% |
| json_insert_returning/postgres_18 | 145.50 us | -1.37% |
| jsonb_insert_returning/pg_fake | 84.66 us | +82.30% |
| jsonb_insert_returning/postgres_18 | 146.92 us | -0.01% |
| jsonb_extraction/pg_fake | 124.09 us | +29.96% |
| jsonb_extraction/postgres_18 | 67.33 us | -1.12% |
| jsonb_containment/pg_fake | 276.61 us | +33.67% |
| jsonb_containment/postgres_18 | 43.58 us | -2.08% |
| jsonb_join_group/pg_fake | 748.66 us | +1.36% |
| jsonb_join_group/postgres_18 | 354.86 us | +0.48% |
| window_row_number_100_rows/pg_fake | 185.15 us | +47.23% |
| window_row_number_100_rows/postgres_18 | 84.84 us | -0.43% |
| window_rank_100_rows/pg_fake | 505.70 us | N/A |
| window_rank_100_rows/postgres_18 | 109.37 us | N/A |
| window_offset_100_rows/pg_fake | 1.04 ms | N/A |
| window_offset_100_rows/postgres_18 | 125.85 us | N/A |
| window_moving_aggregate_100_rows/pg_fake | 605.68 us | N/A |
| window_moving_aggregate_100_rows/postgres_18 | 151.19 us | N/A |
| ordered_string_agg_100_rows/pg_fake | 276.40 us | +17.42% |
| ordered_string_agg_100_rows/postgres_18 | 72.62 us | -4.23% |
| transaction_insert/pg_fake | 65.01 us | -4.42% |
| transaction_insert/postgres_18 | 136.23 us | -0.11% |
| transaction_repeatable_read_select_for_update/pg_fake | 68.12 ms | -72.08% |
| transaction_repeatable_read_select_for_update/postgres_18 | 85.52 us | -1.94% |
| select_100_rows/pg_fake | 33.94 us | +4.03% |
| select_100_rows/postgres_18 | 59.72 us | -0.33% |
| select_where_100_rows/pg_fake | 19.76 us | +5.91% |
| select_where_100_rows/postgres_18 | 34.65 us | +0.11% |
| select_where_indexed_100_rows/pg_fake | 13.00 us | +9.80% |
| select_where_indexed_100_rows/postgres_18 | 31.68 us | -5.90% |
| limit_offset_ordered_100_rows/pg_fake | 83.57 us | +19.61% |
| limit_offset_ordered_100_rows/postgres_18 | 42.68 us | -0.08% |
| nested_filtered_view_100_rows/pg_fake | 492.08 us | +6.56% |
| nested_filtered_view_100_rows/postgres_18 | 35.55 us | -3.13% |
| order_by_100_rows/pg_fake | 124.79 us | +13.31% |
| order_by_100_rows/postgres_18 | 67.06 us | -1.60% |
| foreign_key_insert/pg_fake | 111.93 us | +49.88% |
| foreign_key_insert/postgres_18 | 181.87 us | +0.02% |
| selective_inner_join/pg_fake | 106.63 us | +25.37% |
| selective_inner_join/postgres_18 | 40.96 us | -1.06% |
| many_match_inner_join/pg_fake | 166.00 us | +14.81% |
| many_match_inner_join/postgres_18 | 67.47 us | -0.46% |
| derived_and_scalar_subquery_100_rows/pg_fake | 378.64 us | +16.52% |
| derived_and_scalar_subquery_100_rows/postgres_18 | 87.13 us | -8.75% |
| materialized_cte_100_rows/pg_fake | 8.97 ms | +2.96% |
| materialized_cte_100_rows/postgres_18 | 79.45 us | -0.82% |
| data_modifying_cte_update_100_rows/pg_fake | 2.20 ms | -15.95% |
| data_modifying_cte_update_100_rows/postgres_18 | 105.93 us | -2.00% |
| recursive_cte_numeric_series_100_rows/pg_fake | 2.52 ms | +11.95% |
| recursive_cte_numeric_series_100_rows/postgres_18 | 75.16 us | -0.78% |
| recursive_cte_branching_traversal_127_rows/pg_fake | 8.18 ms | +10.91% |
| recursive_cte_branching_traversal_127_rows/postgres_18 | 122.72 us | -0.08% |
| correlated_exists_100_rows/pg_fake | 90.47 us | +17.94% |
| correlated_exists_100_rows/postgres_18 | 74.26 us | -35.14% |
| global_aggregate_100_rows/pg_fake | 42.69 us | +4.15% |
| global_aggregate_100_rows/postgres_18 | 41.49 us | +0.08% |
| grouped_aggregate_100_rows/pg_fake | 202.46 us | +16.27% |
| grouped_aggregate_100_rows/postgres_18 | 49.99 us | -0.17% |
| select_distinct_100_rows/pg_fake | 66.39 us | +34.92% |
| select_distinct_100_rows/postgres_18 | 45.61 us | -0.35% |
| union_all_100_rows/pg_fake | 434.25 us | +12.06% |
| union_all_100_rows/postgres_18 | 87.00 us | -0.43% |
| union_100_rows/pg_fake | 475.40 us | +12.31% |
| union_100_rows/postgres_18 | 87.14 us | -0.29% |
| core_snapshot_100_rows/pg_fake | 14.94 us | N/A |
| adapter_overhead_select_100_rows/core | 107.04 us | +9.14% |
| adapter_overhead_select_100_rows/sqlx | 113.36 us | +10.21% |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 18.56 us | +14.97% |
| core_parsed_vs_prepared_point_select/prepared_reuse | 440.92 ns | -0.60% |
| transaction_history_point_select/1 | 269.79 ns | +8.71% |
| transaction_history_point_select/100 | 268.44 ns | +7.05% |
| transaction_history_point_select/10,000 | 277.07 ns | +11.77% |
| transaction_history_point_select/100,000 | 270.79 ns | +8.19% |
| mvcc_old_snapshot_read/1 | 498.31 ns | -6.85% |
| mvcc_old_snapshot_read/100 | 1.89 us | -23.32% |
| mvcc_old_snapshot_read/10,000 | 664.97 us | -4.27% |
| point_lookup_index_vs_scan/heap_scan/100 | 2.73 us | -5.09% |
| point_lookup_index_vs_scan/unique_index/100 | 481.85 ns | +0.91% |
| point_lookup_index_vs_scan/heap_scan/10,000 | 261.82 us | -3.37% |
| point_lookup_index_vs_scan/unique_index/10,000 | 531.22 ns | +0.84% |
| concurrent_uncontended_reads/sequential | 18.59 us | -5.72% |
| concurrent_uncontended_reads/parallel | 17.24 us | +2.72% |
| concurrent_same_row_contention/wait_then_rollback | 2.10 ms | +13.76% |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 1.98x |
| transaction_local_guc_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 1.26x |
| nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 1.58x |
| nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 1.58x |
| skip_locked_queue_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1525.94x |
| lateral_latest_per_parent_100_rows | postgres_18 | pg_fake | 🔴 ↓ 41.45x |
| runtime_temporal_100_rows | postgres_18 | pg_fake | 🔴 ↓ 10.47x |
| runtime_patterns_100_rows | postgres_18 | pg_fake | 🔴 ↓ 35.80x |
| create_table | postgres_18 | pg_fake | 🟢 ↑ 4.61x |
| transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 23.63x |
| migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 1.99x |
| sqlx_migration_chain | postgres_18 | pg_fake | 🔴 ↓ 10.95x |
| procedural_trigger_insert_update | postgres_18 | pg_fake | 🔴 ↓ 145.03x |
| alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.11x |
| partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 5.06x |
| temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 8.91x |
| insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.51x |
| insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 1.32x |
| insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 1.50x |
| insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🔴 ↓ 1.39x |
| insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🔴 ↓ 1.40x |
| insert_on_conflict_do_update | postgres_18 | pg_fake | 🔴 ↓ 1.55x |
| update_row | postgres_18 | pg_fake | 🟢 ↑ 1.70x |
| update_from_row | postgres_18 | pg_fake | 🟢 ↑ 1.73x |
| delete_row | postgres_18 | pg_fake | 🟢 ↑ 3.02x |
| sequence_nextval | postgres_18 | pg_fake | 🔴 ↓ 1.52x |
| catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 2.34x |
| serial_identity_insert | postgres_18 | pg_fake | 🔴 ↓ 1.30x |
| uuid_temporal_select | postgres_18 | pg_fake | 🔴 ↓ 1.51x |
| offset_datetime_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.68x |
| bigint_uuid_array_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.83x |
| uuid_any_100_rows | postgres_18 | pg_fake | 🔴 ↓ 57.07x |
| ordered_filtered_array_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.88x |
| array_containment_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.92x |
| correlated_unnest_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.01x |
| hashed_advisory_lock_acquisition | postgres_18 | pg_fake | 🔴 ↓ 468.54x |
| json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 1.86x |
| jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 1.74x |
| jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.84x |
| jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 6.35x |
| jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 2.11x |
| window_row_number_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.18x |
| window_rank_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.62x |
| window_offset_100_rows | postgres_18 | pg_fake | 🔴 ↓ 8.25x |
| window_moving_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.01x |
| ordered_string_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.81x |
| transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.10x |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🔴 ↓ 796.56x |
| select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.76x |
| select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.75x |
| select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.44x |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.96x |
| nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 13.84x |
| order_by_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.86x |
| foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 1.62x |
| selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.60x |
| many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.46x |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.35x |
| materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 112.95x |
| data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 20.78x |
| recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 33.54x |
| recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 66.68x |
| correlated_exists_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.22x |
| global_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.03x |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.05x |
| select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.46x |
| union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.99x |
| union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.46x |
| adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.06x |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 42.08x |
| transaction_history_point_select | 1 | 100 | 🟢 ↑ 1.01x |
| transaction_history_point_select | 1 | 10,000 | 🔴 ↓ 1.03x |
| transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.00x |
| mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.79x |
| mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1334.44x |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.67x |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 492.88x |
| concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.08x |
