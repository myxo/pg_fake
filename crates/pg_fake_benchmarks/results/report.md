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
| recorded_at | 2026-08-20T05:05:34Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average |
| --- | ---: |
| create_table/pg_fake | 18.14 us |
| create_table/postgres_18 | 942.86 us |
| insert_row/pg_fake | 26.47 us |
| insert_row/postgres_18 | 89.75 us |
| insert_row_returning/pg_fake | 42.87 us |
| insert_row_returning/postgres_18 | 90.61 us |
| insert_row_with_defaults/pg_fake | 28.66 us |
| insert_row_with_defaults/postgres_18 | 87.83 us |
| update_row/pg_fake | 18.60 us |
| update_row/postgres_18 | 87.86 us |
| update_from_row/pg_fake | 19.92 us |
| update_from_row/postgres_18 | 95.02 us |
| delete_row/pg_fake | 13.34 us |
| delete_row/postgres_18 | 127.15 us |
| sequence_nextval/pg_fake | 14.15 us |
| sequence_nextval/postgres_18 | 26.82 us |
| serial_identity_insert/pg_fake | 14.49 us |
| serial_identity_insert/postgres_18 | 30.30 us |
| uuid_temporal_select/pg_fake | 18.68 us |
| uuid_temporal_select/postgres_18 | 28.95 us |
| transaction_insert/pg_fake | 34.77 us |
| transaction_insert/postgres_18 | 126.53 us |
| transaction_repeatable_read_select_for_update/pg_fake | 28.33 us |
| transaction_repeatable_read_select_for_update/postgres_18 | 77.40 us |
| select_100_rows/pg_fake | 37.33 us |
| select_100_rows/postgres_18 | 57.89 us |
| limit_offset_ordered_100_rows/pg_fake | 48.27 us |
| limit_offset_ordered_100_rows/postgres_18 | 38.80 us |
| order_by_100_rows/pg_fake | 54.16 us |
| order_by_100_rows/postgres_18 | 64.66 us |
| adapter_overhead_select_100_rows/core | 37.63 us |
| adapter_overhead_select_100_rows/sqlx | 46.33 us |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 15.95 us |
| core_parsed_vs_prepared_point_select/prepared_reuse | 5.59 us |
| transaction_history_point_select/1 | 3.00 us |
| transaction_history_point_select/100 | 2.99 us |
| transaction_history_point_select/10,000 | 3.11 us |
| transaction_history_point_select/100,000 | 3.09 us |
| mvcc_old_snapshot_read/1 | 4.78 us |
| mvcc_old_snapshot_read/100 | 7.27 us |
| mvcc_old_snapshot_read/10,000 | 1.34 ms |
| point_lookup_index_vs_scan/heap_scan/100 | 23.57 us |
| point_lookup_index_vs_scan/unique_index/100 | 5.58 us |
| point_lookup_index_vs_scan/heap_scan/10,000 | 1.92 ms |
| point_lookup_index_vs_scan/unique_index/10,000 | 5.58 us |
| concurrent_uncontended_reads/sequential | 33.47 us |
| concurrent_uncontended_reads/parallel | 30.19 us |
| concurrent_same_row_contention/wait_then_rollback | 1.56 ms |
| foreign_key_insert/pg_fake | 50.78 us |
| foreign_key_insert/postgres_18 | 169.37 us |
| selective_inner_join/pg_fake | 66.30 us |
| selective_inner_join/postgres_18 | 38.29 us |
| many_match_inner_join/pg_fake | 110.48 us |
| many_match_inner_join/postgres_18 | 64.55 us |
| derived_and_scalar_subquery_100_rows/pg_fake | 208.03 us |
| derived_and_scalar_subquery_100_rows/postgres_18 | 84.63 us |
| correlated_exists_100_rows/pg_fake | 61.67 us |
| correlated_exists_100_rows/postgres_18 | 81.67 us |
| global_aggregate_100_rows/pg_fake | 69.14 us |
| global_aggregate_100_rows/postgres_18 | 37.52 us |
| grouped_aggregate_100_rows/pg_fake | 89.16 us |
| grouped_aggregate_100_rows/postgres_18 | 46.06 us |
| select_distinct_100_rows/pg_fake | 42.60 us |
| select_distinct_100_rows/postgres_18 | 41.75 us |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 51.97x faster |
| insert_row | postgres_18 | pg_fake | 3.39x faster |
| insert_row_returning | postgres_18 | pg_fake | 2.11x faster |
| insert_row_with_defaults | postgres_18 | pg_fake | 3.06x faster |
| update_row | postgres_18 | pg_fake | 4.72x faster |
| update_from_row | postgres_18 | pg_fake | 4.77x faster |
| delete_row | postgres_18 | pg_fake | 9.53x faster |
| sequence_nextval | postgres_18 | pg_fake | 1.90x faster |
| serial_identity_insert | postgres_18 | pg_fake | 2.09x faster |
| uuid_temporal_select | postgres_18 | pg_fake | 1.55x faster |
| transaction_insert | postgres_18 | pg_fake | 3.64x faster |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 2.73x faster |
| select_100_rows | postgres_18 | pg_fake | 1.55x faster |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 1.24x slower |
| order_by_100_rows | postgres_18 | pg_fake | 1.19x faster |
| adapter_overhead_select_100_rows | core | sqlx | 1.23x slower |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 2.85x faster |
| transaction_history_point_select | 1 | 100 | 1.00x faster |
| transaction_history_point_select | 1 | 10,000 | 1.04x slower |
| transaction_history_point_select | 1 | 100,000 | 1.03x slower |
| mvcc_old_snapshot_read | 1 | 100 | 1.52x slower |
| mvcc_old_snapshot_read | 1 | 10,000 | 279.49x slower |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 4.22x faster |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 343.69x faster |
| concurrent_uncontended_reads | sequential | parallel | 1.11x faster |
| foreign_key_insert | postgres_18 | pg_fake | 3.34x faster |
| selective_inner_join | postgres_18 | pg_fake | 1.73x slower |
| many_match_inner_join | postgres_18 | pg_fake | 1.71x slower |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 2.46x slower |
| correlated_exists_100_rows | postgres_18 | pg_fake | 1.32x faster |
| global_aggregate_100_rows | postgres_18 | pg_fake | 1.84x slower |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 1.94x slower |
| select_distinct_100_rows | postgres_18 | pg_fake | 1.02x slower |
