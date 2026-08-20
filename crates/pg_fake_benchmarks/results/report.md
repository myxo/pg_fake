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
| recorded_at | 2026-08-20T03:50:30Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average |
| --- | ---: |
| create_table/pg_fake | 18.39 us |
| create_table/postgres_18 | 862.58 us |
| insert_row/pg_fake | 27.04 us |
| insert_row/postgres_18 | 85.82 us |
| insert_row_returning/pg_fake | 41.59 us |
| insert_row_returning/postgres_18 | 91.71 us |
| insert_row_with_defaults/pg_fake | 29.02 us |
| insert_row_with_defaults/postgres_18 | 87.85 us |
| update_row/pg_fake | 22.90 us |
| update_row/postgres_18 | 87.76 us |
| update_from_row/pg_fake | 21.75 us |
| update_from_row/postgres_18 | 94.84 us |
| delete_row/pg_fake | 13.44 us |
| delete_row/postgres_18 | 128.51 us |
| sequence_nextval/pg_fake | 14.54 us |
| sequence_nextval/postgres_18 | 26.96 us |
| serial_identity_insert/pg_fake | 14.93 us |
| serial_identity_insert/postgres_18 | 32.13 us |
| uuid_temporal_select/pg_fake | 35.78 us |
| uuid_temporal_select/postgres_18 | 29.14 us |
| transaction_insert/pg_fake | 35.26 us |
| transaction_insert/postgres_18 | 125.40 us |
| transaction_repeatable_read_select_for_update/pg_fake | 28.10 us |
| transaction_repeatable_read_select_for_update/postgres_18 | 77.43 us |
| select_100_rows/pg_fake | 36.34 us |
| select_100_rows/postgres_18 | 56.91 us |
| limit_offset_ordered_100_rows/pg_fake | 48.24 us |
| limit_offset_ordered_100_rows/postgres_18 | 39.74 us |
| order_by_100_rows/pg_fake | 55.34 us |
| order_by_100_rows/postgres_18 | 68.81 us |
| adapter_overhead_select_100_rows/core | 37.89 us |
| adapter_overhead_select_100_rows/sqlx | 46.57 us |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 42.52 us |
| core_parsed_vs_prepared_point_select/prepared_reuse | 31.31 us |
| transaction_history_point_select/1 | 2.99 us |
| transaction_history_point_select/100 | 3.04 us |
| transaction_history_point_select/10,000 | 3.07 us |
| transaction_history_point_select/100,000 | 3.12 us |
| mvcc_old_snapshot_read/1 | 4.68 us |
| mvcc_old_snapshot_read/100 | 5.88 us |
| mvcc_old_snapshot_read/10,000 | 674.02 us |
| point_lookup_index_vs_scan/heap_scan/100 | 31.38 us |
| point_lookup_index_vs_scan/unique_index/100 | 31.38 us |
| point_lookup_index_vs_scan/heap_scan/10,000 | 2.68 ms |
| point_lookup_index_vs_scan/unique_index/10,000 | 2.68 ms |
| concurrent_uncontended_reads/sequential | 32.73 us |
| concurrent_uncontended_reads/parallel | 30.01 us |
| concurrent_same_row_contention/wait_then_rollback | 1.56 ms |
| foreign_key_insert/pg_fake | 51.43 us |
| foreign_key_insert/postgres_18 | 170.40 us |
| selective_inner_join/pg_fake | 67.12 us |
| selective_inner_join/postgres_18 | 38.54 us |
| many_match_inner_join/pg_fake | 111.18 us |
| many_match_inner_join/postgres_18 | 64.76 us |
| derived_and_scalar_subquery_100_rows/pg_fake | 185.42 us |
| derived_and_scalar_subquery_100_rows/postgres_18 | 72.02 us |
| correlated_exists_100_rows/pg_fake | 63.21 us |
| correlated_exists_100_rows/postgres_18 | 67.38 us |
| global_aggregate_100_rows/pg_fake | 70.06 us |
| global_aggregate_100_rows/postgres_18 | 38.56 us |
| grouped_aggregate_100_rows/pg_fake | 90.21 us |
| grouped_aggregate_100_rows/postgres_18 | 46.00 us |
| select_distinct_100_rows/pg_fake | 45.72 us |
| select_distinct_100_rows/postgres_18 | 41.97 us |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 46.91x faster |
| insert_row | postgres_18 | pg_fake | 3.17x faster |
| insert_row_returning | postgres_18 | pg_fake | 2.20x faster |
| insert_row_with_defaults | postgres_18 | pg_fake | 3.03x faster |
| update_row | postgres_18 | pg_fake | 3.83x faster |
| update_from_row | postgres_18 | pg_fake | 4.36x faster |
| delete_row | postgres_18 | pg_fake | 9.56x faster |
| sequence_nextval | postgres_18 | pg_fake | 1.85x faster |
| serial_identity_insert | postgres_18 | pg_fake | 2.15x faster |
| uuid_temporal_select | postgres_18 | pg_fake | 1.23x slower |
| transaction_insert | postgres_18 | pg_fake | 3.56x faster |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 2.76x faster |
| select_100_rows | postgres_18 | pg_fake | 1.57x faster |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 1.21x slower |
| order_by_100_rows | postgres_18 | pg_fake | 1.24x faster |
| adapter_overhead_select_100_rows | core | sqlx | 1.23x slower |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 1.36x faster |
| transaction_history_point_select | 1 | 100 | 1.02x slower |
| transaction_history_point_select | 1 | 10,000 | 1.03x slower |
| transaction_history_point_select | 1 | 100,000 | 1.04x slower |
| mvcc_old_snapshot_read | 1 | 100 | 1.25x slower |
| mvcc_old_snapshot_read | 1 | 10,000 | 143.93x slower |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 1.00x slower |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 1.00x faster |
| concurrent_uncontended_reads | sequential | parallel | 1.09x faster |
| foreign_key_insert | postgres_18 | pg_fake | 3.31x faster |
| selective_inner_join | postgres_18 | pg_fake | 1.74x slower |
| many_match_inner_join | postgres_18 | pg_fake | 1.72x slower |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 2.57x slower |
| correlated_exists_100_rows | postgres_18 | pg_fake | 1.07x faster |
| global_aggregate_100_rows | postgres_18 | pg_fake | 1.82x slower |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 1.96x slower |
| select_distinct_100_rows | postgres_18 | pg_fake | 1.09x slower |
