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
| recorded_at | 2026-09-03T07:41:31Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| create_table/pg_fake | 18.15 us | +1.21% |
| create_table/postgres_18 | 848.85 us | -4.68% |
| insert_row/pg_fake | 29.00 us | +4.68% |
| insert_row/postgres_18 | 87.47 us | +2.25% |
| insert_row_returning/pg_fake | 54.15 us | +5.28% |
| insert_row_returning/postgres_18 | 91.25 us | +0.49% |
| insert_row_with_defaults/pg_fake | 28.53 us | -0.38% |
| insert_row_with_defaults/postgres_18 | 87.82 us | +0.19% |
| update_row/pg_fake | 16.96 us | -20.34% |
| update_row/postgres_18 | 86.82 us | -0.96% |
| update_from_row/pg_fake | 19.40 us | -3.25% |
| update_from_row/postgres_18 | 93.91 us | -0.80% |
| delete_row/pg_fake | 12.55 us | -7.37% |
| delete_row/postgres_18 | 129.79 us | +16.61% |
| sequence_nextval/pg_fake | 13.27 us | -9.29% |
| sequence_nextval/postgres_18 | 26.66 us | -1.29% |
| serial_identity_insert/pg_fake | 12.99 us | -11.99% |
| serial_identity_insert/postgres_18 | 30.39 us | -0.99% |
| uuid_temporal_select/pg_fake | 18.25 us | -6.58% |
| uuid_temporal_select/postgres_18 | 29.17 us | -0.96% |
| transaction_insert/pg_fake | 34.71 us | +1.68% |
| transaction_insert/postgres_18 | 124.97 us | -0.26% |
| transaction_repeatable_read_select_for_update/pg_fake | 27.03 us | -3.07% |
| transaction_repeatable_read_select_for_update/postgres_18 | 77.40 us | -2.34% |
| select_100_rows/pg_fake | 26.71 us | +13.65% |
| select_100_rows/postgres_18 | 55.72 us | -6.93% |
| select_where_100_rows/pg_fake | 15.89 us | +1.52% |
| select_where_100_rows/postgres_18 | 31.66 us | -3.26% |
| select_where_indexed_100_rows/pg_fake | 8.77 us | +2.03% |
| select_where_indexed_100_rows/postgres_18 | 28.55 us | -12.17% |
| limit_offset_ordered_100_rows/pg_fake | 51.55 us | +6.71% |
| limit_offset_ordered_100_rows/postgres_18 | 38.50 us | -1.08% |
| order_by_100_rows/pg_fake | 52.98 us | -0.61% |
| order_by_100_rows/postgres_18 | 63.54 us | -1.26% |
| foreign_key_insert/pg_fake | 47.48 us | -8.80% |
| foreign_key_insert/postgres_18 | 167.74 us | -1.80% |
| selective_inner_join/pg_fake | 65.92 us | -3.52% |
| selective_inner_join/postgres_18 | 38.10 us | -0.53% |
| many_match_inner_join/pg_fake | 104.98 us | -6.95% |
| many_match_inner_join/postgres_18 | 63.96 us | -1.00% |
| derived_and_scalar_subquery_100_rows/pg_fake | 175.71 us | -14.26% |
| derived_and_scalar_subquery_100_rows/postgres_18 | 89.18 us | +5.70% |
| materialized_cte_100_rows/pg_fake | 4.08 ms | N/A |
| materialized_cte_100_rows/postgres_18 | 78.33 us | N/A |
| data_modifying_cte_update_100_rows/pg_fake | 233.03 us | N/A |
| data_modifying_cte_update_100_rows/postgres_18 | 106.49 us | N/A |
| recursive_cte_numeric_series_100_rows/pg_fake | 578.68 us | N/A |
| recursive_cte_numeric_series_100_rows/postgres_18 | 71.95 us | N/A |
| recursive_cte_branching_traversal_127_rows/pg_fake | 6.09 ms | N/A |
| recursive_cte_branching_traversal_127_rows/postgres_18 | 120.73 us | N/A |
| correlated_exists_100_rows/pg_fake | 66.22 us | +5.21% |
| correlated_exists_100_rows/postgres_18 | 81.30 us | -0.45% |
| global_aggregate_100_rows/pg_fake | 70.58 us | -0.40% |
| global_aggregate_100_rows/postgres_18 | 38.52 us | -0.02% |
| grouped_aggregate_100_rows/pg_fake | 90.78 us | +1.39% |
| grouped_aggregate_100_rows/postgres_18 | 46.07 us | +0.36% |
| select_distinct_100_rows/pg_fake | 42.12 us | +0.70% |
| select_distinct_100_rows/postgres_18 | 41.85 us | +0.22% |
| union_all_100_rows/pg_fake | 197.77 us | +1.44% |
| union_all_100_rows/postgres_18 | 85.47 us | -0.41% |
| union_100_rows/pg_fake | 216.90 us | -4.39% |
| union_100_rows/postgres_18 | 85.00 us | +0.13% |
| adapter_overhead_select_100_rows/core | 37.51 us | +0.90% |
| adapter_overhead_select_100_rows/sqlx | 45.17 us | -1.88% |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 12.99 us | +23.01% |
| core_parsed_vs_prepared_point_select/prepared_reuse | 304.76 ns | +5.18% |
| transaction_history_point_select/1 | 126.75 ns | +11.05% |
| transaction_history_point_select/100 | 128.14 ns | +5.03% |
| transaction_history_point_select/10,000 | 125.83 ns | -6.83% |
| transaction_history_point_select/100,000 | 126.93 ns | -10.85% |
| mvcc_old_snapshot_read/1 | 270.48 ns | +15.30% |
| mvcc_old_snapshot_read/100 | 2.85 us | +5.13% |
| mvcc_old_snapshot_read/10,000 | 1.53 ms | +19.41% |
| point_lookup_index_vs_scan/heap_scan/100 | 2.48 us | +13.82% |
| point_lookup_index_vs_scan/unique_index/100 | 376.33 ns | +13.03% |
| point_lookup_index_vs_scan/heap_scan/10,000 | 211.17 us | +1.65% |
| point_lookup_index_vs_scan/unique_index/10,000 | 442.02 ns | +7.83% |
| concurrent_uncontended_reads/sequential | 16.62 us | -0.18% |
| concurrent_uncontended_reads/parallel | 15.48 us | +4.66% |
| concurrent_same_row_contention/wait_then_rollback | 1.47 ms | -13.50% |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 🟢 ↑ 46.77x |
| insert_row | postgres_18 | pg_fake | 🟢 ↑ 3.02x |
| insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 1.69x |
| insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 3.08x |
| update_row | postgres_18 | pg_fake | 🟢 ↑ 5.12x |
| update_from_row | postgres_18 | pg_fake | 🟢 ↑ 4.84x |
| delete_row | postgres_18 | pg_fake | 🟢 ↑ 10.34x |
| sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 2.01x |
| serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 2.34x |
| uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.60x |
| transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 3.60x |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 2.86x |
| select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.09x |
| select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.99x |
| select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 3.25x |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.34x |
| order_by_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.20x |
| foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 3.53x |
| selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 1.73x |
| many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 1.64x |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.97x |
| materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 52.06x |
| data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.19x |
| recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 8.04x |
| recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 50.43x |
| correlated_exists_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.23x |
| global_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.83x |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.97x |
| select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.01x |
| union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.31x |
| union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.55x |
| adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.20x |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 42.61x |
| transaction_history_point_select | 1 | 100 | 🔴 ↓ 1.01x |
| transaction_history_point_select | 1 | 10,000 | 🟢 ↑ 1.01x |
| transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.00x |
| mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 10.54x |
| mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 5672.52x |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 6.60x |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 477.74x |
| concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.07x |
