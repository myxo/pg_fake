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
| recorded_at | 2026-09-14T05:22:22Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| skip_locked_queue_100_rows/pg_fake | 108.86 ms | N/A |
| skip_locked_queue_100_rows/postgres_18 | 48.66 us | N/A |
| lateral_latest_per_parent_100_rows/pg_fake | 5.85 ms | N/A |
| lateral_latest_per_parent_100_rows/postgres_18 | 599.23 us | N/A |
| runtime_temporal_100_rows/pg_fake | 1.02 ms | N/A |
| runtime_temporal_100_rows/postgres_18 | 117.54 us | N/A |
| runtime_patterns_100_rows/pg_fake | 4.16 ms | N/A |
| runtime_patterns_100_rows/postgres_18 | 117.61 us | N/A |
| create_table/pg_fake | 36.66 us | +4.49% |
| create_table/postgres_18 | 893.53 us | +7.27% |
| transactional_ddl_create_rollback/pg_fake | 31.77 us | +2.32% |
| transactional_ddl_create_rollback/postgres_18 | 1.43 ms | +15.62% |
| migration_table_lock_two_relations/pg_fake | 25.70 us | +13.79% |
| migration_table_lock_two_relations/postgres_18 | 74.53 us | -1.91% |
| sqlx_migration_chain/pg_fake | 84.39 ms | N/A |
| sqlx_migration_chain/postgres_18 | 6.87 ms | N/A |
| procedural_trigger_insert_update/pg_fake | 21.26 ms | -5.45% |
| procedural_trigger_insert_update/postgres_18 | 122.22 us | +8.25% |
| alter_table_rewrite_100_rows/pg_fake | 301.66 us | +1.39% |
| alter_table_rewrite_100_rows/postgres_18 | 357.90 us | -7.06% |
| partial_unique_index_100_rows/pg_fake | 64.76 us | +1.82% |
| partial_unique_index_100_rows/postgres_18 | 490.46 us | +6.54% |
| temporary_table_on_commit_drop/pg_fake | 17.29 us | +13.63% |
| temporary_table_on_commit_drop/postgres_18 | 213.41 us | -1.07% |
| insert_row/pg_fake | 44.52 us | +16.18% |
| insert_row/postgres_18 | 92.79 us | +8.75% |
| insert_row_returning/pg_fake | 51.15 us | +2.34% |
| insert_row_returning/postgres_18 | 97.60 us | +7.24% |
| insert_row_with_defaults/pg_fake | 44.95 us | +11.55% |
| insert_row_with_defaults/postgres_18 | 94.73 us | +8.41% |
| insert_on_conflict_do_nothing/pg_fake | 21.73 us | +24.18% |
| insert_on_conflict_do_nothing/postgres_18 | 30.81 us | +9.37% |
| insert_on_conflict_conflict_free/pg_fake | 50.53 us | +31.47% |
| insert_on_conflict_conflict_free/postgres_18 | 67.64 us | +8.57% |
| insert_on_conflict_do_update/pg_fake | 35.60 us | +54.53% |
| insert_on_conflict_do_update/postgres_18 | 34.86 us | +8.99% |
| update_row/pg_fake | 36.31 us | +73.99% |
| update_row/postgres_18 | 93.75 us | +7.97% |
| update_from_row/pg_fake | 42.18 us | +85.25% |
| update_from_row/postgres_18 | 101.13 us | +7.36% |
| delete_row/pg_fake | 18.76 us | +18.60% |
| delete_row/postgres_18 | 119.37 us | -6.09% |
| sequence_nextval/pg_fake | 22.70 us | +47.87% |
| sequence_nextval/postgres_18 | 27.93 us | +4.71% |
| serial_identity_insert/pg_fake | 22.43 us | +27.32% |
| serial_identity_insert/postgres_18 | 32.36 us | +6.52% |
| uuid_temporal_select/pg_fake | 25.41 us | +33.41% |
| uuid_temporal_select/postgres_18 | 30.75 us | +6.88% |
| json_insert_returning/pg_fake | 38.76 us | +22.99% |
| json_insert_returning/postgres_18 | 145.94 us | +7.92% |
| jsonb_insert_returning/pg_fake | 45.01 us | +26.07% |
| jsonb_insert_returning/postgres_18 | 145.03 us | +45.08% |
| jsonb_extraction/pg_fake | 93.14 us | +18.21% |
| jsonb_extraction/postgres_18 | 67.28 us | +3.63% |
| jsonb_containment/pg_fake | 204.91 us | +4.56% |
| jsonb_containment/postgres_18 | 43.55 us | +6.35% |
| jsonb_join_group/pg_fake | 707.79 us | +72.54% |
| jsonb_join_group/postgres_18 | 338.70 us | +2.72% |
| window_row_number_100_rows/pg_fake | 128.50 us | N/A |
| window_row_number_100_rows/postgres_18 | 84.46 us | N/A |
| ordered_string_agg_100_rows/pg_fake | 231.77 us | N/A |
| ordered_string_agg_100_rows/postgres_18 | 71.00 us | N/A |
| transaction_insert/pg_fake | 66.13 us | +56.01% |
| transaction_insert/postgres_18 | 136.27 us | +10.51% |
| transaction_repeatable_read_select_for_update/pg_fake | 105.13 ms | +248520.07% |
| transaction_repeatable_read_select_for_update/postgres_18 | 85.72 us | +11.09% |
| select_100_rows/pg_fake | 30.66 us | +2.72% |
| select_100_rows/postgres_18 | 59.53 us | +3.49% |
| select_where_100_rows/pg_fake | 17.27 us | +0.55% |
| select_where_100_rows/postgres_18 | 34.55 us | +6.68% |
| select_where_indexed_100_rows/pg_fake | 9.94 us | +1.50% |
| select_where_indexed_100_rows/postgres_18 | 33.97 us | +20.71% |
| limit_offset_ordered_100_rows/pg_fake | 71.65 us | +30.28% |
| limit_offset_ordered_100_rows/postgres_18 | 42.08 us | +8.90% |
| nested_filtered_view_100_rows/pg_fake | 452.05 us | +153.12% |
| nested_filtered_view_100_rows/postgres_18 | 35.37 us | +5.95% |
| order_by_100_rows/pg_fake | 107.21 us | +87.47% |
| order_by_100_rows/postgres_18 | 67.12 us | +3.61% |
| foreign_key_insert/pg_fake | 70.37 us | +17.09% |
| foreign_key_insert/postgres_18 | 179.30 us | +4.26% |
| selective_inner_join/pg_fake | 82.90 us | +28.42% |
| selective_inner_join/postgres_18 | 38.74 us | +2.17% |
| many_match_inner_join/pg_fake | 136.48 us | +26.43% |
| many_match_inner_join/postgres_18 | 66.47 us | +3.26% |
| derived_and_scalar_subquery_100_rows/pg_fake | 317.60 us | +54.22% |
| derived_and_scalar_subquery_100_rows/postgres_18 | 85.71 us | +20.02% |
| materialized_cte_100_rows/pg_fake | 8.35 ms | +158.30% |
| materialized_cte_100_rows/postgres_18 | 78.46 us | +1.69% |
| data_modifying_cte_update_100_rows/pg_fake | 2.10 ms | +527.17% |
| data_modifying_cte_update_100_rows/postgres_18 | 100.87 us | -2.42% |
| recursive_cte_numeric_series_100_rows/pg_fake | 2.17 ms | +182.23% |
| recursive_cte_numeric_series_100_rows/postgres_18 | 74.44 us | +2.02% |
| recursive_cte_branching_traversal_127_rows/pg_fake | 6.60 ms | +32.81% |
| recursive_cte_branching_traversal_127_rows/postgres_18 | 121.34 us | +2.06% |
| correlated_exists_100_rows/pg_fake | 72.80 us | +8.43% |
| correlated_exists_100_rows/postgres_18 | 76.94 us | +14.78% |
| global_aggregate_100_rows/pg_fake | 38.86 us | +2.21% |
| global_aggregate_100_rows/postgres_18 | 39.68 us | +4.00% |
| grouped_aggregate_100_rows/pg_fake | 166.62 us | +91.40% |
| grouped_aggregate_100_rows/postgres_18 | 49.11 us | +8.09% |
| select_distinct_100_rows/pg_fake | 49.85 us | +14.71% |
| select_distinct_100_rows/postgres_18 | 45.12 us | +8.98% |
| union_all_100_rows/pg_fake | 407.31 us | +89.45% |
| union_all_100_rows/postgres_18 | 86.35 us | +2.11% |
| union_100_rows/pg_fake | 429.51 us | +80.77% |
| union_100_rows/postgres_18 | 85.78 us | +1.26% |
| adapter_overhead_select_100_rows/core | 92.99 us | +132.87% |
| adapter_overhead_select_100_rows/sqlx | 100.54 us | +107.76% |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 15.09 us | +0.25% |
| core_parsed_vs_prepared_point_select/prepared_reuse | 381.90 ns | -3.98% |
| transaction_history_point_select/1 | 204.69 ns | -0.64% |
| transaction_history_point_select/100 | 204.66 ns | -0.42% |
| transaction_history_point_select/10,000 | 204.72 ns | -1.16% |
| transaction_history_point_select/100,000 | 205.25 ns | -1.10% |
| mvcc_old_snapshot_read/1 | 430.49 ns | +0.04% |
| mvcc_old_snapshot_read/100 | 1.70 us | +0.91% |
| mvcc_old_snapshot_read/10,000 | 672.08 us | -0.48% |
| point_lookup_index_vs_scan/heap_scan/100 | 2.64 us | -0.32% |
| point_lookup_index_vs_scan/unique_index/100 | 419.26 ns | -6.09% |
| point_lookup_index_vs_scan/heap_scan/10,000 | 250.37 us | +1.51% |
| point_lookup_index_vs_scan/unique_index/10,000 | 470.24 ns | -4.95% |
| concurrent_uncontended_reads/sequential | 17.48 us | -1.57% |
| concurrent_uncontended_reads/parallel | 15.79 us | -2.13% |
| concurrent_same_row_contention/wait_then_rollback | 1.88 ms | +10.81% |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| skip_locked_queue_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2237.04x |
| lateral_latest_per_parent_100_rows | postgres_18 | pg_fake | 🔴 ↓ 9.76x |
| runtime_temporal_100_rows | postgres_18 | pg_fake | 🔴 ↓ 8.65x |
| runtime_patterns_100_rows | postgres_18 | pg_fake | 🔴 ↓ 35.35x |
| create_table | postgres_18 | pg_fake | 🟢 ↑ 24.37x |
| transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 44.86x |
| migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 2.90x |
| sqlx_migration_chain | postgres_18 | pg_fake | 🔴 ↓ 12.28x |
| procedural_trigger_insert_update | postgres_18 | pg_fake | 🔴 ↓ 173.91x |
| alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.19x |
| partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 7.57x |
| temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 12.35x |
| insert_row | postgres_18 | pg_fake | 🟢 ↑ 2.08x |
| insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 1.91x |
| insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.11x |
| insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.42x |
| insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🟢 ↑ 1.34x |
| insert_on_conflict_do_update | postgres_18 | pg_fake | 🔴 ↓ 1.02x |
| update_row | postgres_18 | pg_fake | 🟢 ↑ 2.58x |
| update_from_row | postgres_18 | pg_fake | 🟢 ↑ 2.40x |
| delete_row | postgres_18 | pg_fake | 🟢 ↑ 6.36x |
| sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.23x |
| serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.44x |
| uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.21x |
| json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.77x |
| jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.22x |
| jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.38x |
| jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 4.71x |
| jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 2.09x |
| window_row_number_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.52x |
| ordered_string_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.26x |
| transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.06x |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🔴 ↓ 1226.52x |
| select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.94x |
| select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.00x |
| select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 3.42x |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.70x |
| nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 12.78x |
| order_by_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.60x |
| foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.55x |
| selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.14x |
| many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.05x |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.71x |
| materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 106.43x |
| data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 20.85x |
| recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 29.17x |
| recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 54.39x |
| correlated_exists_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.06x |
| global_aggregate_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.02x |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.39x |
| select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.10x |
| union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.72x |
| union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.01x |
| adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.08x |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 39.52x |
| transaction_history_point_select | 1 | 100 | 🟢 ↑ 1.00x |
| transaction_history_point_select | 1 | 10,000 | 🔴 ↓ 1.00x |
| transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.00x |
| mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.94x |
| mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1561.21x |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 6.29x |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 532.43x |
| concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.11x |
