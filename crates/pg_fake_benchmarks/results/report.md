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
| recorded_at | 2026-09-06T06:10:16Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| create_table/pg_fake | 35.09 us | -36.97% |
| create_table/postgres_18 | 832.95 us | -3.93% |
| transactional_ddl_create_rollback/pg_fake | 31.05 us | -14.49% |
| transactional_ddl_create_rollback/postgres_18 | 1.23 ms | +3.51% |
| migration_table_lock_two_relations/pg_fake | 22.59 us | -15.55% |
| migration_table_lock_two_relations/postgres_18 | 75.98 us | -0.88% |
| procedural_trigger_insert_update/pg_fake | 22.48 ms | +18.99% |
| procedural_trigger_insert_update/postgres_18 | 112.91 us | -17.38% |
| alter_table_rewrite_100_rows/pg_fake | 297.53 us | -36.87% |
| alter_table_rewrite_100_rows/postgres_18 | 385.07 us | +21.08% |
| partial_unique_index_100_rows/pg_fake | 63.61 us | -57.64% |
| partial_unique_index_100_rows/postgres_18 | 460.34 us | -9.97% |
| temporary_table_on_commit_drop/pg_fake | 15.21 us | -13.41% |
| temporary_table_on_commit_drop/postgres_18 | 215.70 us | -17.87% |
| insert_row/pg_fake | 38.32 us | -99.39% |
| insert_row/postgres_18 | 85.32 us | -0.08% |
| insert_row_returning/pg_fake | 49.98 us | -99.34% |
| insert_row_returning/postgres_18 | 91.01 us | +1.11% |
| insert_row_with_defaults/pg_fake | 40.29 us | -99.53% |
| insert_row_with_defaults/postgres_18 | 87.38 us | -0.36% |
| insert_on_conflict_do_nothing/pg_fake | 17.50 us | -40.68% |
| insert_on_conflict_do_nothing/postgres_18 | 28.17 us | +0.78% |
| insert_on_conflict_conflict_free/pg_fake | 38.43 us | -38.71% |
| insert_on_conflict_conflict_free/postgres_18 | 62.30 us | +0.31% |
| insert_on_conflict_do_update/pg_fake | 23.04 us | -38.23% |
| insert_on_conflict_do_update/postgres_18 | 31.98 us | +0.10% |
| update_row/pg_fake | 20.87 us | -51.56% |
| update_row/postgres_18 | 86.83 us | -1.24% |
| update_from_row/pg_fake | 22.77 us | -50.35% |
| update_from_row/postgres_18 | 94.20 us | -0.49% |
| delete_row/pg_fake | 15.82 us | -36.45% |
| delete_row/postgres_18 | 127.12 us | +0.83% |
| sequence_nextval/pg_fake | 15.35 us | -39.22% |
| sequence_nextval/postgres_18 | 26.67 us | +1.21% |
| serial_identity_insert/pg_fake | 17.62 us | -98.54% |
| serial_identity_insert/postgres_18 | 30.38 us | -0.68% |
| uuid_temporal_select/pg_fake | 19.05 us | -82.17% |
| uuid_temporal_select/postgres_18 | 28.77 us | +0.29% |
| json_insert_returning/pg_fake | 31.51 us | -33.82% |
| json_insert_returning/postgres_18 | 135.23 us | +11.82% |
| jsonb_insert_returning/pg_fake | 35.71 us | -46.21% |
| jsonb_insert_returning/postgres_18 | 99.96 us | -18.26% |
| jsonb_extraction/pg_fake | 78.80 us | -18.52% |
| jsonb_extraction/postgres_18 | 64.93 us | +0.42% |
| jsonb_containment/pg_fake | 195.96 us | -9.11% |
| jsonb_containment/postgres_18 | 40.95 us | -0.19% |
| jsonb_join_group/pg_fake | 410.23 us | -26.07% |
| jsonb_join_group/postgres_18 | 329.75 us | -0.32% |
| transaction_insert/pg_fake | 42.39 us | -96.85% |
| transaction_insert/postgres_18 | 123.32 us | -1.66% |
| transaction_repeatable_read_select_for_update/pg_fake | 42.29 us | +6.41% |
| transaction_repeatable_read_select_for_update/postgres_18 | 77.16 us | -0.84% |
| select_100_rows/pg_fake | 29.84 us | -69.49% |
| select_100_rows/postgres_18 | 57.52 us | +0.63% |
| select_where_100_rows/pg_fake | 17.17 us | -80.86% |
| select_where_100_rows/postgres_18 | 32.39 us | +0.41% |
| select_where_indexed_100_rows/pg_fake | 9.80 us | -88.37% |
| select_where_indexed_100_rows/postgres_18 | 28.14 us | -12.74% |
| limit_offset_ordered_100_rows/pg_fake | 55.00 us | -61.72% |
| limit_offset_ordered_100_rows/postgres_18 | 38.64 us | -0.18% |
| nested_filtered_view_100_rows/pg_fake | 178.60 us | -24.23% |
| nested_filtered_view_100_rows/postgres_18 | 33.39 us | +1.64% |
| order_by_100_rows/pg_fake | 57.19 us | -18.26% |
| order_by_100_rows/postgres_18 | 64.78 us | -0.76% |
| foreign_key_insert/pg_fake | 60.10 us | -99.46% |
| foreign_key_insert/postgres_18 | 171.98 us | +2.37% |
| selective_inner_join/pg_fake | 64.55 us | -34.14% |
| selective_inner_join/postgres_18 | 37.92 us | +0.06% |
| many_match_inner_join/pg_fake | 107.94 us | -23.50% |
| many_match_inner_join/postgres_18 | 64.37 us | +0.53% |
| derived_and_scalar_subquery_100_rows/pg_fake | 205.94 us | -24.15% |
| derived_and_scalar_subquery_100_rows/postgres_18 | 71.41 us | -16.11% |
| materialized_cte_100_rows/pg_fake | 3.23 ms | -7.88% |
| materialized_cte_100_rows/postgres_18 | 77.15 us | -0.86% |
| data_modifying_cte_update_100_rows/pg_fake | 335.36 us | -44.34% |
| data_modifying_cte_update_100_rows/postgres_18 | 103.37 us | +0.59% |
| recursive_cte_numeric_series_100_rows/pg_fake | 769.25 us | -37.88% |
| recursive_cte_numeric_series_100_rows/postgres_18 | 72.96 us | +0.85% |
| recursive_cte_branching_traversal_127_rows/pg_fake | 4.97 ms | -15.01% |
| recursive_cte_branching_traversal_127_rows/postgres_18 | 118.90 us | -1.52% |
| correlated_exists_100_rows/pg_fake | 67.14 us | -27.43% |
| correlated_exists_100_rows/postgres_18 | 67.04 us | -17.99% |
| global_aggregate_100_rows/pg_fake | 38.03 us | -70.33% |
| global_aggregate_100_rows/postgres_18 | 38.15 us | -0.34% |
| grouped_aggregate_100_rows/pg_fake | 87.06 us | -30.10% |
| grouped_aggregate_100_rows/postgres_18 | 45.43 us | -1.29% |
| select_distinct_100_rows/pg_fake | 43.46 us | -24.49% |
| select_distinct_100_rows/postgres_18 | 41.40 us | -0.93% |
| union_all_100_rows/pg_fake | 215.00 us | -56.04% |
| union_all_100_rows/postgres_18 | 84.57 us | -1.59% |
| union_100_rows/pg_fake | 237.60 us | -53.81% |
| union_100_rows/postgres_18 | 84.72 us | +0.19% |
| adapter_overhead_select_100_rows/core | 39.93 us | -29.43% |
| adapter_overhead_select_100_rows/sqlx | 48.39 us | -18.85% |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 15.06 us | -79.36% |
| core_parsed_vs_prepared_point_select/prepared_reuse | 397.72 ns | -98.96% |
| transaction_history_point_select/1 | 206.02 ns | -96.78% |
| transaction_history_point_select/100 | 205.52 ns | -96.71% |
| transaction_history_point_select/10,000 | 207.12 ns | -96.13% |
| transaction_history_point_select/100,000 | 207.54 ns | -96.11% |
| mvcc_old_snapshot_read/1 | 430.31 ns | -94.31% |
| mvcc_old_snapshot_read/100 | 1.68 us | -93.14% |
| mvcc_old_snapshot_read/10,000 | 675.30 us | -75.75% |
| point_lookup_index_vs_scan/heap_scan/100 | 2.65 us | -75.24% |
| point_lookup_index_vs_scan/unique_index/100 | 446.44 ns | -98.83% |
| point_lookup_index_vs_scan/heap_scan/10,000 | 246.64 us | -38.21% |
| point_lookup_index_vs_scan/unique_index/10,000 | 494.70 ns | -99.99% |
| concurrent_uncontended_reads/sequential | 17.76 us | -74.19% |
| concurrent_uncontended_reads/parallel | 16.13 us | -74.26% |
| concurrent_same_row_contention/wait_then_rollback | 1.70 ms | +2.00% |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 🟢 ↑ 23.74x |
| transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 39.70x |
| migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 3.36x |
| procedural_trigger_insert_update | postgres_18 | pg_fake | 🔴 ↓ 199.10x |
| alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.29x |
| partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 7.24x |
| temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 14.18x |
| insert_row | postgres_18 | pg_fake | 🟢 ↑ 2.23x |
| insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 1.82x |
| insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.17x |
| insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.61x |
| insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🟢 ↑ 1.62x |
| insert_on_conflict_do_update | postgres_18 | pg_fake | 🟢 ↑ 1.39x |
| update_row | postgres_18 | pg_fake | 🟢 ↑ 4.16x |
| update_from_row | postgres_18 | pg_fake | 🟢 ↑ 4.14x |
| delete_row | postgres_18 | pg_fake | 🟢 ↑ 8.04x |
| sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.74x |
| serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.72x |
| uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.51x |
| json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 4.29x |
| jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.80x |
| jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.21x |
| jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 4.79x |
| jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 1.24x |
| transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.91x |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.82x |
| select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.93x |
| select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.89x |
| select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.87x |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.42x |
| nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.35x |
| order_by_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.13x |
| foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.86x |
| selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 1.70x |
| many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 1.68x |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.88x |
| materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 41.90x |
| data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.24x |
| recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 10.54x |
| recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 41.79x |
| correlated_exists_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.00x |
| global_aggregate_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.00x |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.92x |
| select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.05x |
| union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.54x |
| union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.80x |
| adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.21x |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 37.85x |
| transaction_history_point_select | 1 | 100 | 🟢 ↑ 1.00x |
| transaction_history_point_select | 1 | 10,000 | 🔴 ↓ 1.01x |
| transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.01x |
| mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.91x |
| mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1569.33x |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.93x |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 498.56x |
| concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.10x |
