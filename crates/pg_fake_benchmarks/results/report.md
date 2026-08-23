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
| recorded_at | 2026-08-22T20:59:47Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average |
| --- | ---: |
| create_table/pg_fake | 18.33 us |
| create_table/postgres_18 | 837.06 us |
| insert_row/pg_fake | 27.88 us |
| insert_row/postgres_18 | 86.32 us |
| insert_row_returning/pg_fake | 56.07 us |
| insert_row_returning/postgres_18 | 91.04 us |
| insert_row_with_defaults/pg_fake | 28.14 us |
| insert_row_with_defaults/postgres_18 | 87.82 us |
| update_row/pg_fake | 21.37 us |
| update_row/postgres_18 | 87.29 us |
| update_from_row/pg_fake | 20.43 us |
| update_from_row/postgres_18 | 150.86 us |
| delete_row/pg_fake | 14.03 us |
| delete_row/postgres_18 | 127.88 us |
| sequence_nextval/pg_fake | 14.76 us |
| sequence_nextval/postgres_18 | 26.59 us |
| serial_identity_insert/pg_fake | 14.91 us |
| serial_identity_insert/postgres_18 | 30.67 us |
| uuid_temporal_select/pg_fake | 18.85 us |
| uuid_temporal_select/postgres_18 | 29.10 us |
| transaction_insert/pg_fake | 34.61 us |
| transaction_insert/postgres_18 | 126.02 us |
| transaction_repeatable_read_select_for_update/pg_fake | 28.07 us |
| transaction_repeatable_read_select_for_update/postgres_18 | 78.18 us |
| select_100_rows/pg_fake | 37.15 us |
| select_100_rows/postgres_18 | 57.68 us |
| select_where_100_rows/pg_fake | 37.47 us |
| select_where_100_rows/postgres_18 | 33.09 us |
| limit_offset_ordered_100_rows/pg_fake | 47.53 us |
| limit_offset_ordered_100_rows/postgres_18 | 38.98 us |
| order_by_100_rows/pg_fake | 53.63 us |
| order_by_100_rows/postgres_18 | 64.06 us |
| foreign_key_insert/pg_fake | 51.02 us |
| foreign_key_insert/postgres_18 | 168.60 us |
| selective_inner_join/pg_fake | 69.99 us |
| selective_inner_join/postgres_18 | 38.82 us |
| many_match_inner_join/pg_fake | 115.37 us |
| many_match_inner_join/postgres_18 | 64.76 us |
| derived_and_scalar_subquery_100_rows/pg_fake | 207.67 us |
| derived_and_scalar_subquery_100_rows/postgres_18 | 84.78 us |
| correlated_exists_100_rows/pg_fake | 63.93 us |
| correlated_exists_100_rows/postgres_18 | 81.88 us |
| global_aggregate_100_rows/pg_fake | 69.53 us |
| global_aggregate_100_rows/postgres_18 | 38.56 us |
| grouped_aggregate_100_rows/pg_fake | 90.11 us |
| grouped_aggregate_100_rows/postgres_18 | 46.19 us |
| select_distinct_100_rows/pg_fake | 42.65 us |
| select_distinct_100_rows/postgres_18 | 42.01 us |
| union_all_100_rows/pg_fake | 190.25 us |
| union_all_100_rows/postgres_18 | 85.73 us |
| union_100_rows/pg_fake | 223.60 us |
| union_100_rows/postgres_18 | 85.22 us |
| adapter_overhead_select_100_rows/core | 37.62 us |
| adapter_overhead_select_100_rows/sqlx | 46.34 us |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 16.03 us |
| core_parsed_vs_prepared_point_select/prepared_reuse | 5.61 us |
| transaction_history_point_select/1 | 2.99 us |
| transaction_history_point_select/100 | 3.06 us |
| transaction_history_point_select/10,000 | 3.04 us |
| transaction_history_point_select/100,000 | 3.07 us |
| mvcc_old_snapshot_read/1 | 4.90 us |
| mvcc_old_snapshot_read/100 | 7.46 us |
| mvcc_old_snapshot_read/10,000 | 1.33 ms |
| point_lookup_index_vs_scan/heap_scan/100 | 24.04 us |
| point_lookup_index_vs_scan/unique_index/100 | 5.70 us |
| point_lookup_index_vs_scan/heap_scan/10,000 | 1.98 ms |
| point_lookup_index_vs_scan/unique_index/10,000 | 5.74 us |
| concurrent_uncontended_reads/sequential | 34.19 us |
| concurrent_uncontended_reads/parallel | 30.59 us |
| concurrent_same_row_contention/wait_then_rollback | 1.59 ms |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 45.67x faster |
| insert_row | postgres_18 | pg_fake | 3.10x faster |
| insert_row_returning | postgres_18 | pg_fake | 1.62x faster |
| insert_row_with_defaults | postgres_18 | pg_fake | 3.12x faster |
| update_row | postgres_18 | pg_fake | 4.08x faster |
| update_from_row | postgres_18 | pg_fake | 7.38x faster |
| delete_row | postgres_18 | pg_fake | 9.12x faster |
| sequence_nextval | postgres_18 | pg_fake | 1.80x faster |
| serial_identity_insert | postgres_18 | pg_fake | 2.06x faster |
| uuid_temporal_select | postgres_18 | pg_fake | 1.54x faster |
| transaction_insert | postgres_18 | pg_fake | 3.64x faster |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 2.79x faster |
| select_100_rows | postgres_18 | pg_fake | 1.55x faster |
| select_where_100_rows | postgres_18 | pg_fake | 1.13x slower |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 1.22x slower |
| order_by_100_rows | postgres_18 | pg_fake | 1.19x faster |
| foreign_key_insert | postgres_18 | pg_fake | 3.30x faster |
| selective_inner_join | postgres_18 | pg_fake | 1.80x slower |
| many_match_inner_join | postgres_18 | pg_fake | 1.78x slower |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 2.45x slower |
| correlated_exists_100_rows | postgres_18 | pg_fake | 1.28x faster |
| global_aggregate_100_rows | postgres_18 | pg_fake | 1.80x slower |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 1.95x slower |
| select_distinct_100_rows | postgres_18 | pg_fake | 1.02x slower |
| union_all_100_rows | postgres_18 | pg_fake | 2.22x slower |
| union_100_rows | postgres_18 | pg_fake | 2.62x slower |
| adapter_overhead_select_100_rows | core | sqlx | 1.23x slower |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 2.86x faster |
| transaction_history_point_select | 1 | 100 | 1.02x slower |
| transaction_history_point_select | 1 | 10,000 | 1.02x slower |
| transaction_history_point_select | 1 | 100,000 | 1.03x slower |
| mvcc_old_snapshot_read | 1 | 100 | 1.52x slower |
| mvcc_old_snapshot_read | 1 | 10,000 | 271.52x slower |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 4.22x faster |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 344.62x faster |
| concurrent_uncontended_reads | sequential | parallel | 1.12x faster |
