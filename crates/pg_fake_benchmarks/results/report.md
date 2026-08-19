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
| recorded_at | 2026-08-19T18:20:28Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average |
| --- | ---: |
| create_table/pg_fake | 19.30 us |
| create_table/postgres_18 | 917.72 us |
| insert_row/pg_fake | 32.13 us |
| insert_row/postgres_18 | 86.36 us |
| insert_row_returning/pg_fake | 47.94 us |
| insert_row_returning/postgres_18 | 91.28 us |
| insert_row_with_defaults/pg_fake | 35.52 us |
| insert_row_with_defaults/postgres_18 | 88.01 us |
| update_row/pg_fake | 24.86 us |
| update_row/postgres_18 | 89.72 us |
| update_from_row/pg_fake | 27.57 us |
| update_from_row/postgres_18 | 94.21 us |
| delete_row/pg_fake | 16.95 us |
| delete_row/postgres_18 | 127.53 us |
| sequence_nextval/pg_fake | 16.92 us |
| sequence_nextval/postgres_18 | 26.79 us |
| serial_identity_insert/pg_fake | 17.08 us |
| serial_identity_insert/postgres_18 | 30.76 us |
| uuid_temporal_select/pg_fake | 79.50 us |
| uuid_temporal_select/postgres_18 | 29.12 us |
| transaction_insert/pg_fake | 40.67 us |
| transaction_insert/postgres_18 | 126.26 us |
| transaction_repeatable_read_select_for_update/pg_fake | 29.53 us |
| transaction_repeatable_read_select_for_update/postgres_18 | 78.59 us |
| select_100_rows/pg_fake | 39.90 us |
| select_100_rows/postgres_18 | 57.93 us |
| limit_offset_ordered_100_rows/pg_fake | 74.44 us |
| limit_offset_ordered_100_rows/postgres_18 | 38.60 us |
| order_by_100_rows/pg_fake | 81.23 us |
| order_by_100_rows/postgres_18 | 64.85 us |
| adapter_overhead_select_100_rows/core | 63.58 us |
| adapter_overhead_select_100_rows/sqlx | 73.38 us |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 79.96 us |
| core_parsed_vs_prepared_point_select/prepared_reuse | 68.84 us |
| transaction_history_point_select/1 | 4.40 us |
| transaction_history_point_select/100 | 4.42 us |
| transaction_history_point_select/10,000 | 4.35 us |
| transaction_history_point_select/100,000 | 4.40 us |
| mvcc_old_snapshot_read/1 | 8.20 us |
| mvcc_old_snapshot_read/100 | 9.55 us |
| mvcc_old_snapshot_read/10,000 | 682.20 us |
| point_lookup_index_vs_scan/heap_scan/100 | 69.17 us |
| point_lookup_index_vs_scan/unique_index/100 | 69.98 us |
| point_lookup_index_vs_scan/heap_scan/10,000 | 6.11 ms |
| point_lookup_index_vs_scan/unique_index/10,000 | 6.08 ms |
| concurrent_uncontended_reads/sequential | 42.39 us |
| concurrent_uncontended_reads/parallel | 39.51 us |
| concurrent_same_row_contention/wait_then_rollback | 1.67 ms |
| foreign_key_insert/pg_fake | 57.82 us |
| foreign_key_insert/postgres_18 | 168.92 us |
| selective_inner_join/pg_fake | 126.01 us |
| selective_inner_join/postgres_18 | 38.93 us |
| many_match_inner_join/pg_fake | 237.76 us |
| many_match_inner_join/postgres_18 | 64.87 us |
| derived_and_scalar_subquery_100_rows/pg_fake | 430.48 us |
| derived_and_scalar_subquery_100_rows/postgres_18 | 85.43 us |
| correlated_exists_100_rows/pg_fake | 94.25 us |
| correlated_exists_100_rows/postgres_18 | 81.85 us |
| global_aggregate_100_rows/pg_fake | 124.03 us |
| global_aggregate_100_rows/postgres_18 | 38.96 us |
| grouped_aggregate_100_rows/pg_fake | 125.88 us |
| grouped_aggregate_100_rows/postgres_18 | 46.21 us |
| select_distinct_100_rows/pg_fake | 58.99 us |
| select_distinct_100_rows/postgres_18 | 42.20 us |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 47.55x faster |
| insert_row | postgres_18 | pg_fake | 2.69x faster |
| insert_row_returning | postgres_18 | pg_fake | 1.90x faster |
| insert_row_with_defaults | postgres_18 | pg_fake | 2.48x faster |
| update_row | postgres_18 | pg_fake | 3.61x faster |
| update_from_row | postgres_18 | pg_fake | 3.42x faster |
| delete_row | postgres_18 | pg_fake | 7.53x faster |
| sequence_nextval | postgres_18 | pg_fake | 1.58x faster |
| serial_identity_insert | postgres_18 | pg_fake | 1.80x faster |
| uuid_temporal_select | postgres_18 | pg_fake | 2.73x slower |
| transaction_insert | postgres_18 | pg_fake | 3.10x faster |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 2.66x faster |
| select_100_rows | postgres_18 | pg_fake | 1.45x faster |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 1.93x slower |
| order_by_100_rows | postgres_18 | pg_fake | 1.25x slower |
| adapter_overhead_select_100_rows | core | sqlx | 1.15x slower |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 1.16x faster |
| transaction_history_point_select | 1 | 100 | 1.00x slower |
| transaction_history_point_select | 1 | 10,000 | 1.01x faster |
| transaction_history_point_select | 1 | 100,000 | 1.00x faster |
| mvcc_old_snapshot_read | 1 | 100 | 1.16x slower |
| mvcc_old_snapshot_read | 1 | 10,000 | 83.23x slower |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 1.01x slower |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 1.00x faster |
| concurrent_uncontended_reads | sequential | parallel | 1.07x faster |
| foreign_key_insert | postgres_18 | pg_fake | 2.92x faster |
| selective_inner_join | postgres_18 | pg_fake | 3.24x slower |
| many_match_inner_join | postgres_18 | pg_fake | 3.67x slower |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 5.04x slower |
| correlated_exists_100_rows | postgres_18 | pg_fake | 1.15x slower |
| global_aggregate_100_rows | postgres_18 | pg_fake | 3.18x slower |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 2.72x slower |
| select_distinct_100_rows | postgres_18 | pg_fake | 1.40x slower |
