# Benchmark results

## Environment

| Property | Value |
| --- | --- |
| architecture | aarch64 |
| cpu | Apple M2 |
| criterion | 0.5 |
| logical_cpus | 8 |
| os | macos |
| os_version | Darwin 23.6.0 |
| performance_levels | level 0: 4 physical / 4 logical; level 1: 4 physical / 4 logical |
| physical_cores | 8 |
| postgres_target | 18 |
| recorded_at | 2026-09-05T15:47:10Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| create_table/pg_fake | 55.67 us | +206.73% |
| create_table/postgres_18 | 867.00 us | +2.14% |
| transactional_ddl_create_rollback/pg_fake | 36.31 us | N/A |
| transactional_ddl_create_rollback/postgres_18 | 1.19 ms | N/A |
| migration_table_lock_two_relations/pg_fake | 26.75 us | N/A |
| migration_table_lock_two_relations/postgres_18 | 76.66 us | N/A |
| procedural_trigger_insert_update/pg_fake | 18.89 ms | N/A |
| procedural_trigger_insert_update/postgres_18 | 136.67 us | N/A |
| alter_table_rewrite_100_rows/pg_fake | 471.28 us | N/A |
| alter_table_rewrite_100_rows/postgres_18 | 318.03 us | N/A |
| partial_unique_index_100_rows/pg_fake | 150.14 us | N/A |
| partial_unique_index_100_rows/postgres_18 | 511.31 us | N/A |
| temporary_table_on_commit_drop/pg_fake | 17.57 us | N/A |
| temporary_table_on_commit_drop/postgres_18 | 262.64 us | N/A |
| insert_row/pg_fake | 6.32 ms | +21703.27% |
| insert_row/postgres_18 | 85.39 us | -2.38% |
| insert_row_returning/pg_fake | 7.53 ms | +13805.77% |
| insert_row_returning/postgres_18 | 90.01 us | -1.37% |
| insert_row_with_defaults/pg_fake | 8.52 ms | +29762.72% |
| insert_row_with_defaults/postgres_18 | 87.70 us | -0.14% |
| insert_on_conflict_do_nothing/pg_fake | 29.50 us | N/A |
| insert_on_conflict_do_nothing/postgres_18 | 27.95 us | N/A |
| insert_on_conflict_conflict_free/pg_fake | 62.71 us | N/A |
| insert_on_conflict_conflict_free/postgres_18 | 62.11 us | N/A |
| insert_on_conflict_do_update/pg_fake | 37.29 us | N/A |
| insert_on_conflict_do_update/postgres_18 | 31.95 us | N/A |
| update_row/pg_fake | 43.08 us | +154.03% |
| update_row/postgres_18 | 87.92 us | +1.26% |
| update_from_row/pg_fake | 45.85 us | +136.32% |
| update_from_row/postgres_18 | 94.67 us | +0.81% |
| delete_row/pg_fake | 24.89 us | +98.39% |
| delete_row/postgres_18 | 126.07 us | -2.86% |
| sequence_nextval/pg_fake | 25.26 us | +90.35% |
| sequence_nextval/postgres_18 | 26.35 us | -1.14% |
| serial_identity_insert/pg_fake | 1.20 ms | +9156.90% |
| serial_identity_insert/postgres_18 | 30.58 us | +0.65% |
| uuid_temporal_select/pg_fake | 106.84 us | +485.45% |
| uuid_temporal_select/postgres_18 | 28.69 us | -1.67% |
| json_insert_returning/pg_fake | 47.61 us | N/A |
| json_insert_returning/postgres_18 | 120.94 us | N/A |
| jsonb_insert_returning/pg_fake | 66.38 us | N/A |
| jsonb_insert_returning/postgres_18 | 122.29 us | N/A |
| jsonb_extraction/pg_fake | 96.71 us | N/A |
| jsonb_extraction/postgres_18 | 64.66 us | N/A |
| jsonb_containment/pg_fake | 215.61 us | N/A |
| jsonb_containment/postgres_18 | 41.03 us | N/A |
| jsonb_join_group/pg_fake | 554.91 us | N/A |
| jsonb_join_group/postgres_18 | 330.81 us | N/A |
| transaction_insert/pg_fake | 1.35 ms | +3777.45% |
| transaction_insert/postgres_18 | 125.40 us | +0.35% |
| transaction_repeatable_read_select_for_update/pg_fake | 39.74 us | +47.04% |
| transaction_repeatable_read_select_for_update/postgres_18 | 77.81 us | +0.53% |
| select_100_rows/pg_fake | 97.82 us | +266.18% |
| select_100_rows/postgres_18 | 57.16 us | +2.59% |
| select_where_100_rows/pg_fake | 89.70 us | +464.33% |
| select_where_100_rows/postgres_18 | 32.26 us | +1.89% |
| select_where_indexed_100_rows/pg_fake | 84.24 us | +860.15% |
| select_where_indexed_100_rows/postgres_18 | 32.25 us | +12.95% |
| limit_offset_ordered_100_rows/pg_fake | 143.66 us | +178.67% |
| limit_offset_ordered_100_rows/postgres_18 | 38.71 us | +0.55% |
| nested_filtered_view_100_rows/pg_fake | 235.72 us | N/A |
| nested_filtered_view_100_rows/postgres_18 | 32.85 us | N/A |
| order_by_100_rows/pg_fake | 69.96 us | +32.07% |
| order_by_100_rows/postgres_18 | 65.28 us | +2.73% |
| foreign_key_insert/pg_fake | 11.11 ms | +23297.96% |
| foreign_key_insert/postgres_18 | 168.00 us | +0.16% |
| selective_inner_join/pg_fake | 98.02 us | +48.69% |
| selective_inner_join/postgres_18 | 37.90 us | -0.53% |
| many_match_inner_join/pg_fake | 141.10 us | +34.41% |
| many_match_inner_join/postgres_18 | 64.03 us | +0.11% |
| derived_and_scalar_subquery_100_rows/pg_fake | 271.51 us | +54.52% |
| derived_and_scalar_subquery_100_rows/postgres_18 | 85.13 us | -4.54% |
| materialized_cte_100_rows/pg_fake | 3.51 ms | -13.95% |
| materialized_cte_100_rows/postgres_18 | 77.83 us | -0.65% |
| data_modifying_cte_update_100_rows/pg_fake | 602.55 us | +158.57% |
| data_modifying_cte_update_100_rows/postgres_18 | 102.77 us | -3.50% |
| recursive_cte_numeric_series_100_rows/pg_fake | 1.24 ms | +114.00% |
| recursive_cte_numeric_series_100_rows/postgres_18 | 72.35 us | +0.55% |
| recursive_cte_branching_traversal_127_rows/pg_fake | 5.85 ms | -3.98% |
| recursive_cte_branching_traversal_127_rows/postgres_18 | 120.73 us | 0.00% |
| correlated_exists_100_rows/pg_fake | 92.52 us | +39.70% |
| correlated_exists_100_rows/postgres_18 | 81.74 us | +0.54% |
| global_aggregate_100_rows/pg_fake | 128.15 us | +81.55% |
| global_aggregate_100_rows/postgres_18 | 38.29 us | -0.59% |
| grouped_aggregate_100_rows/pg_fake | 124.54 us | +37.20% |
| grouped_aggregate_100_rows/postgres_18 | 46.02 us | -0.10% |
| select_distinct_100_rows/pg_fake | 57.55 us | +36.62% |
| select_distinct_100_rows/postgres_18 | 41.79 us | -0.14% |
| union_all_100_rows/pg_fake | 489.10 us | +147.31% |
| union_all_100_rows/postgres_18 | 85.93 us | +0.55% |
| union_100_rows/pg_fake | 514.38 us | +137.15% |
| union_100_rows/postgres_18 | 84.56 us | -0.52% |
| adapter_overhead_select_100_rows/core | 56.59 us | +50.87% |
| adapter_overhead_select_100_rows/sqlx | 59.63 us | +32.03% |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 72.93 us | +461.61% |
| core_parsed_vs_prepared_point_select/prepared_reuse | 38.37 us | +12490.86% |
| transaction_history_point_select/1 | 6.39 us | +4943.44% |
| transaction_history_point_select/100 | 6.25 us | +4777.26% |
| transaction_history_point_select/10,000 | 5.35 us | +4155.38% |
| transaction_history_point_select/100,000 | 5.34 us | +4106.29% |
| mvcc_old_snapshot_read/1 | 7.56 us | +2696.57% |
| mvcc_old_snapshot_read/100 | 24.52 us | +759.69% |
| mvcc_old_snapshot_read/10,000 | 2.78 ms | +81.50% |
| point_lookup_index_vs_scan/heap_scan/100 | 10.69 us | +330.54% |
| point_lookup_index_vs_scan/unique_index/100 | 38.23 us | +10058.97% |
| point_lookup_index_vs_scan/heap_scan/10,000 | 399.15 us | +89.02% |
| point_lookup_index_vs_scan/unique_index/10,000 | 4.27 ms | +965164.99% |
| concurrent_uncontended_reads/sequential | 68.83 us | +314.25% |
| concurrent_uncontended_reads/parallel | 62.68 us | +304.96% |
| concurrent_same_row_contention/wait_then_rollback | 1.67 ms | +13.49% |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 🟢 ↑ 15.57x |
| transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 32.80x |
| migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 2.87x |
| procedural_trigger_insert_update | postgres_18 | pg_fake | 🔴 ↓ 138.24x |
| alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.48x |
| partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 3.41x |
| temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 14.95x |
| insert_row | postgres_18 | pg_fake | 🔴 ↓ 74.04x |
| insert_row_returning | postgres_18 | pg_fake | 🔴 ↓ 83.66x |
| insert_row_with_defaults | postgres_18 | pg_fake | 🔴 ↓ 97.15x |
| insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🔴 ↓ 1.06x |
| insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🔴 ↓ 1.01x |
| insert_on_conflict_do_update | postgres_18 | pg_fake | 🔴 ↓ 1.17x |
| update_row | postgres_18 | pg_fake | 🟢 ↑ 2.04x |
| update_from_row | postgres_18 | pg_fake | 🟢 ↑ 2.06x |
| delete_row | postgres_18 | pg_fake | 🟢 ↑ 5.06x |
| sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.04x |
| serial_identity_insert | postgres_18 | pg_fake | 🔴 ↓ 39.33x |
| uuid_temporal_select | postgres_18 | pg_fake | 🔴 ↓ 3.72x |
| json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.54x |
| jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 1.84x |
| jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.50x |
| jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 5.25x |
| jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 1.68x |
| transaction_insert | postgres_18 | pg_fake | 🔴 ↓ 10.73x |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.96x |
| select_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.71x |
| select_where_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.78x |
| select_where_indexed_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.61x |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.71x |
| nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 7.18x |
| order_by_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.07x |
| foreign_key_insert | postgres_18 | pg_fake | 🔴 ↓ 66.13x |
| selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.59x |
| many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.20x |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.19x |
| materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 45.09x |
| data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.86x |
| recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 17.12x |
| recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 48.43x |
| correlated_exists_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.13x |
| global_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.35x |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.71x |
| select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.38x |
| union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.69x |
| union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 6.08x |
| adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.05x |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 1.90x |
| transaction_history_point_select | 1 | 100 | 🟢 ↑ 1.02x |
| transaction_history_point_select | 1 | 10,000 | 🟢 ↑ 1.19x |
| transaction_history_point_select | 1 | 100,000 | 🟢 ↑ 1.20x |
| mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.24x |
| mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 368.14x |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🔴 ↓ 3.57x |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🔴 ↓ 10.69x |
| concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.10x |
