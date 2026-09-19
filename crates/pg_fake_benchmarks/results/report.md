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
| recorded_at | 2026-09-18T13:21:04Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| skip_locked_queue_100_rows/pg_fake | 417.75 ms | +283.76% |
| skip_locked_queue_100_rows/postgres_18 | 48.71 us | +0.10% |
| lateral_latest_per_parent_100_rows/pg_fake | 6.21 ms | +6.18% |
| lateral_latest_per_parent_100_rows/postgres_18 | 116.43 us | -80.57% |
| runtime_temporal_100_rows/pg_fake | 1.03 ms | +1.57% |
| runtime_temporal_100_rows/postgres_18 | 119.09 us | +1.31% |
| runtime_patterns_100_rows/pg_fake | 4.19 ms | +0.88% |
| runtime_patterns_100_rows/postgres_18 | 118.94 us | +1.13% |
| create_table/pg_fake | 39.35 us | +7.34% |
| create_table/postgres_18 | 1.00 ms | +11.98% |
| transactional_ddl_create_rollback/pg_fake | 34.72 us | +9.28% |
| transactional_ddl_create_rollback/postgres_18 | 1.39 ms | -2.72% |
| migration_table_lock_two_relations/pg_fake | 25.73 us | +0.11% |
| migration_table_lock_two_relations/postgres_18 | 95.40 us | +27.99% |
| sqlx_migration_chain/pg_fake | 89.85 ms | +6.46% |
| sqlx_migration_chain/postgres_18 | 6.68 ms | -2.79% |
| procedural_trigger_insert_update/pg_fake | 22.47 ms | +5.71% |
| procedural_trigger_insert_update/postgres_18 | 127.38 us | +4.22% |
| alter_table_rewrite_100_rows/pg_fake | 383.19 us | +27.03% |
| alter_table_rewrite_100_rows/postgres_18 | 396.48 us | +10.78% |
| partial_unique_index_100_rows/pg_fake | 71.98 us | +11.15% |
| partial_unique_index_100_rows/postgres_18 | 602.45 us | +22.83% |
| temporary_table_on_commit_drop/pg_fake | 17.51 us | +1.32% |
| temporary_table_on_commit_drop/postgres_18 | 228.15 us | +6.91% |
| insert_row/pg_fake | 46.80 us | +5.13% |
| insert_row/postgres_18 | 96.46 us | +3.95% |
| insert_row_returning/pg_fake | 51.78 us | +1.22% |
| insert_row_returning/postgres_18 | 98.12 us | +0.54% |
| insert_row_with_defaults/pg_fake | 45.31 us | +0.81% |
| insert_row_with_defaults/postgres_18 | 103.25 us | +8.99% |
| insert_on_conflict_do_nothing/pg_fake | 27.58 us | +26.95% |
| insert_on_conflict_do_nothing/postgres_18 | 33.41 us | +8.44% |
| insert_on_conflict_conflict_free/pg_fake | 54.85 us | +8.55% |
| insert_on_conflict_conflict_free/postgres_18 | 62.66 us | -7.36% |
| insert_on_conflict_do_update/pg_fake | 36.79 us | +3.36% |
| insert_on_conflict_do_update/postgres_18 | 34.95 us | +0.28% |
| update_row/pg_fake | 38.84 us | +6.98% |
| update_row/postgres_18 | 93.86 us | +0.12% |
| update_from_row/pg_fake | 43.51 us | +3.15% |
| update_from_row/postgres_18 | 103.20 us | +2.04% |
| delete_row/pg_fake | 21.22 us | +13.10% |
| delete_row/postgres_18 | 120.48 us | +0.93% |
| sequence_nextval/pg_fake | 23.42 us | +3.17% |
| sequence_nextval/postgres_18 | 28.18 us | +0.90% |
| catalog_regclass_lookup/pg_fake | 60.39 us | N/A |
| catalog_regclass_lookup/postgres_18 | 33.92 us | N/A |
| serial_identity_insert/pg_fake | 23.43 us | +4.43% |
| serial_identity_insert/postgres_18 | 36.54 us | +12.93% |
| uuid_temporal_select/pg_fake | 27.17 us | +6.93% |
| uuid_temporal_select/postgres_18 | 31.28 us | +1.74% |
| offset_datetime_bind_store_fetch/pg_fake | 44.54 us | N/A |
| offset_datetime_bind_store_fetch/postgres_18 | 38.17 us | N/A |
| bigint_uuid_array_bind_store_fetch/pg_fake | 52.52 us | N/A |
| bigint_uuid_array_bind_store_fetch/postgres_18 | 37.43 us | N/A |
| uuid_any_100_rows/pg_fake | 1.12 ms | N/A |
| uuid_any_100_rows/postgres_18 | 54.42 us | N/A |
| ordered_filtered_array_agg_100_rows/pg_fake | 186.89 us | N/A |
| ordered_filtered_array_agg_100_rows/postgres_18 | 44.06 us | N/A |
| hashed_advisory_lock_acquisition/pg_fake | 50.36 ms | N/A |
| hashed_advisory_lock_acquisition/postgres_18 | 79.30 us | N/A |
| json_insert_returning/pg_fake | 39.96 us | +3.11% |
| json_insert_returning/postgres_18 | 147.52 us | +1.08% |
| jsonb_insert_returning/pg_fake | 46.44 us | +3.16% |
| jsonb_insert_returning/postgres_18 | 146.94 us | +1.32% |
| jsonb_extraction/pg_fake | 95.48 us | +2.51% |
| jsonb_extraction/postgres_18 | 68.09 us | +1.20% |
| jsonb_containment/pg_fake | 206.94 us | +0.99% |
| jsonb_containment/postgres_18 | 44.51 us | +2.19% |
| jsonb_join_group/pg_fake | 738.59 us | +4.35% |
| jsonb_join_group/postgres_18 | 353.17 us | +4.27% |
| window_row_number_100_rows/pg_fake | 125.76 us | -2.13% |
| window_row_number_100_rows/postgres_18 | 85.21 us | +0.89% |
| ordered_string_agg_100_rows/pg_fake | 235.38 us | +1.56% |
| ordered_string_agg_100_rows/postgres_18 | 75.82 us | +6.80% |
| transaction_insert/pg_fake | 68.02 us | +2.87% |
| transaction_insert/postgres_18 | 136.38 us | +0.08% |
| transaction_repeatable_read_select_for_update/pg_fake | 244.01 ms | +132.10% |
| transaction_repeatable_read_select_for_update/postgres_18 | 87.21 us | +1.75% |
| select_100_rows/pg_fake | 32.63 us | +6.42% |
| select_100_rows/postgres_18 | 59.92 us | +0.65% |
| select_where_100_rows/pg_fake | 18.66 us | +8.08% |
| select_where_100_rows/postgres_18 | 34.62 us | +0.18% |
| select_where_indexed_100_rows/pg_fake | 11.84 us | +19.09% |
| select_where_indexed_100_rows/postgres_18 | 33.67 us | -0.88% |
| limit_offset_ordered_100_rows/pg_fake | 69.87 us | -2.48% |
| limit_offset_ordered_100_rows/postgres_18 | 42.71 us | +1.50% |
| nested_filtered_view_100_rows/pg_fake | 461.81 us | +2.16% |
| nested_filtered_view_100_rows/postgres_18 | 36.70 us | +3.76% |
| order_by_100_rows/pg_fake | 110.13 us | +2.72% |
| order_by_100_rows/postgres_18 | 68.15 us | +1.53% |
| foreign_key_insert/pg_fake | 74.68 us | +6.14% |
| foreign_key_insert/postgres_18 | 181.83 us | +1.41% |
| selective_inner_join/pg_fake | 85.05 us | +2.59% |
| selective_inner_join/postgres_18 | 41.39 us | +6.85% |
| many_match_inner_join/pg_fake | 144.59 us | +5.94% |
| many_match_inner_join/postgres_18 | 67.78 us | +1.97% |
| derived_and_scalar_subquery_100_rows/pg_fake | 324.95 us | +2.31% |
| derived_and_scalar_subquery_100_rows/postgres_18 | 95.49 us | +11.41% |
| materialized_cte_100_rows/pg_fake | 8.72 ms | +4.37% |
| materialized_cte_100_rows/postgres_18 | 80.10 us | +2.09% |
| data_modifying_cte_update_100_rows/pg_fake | 2.62 ms | +24.51% |
| data_modifying_cte_update_100_rows/postgres_18 | 108.09 us | +7.16% |
| recursive_cte_numeric_series_100_rows/pg_fake | 2.25 ms | +3.71% |
| recursive_cte_numeric_series_100_rows/postgres_18 | 75.75 us | +1.76% |
| recursive_cte_branching_traversal_127_rows/pg_fake | 7.38 ms | +11.79% |
| recursive_cte_branching_traversal_127_rows/postgres_18 | 122.82 us | +1.22% |
| correlated_exists_100_rows/pg_fake | 76.71 us | +5.37% |
| correlated_exists_100_rows/postgres_18 | 114.48 us | +48.79% |
| global_aggregate_100_rows/pg_fake | 40.99 us | +5.48% |
| global_aggregate_100_rows/postgres_18 | 41.45 us | +4.46% |
| grouped_aggregate_100_rows/pg_fake | 174.13 us | +4.51% |
| grouped_aggregate_100_rows/postgres_18 | 50.07 us | +1.96% |
| select_distinct_100_rows/pg_fake | 49.21 us | -1.29% |
| select_distinct_100_rows/postgres_18 | 45.77 us | +1.43% |
| union_all_100_rows/pg_fake | 387.50 us | -4.86% |
| union_all_100_rows/postgres_18 | 87.37 us | +1.18% |
| union_100_rows/pg_fake | 423.29 us | -1.45% |
| union_100_rows/postgres_18 | 87.39 us | +1.88% |
| adapter_overhead_select_100_rows/core | 98.08 us | +5.47% |
| adapter_overhead_select_100_rows/sqlx | 102.85 us | +2.30% |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 16.14 us | +6.94% |
| core_parsed_vs_prepared_point_select/prepared_reuse | 443.56 ns | +16.15% |
| transaction_history_point_select/1 | 248.17 ns | +21.24% |
| transaction_history_point_select/100 | 250.76 ns | +22.53% |
| transaction_history_point_select/10,000 | 247.90 ns | +21.09% |
| transaction_history_point_select/100,000 | 250.29 ns | +21.94% |
| mvcc_old_snapshot_read/1 | 534.94 ns | +24.26% |
| mvcc_old_snapshot_read/100 | 2.46 us | +44.93% |
| mvcc_old_snapshot_read/10,000 | 694.60 us | +3.35% |
| point_lookup_index_vs_scan/heap_scan/100 | 2.88 us | +9.07% |
| point_lookup_index_vs_scan/unique_index/100 | 477.49 ns | +13.89% |
| point_lookup_index_vs_scan/heap_scan/10,000 | 270.95 us | +8.22% |
| point_lookup_index_vs_scan/unique_index/10,000 | 526.77 ns | +12.02% |
| concurrent_uncontended_reads/sequential | 19.71 us | +12.75% |
| concurrent_uncontended_reads/parallel | 16.78 us | +6.26% |
| concurrent_same_row_contention/wait_then_rollback | 1.85 ms | -1.85% |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| skip_locked_queue_100_rows | postgres_18 | pg_fake | 🔴 ↓ 8576.34x |
| lateral_latest_per_parent_100_rows | postgres_18 | pg_fake | 🔴 ↓ 53.34x |
| runtime_temporal_100_rows | postgres_18 | pg_fake | 🔴 ↓ 8.67x |
| runtime_patterns_100_rows | postgres_18 | pg_fake | 🔴 ↓ 35.26x |
| create_table | postgres_18 | pg_fake | 🟢 ↑ 25.43x |
| transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 39.94x |
| migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 3.71x |
| sqlx_migration_chain | postgres_18 | pg_fake | 🔴 ↓ 13.45x |
| procedural_trigger_insert_update | postgres_18 | pg_fake | 🔴 ↓ 176.41x |
| alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.03x |
| partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 8.37x |
| temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 13.03x |
| insert_row | postgres_18 | pg_fake | 🟢 ↑ 2.06x |
| insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 1.90x |
| insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.28x |
| insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.21x |
| insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🟢 ↑ 1.14x |
| insert_on_conflict_do_update | postgres_18 | pg_fake | 🔴 ↓ 1.05x |
| update_row | postgres_18 | pg_fake | 🟢 ↑ 2.42x |
| update_from_row | postgres_18 | pg_fake | 🟢 ↑ 2.37x |
| delete_row | postgres_18 | pg_fake | 🟢 ↑ 5.68x |
| sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.20x |
| catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.78x |
| serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.56x |
| uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.15x |
| offset_datetime_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.17x |
| bigint_uuid_array_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.40x |
| uuid_any_100_rows | postgres_18 | pg_fake | 🔴 ↓ 20.51x |
| ordered_filtered_array_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.24x |
| hashed_advisory_lock_acquisition | postgres_18 | pg_fake | 🔴 ↓ 635.10x |
| json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.69x |
| jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.16x |
| jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.40x |
| jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 4.65x |
| jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 2.09x |
| window_row_number_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.48x |
| ordered_string_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.10x |
| transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.00x |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🔴 ↓ 2797.86x |
| select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.84x |
| select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.85x |
| select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.84x |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.64x |
| nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 12.58x |
| order_by_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.62x |
| foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.43x |
| selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.05x |
| many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.13x |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.40x |
| materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 108.81x |
| data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 24.23x |
| recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 29.73x |
| recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 60.07x |
| correlated_exists_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.49x |
| global_aggregate_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.01x |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.48x |
| select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.08x |
| union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.44x |
| union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.84x |
| adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.05x |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 36.39x |
| transaction_history_point_select | 1 | 100 | 🔴 ↓ 1.01x |
| transaction_history_point_select | 1 | 10,000 | 🟢 ↑ 1.00x |
| transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.01x |
| mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 4.60x |
| mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1298.46x |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 6.03x |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 514.35x |
| concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.17x |
