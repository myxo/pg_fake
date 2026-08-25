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
| recorded_at | 2026-08-25T20:09:11Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average |
| --- | ---: |
| create_table/pg_fake | 17.93 us |
| create_table/postgres_18 | 890.54 us |
| insert_row/pg_fake | 27.70 us |
| insert_row/postgres_18 | 85.55 us |
| insert_row_returning/pg_fake | 51.44 us |
| insert_row_returning/postgres_18 | 90.81 us |
| insert_row_with_defaults/pg_fake | 28.64 us |
| insert_row_with_defaults/postgres_18 | 87.66 us |
| update_row/pg_fake | 21.29 us |
| update_row/postgres_18 | 87.66 us |
| update_from_row/pg_fake | 20.06 us |
| update_from_row/postgres_18 | 94.67 us |
| delete_row/pg_fake | 13.55 us |
| delete_row/postgres_18 | 111.30 us |
| sequence_nextval/pg_fake | 14.63 us |
| sequence_nextval/postgres_18 | 27.01 us |
| serial_identity_insert/pg_fake | 14.77 us |
| serial_identity_insert/postgres_18 | 30.69 us |
| uuid_temporal_select/pg_fake | 19.54 us |
| uuid_temporal_select/postgres_18 | 29.46 us |
| transaction_insert/pg_fake | 34.14 us |
| transaction_insert/postgres_18 | 125.30 us |
| transaction_repeatable_read_select_for_update/pg_fake | 27.88 us |
| transaction_repeatable_read_select_for_update/postgres_18 | 79.26 us |
| select_100_rows/pg_fake | 23.51 us |
| select_100_rows/postgres_18 | 59.87 us |
| select_where_100_rows/pg_fake | 15.66 us |
| select_where_100_rows/postgres_18 | 32.73 us |
| select_where_indexed_100_rows/pg_fake | 8.60 us |
| select_where_indexed_100_rows/postgres_18 | 32.51 us |
| limit_offset_ordered_100_rows/pg_fake | 48.31 us |
| limit_offset_ordered_100_rows/postgres_18 | 38.92 us |
| order_by_100_rows/pg_fake | 53.30 us |
| order_by_100_rows/postgres_18 | 64.35 us |
| foreign_key_insert/pg_fake | 52.06 us |
| foreign_key_insert/postgres_18 | 170.81 us |
| selective_inner_join/pg_fake | 68.33 us |
| selective_inner_join/postgres_18 | 38.30 us |
| many_match_inner_join/pg_fake | 112.82 us |
| many_match_inner_join/postgres_18 | 64.61 us |
| derived_and_scalar_subquery_100_rows/pg_fake | 204.94 us |
| derived_and_scalar_subquery_100_rows/postgres_18 | 84.37 us |
| correlated_exists_100_rows/pg_fake | 62.94 us |
| correlated_exists_100_rows/postgres_18 | 81.67 us |
| global_aggregate_100_rows/pg_fake | 70.86 us |
| global_aggregate_100_rows/postgres_18 | 38.52 us |
| grouped_aggregate_100_rows/pg_fake | 89.53 us |
| grouped_aggregate_100_rows/postgres_18 | 45.90 us |
| select_distinct_100_rows/pg_fake | 41.83 us |
| select_distinct_100_rows/postgres_18 | 41.76 us |
| union_all_100_rows/pg_fake | 194.95 us |
| union_all_100_rows/postgres_18 | 85.82 us |
| union_100_rows/pg_fake | 226.86 us |
| union_100_rows/postgres_18 | 84.89 us |
| adapter_overhead_select_100_rows/core | 37.17 us |
| adapter_overhead_select_100_rows/sqlx | 46.03 us |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 10.56 us |
| core_parsed_vs_prepared_point_select/prepared_reuse | 289.75 ns |
| transaction_history_point_select/1 | 114.14 ns |
| transaction_history_point_select/100 | 122.01 ns |
| transaction_history_point_select/10,000 | 135.06 ns |
| transaction_history_point_select/100,000 | 142.37 ns |
| mvcc_old_snapshot_read/1 | 234.59 ns |
| mvcc_old_snapshot_read/100 | 2.71 us |
| mvcc_old_snapshot_read/10,000 | 1.28 ms |
| point_lookup_index_vs_scan/heap_scan/100 | 2.18 us |
| point_lookup_index_vs_scan/unique_index/100 | 332.95 ns |
| point_lookup_index_vs_scan/heap_scan/10,000 | 207.75 us |
| point_lookup_index_vs_scan/unique_index/10,000 | 409.92 ns |
| concurrent_uncontended_reads/sequential | 16.65 us |
| concurrent_uncontended_reads/parallel | 14.79 us |
| concurrent_same_row_contention/wait_then_rollback | 1.70 ms |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 49.66x faster |
| insert_row | postgres_18 | pg_fake | 3.09x faster |
| insert_row_returning | postgres_18 | pg_fake | 1.77x faster |
| insert_row_with_defaults | postgres_18 | pg_fake | 3.06x faster |
| update_row | postgres_18 | pg_fake | 4.12x faster |
| update_from_row | postgres_18 | pg_fake | 4.72x faster |
| delete_row | postgres_18 | pg_fake | 8.22x faster |
| sequence_nextval | postgres_18 | pg_fake | 1.85x faster |
| serial_identity_insert | postgres_18 | pg_fake | 2.08x faster |
| uuid_temporal_select | postgres_18 | pg_fake | 1.51x faster |
| transaction_insert | postgres_18 | pg_fake | 3.67x faster |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 2.84x faster |
| select_100_rows | postgres_18 | pg_fake | 2.55x faster |
| select_where_100_rows | postgres_18 | pg_fake | 2.09x faster |
| select_where_indexed_100_rows | postgres_18 | pg_fake | 3.78x faster |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 1.24x slower |
| order_by_100_rows | postgres_18 | pg_fake | 1.21x faster |
| foreign_key_insert | postgres_18 | pg_fake | 3.28x faster |
| selective_inner_join | postgres_18 | pg_fake | 1.78x slower |
| many_match_inner_join | postgres_18 | pg_fake | 1.75x slower |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 2.43x slower |
| correlated_exists_100_rows | postgres_18 | pg_fake | 1.30x faster |
| global_aggregate_100_rows | postgres_18 | pg_fake | 1.84x slower |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 1.95x slower |
| select_distinct_100_rows | postgres_18 | pg_fake | 1.00x slower |
| union_all_100_rows | postgres_18 | pg_fake | 2.27x slower |
| union_100_rows | postgres_18 | pg_fake | 2.67x slower |
| adapter_overhead_select_100_rows | core | sqlx | 1.24x slower |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 36.44x faster |
| transaction_history_point_select | 1 | 100 | 1.07x slower |
| transaction_history_point_select | 1 | 10,000 | 1.18x slower |
| transaction_history_point_select | 1 | 100,000 | 1.25x slower |
| mvcc_old_snapshot_read | 1 | 100 | 11.56x slower |
| mvcc_old_snapshot_read | 1 | 10,000 | 5477.32x slower |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 6.55x faster |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 506.81x faster |
| concurrent_uncontended_reads | sequential | parallel | 1.13x faster |
