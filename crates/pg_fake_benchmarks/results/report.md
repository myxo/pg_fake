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
| recorded_at | 2026-08-13T03:12:04Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average |
| --- | ---: |
| create_table/pg_fake | 17.91 us |
| create_table/postgres_18 | 1.05 ms |
| insert_row/pg_fake | 29.04 us |
| insert_row/postgres_18 | 712.00 us |
| insert_row_with_defaults/pg_fake | 37.07 us |
| insert_row_with_defaults/postgres_18 | 729.97 us |
| update_row/pg_fake | 2.36 ms |
| update_row/postgres_18 | 247.45 us |
| delete_row/pg_fake | 2.37 ms |
| delete_row/postgres_18 | 494.46 us |
| transaction_insert/pg_fake | 38.69 us |
| transaction_insert/postgres_18 | 1.17 ms |
| transaction_repeatable_read_select_for_update/pg_fake | 28.57 us |
| transaction_repeatable_read_select_for_update/postgres_18 | 699.06 us |
| select_100_rows/pg_fake | 39.27 us |
| select_100_rows/postgres_18 | 317.46 us |
| limit_offset_ordered_100_rows/pg_fake | 174.02 us |
| limit_offset_ordered_100_rows/postgres_18 | 270.26 us |
| order_by_100_rows/pg_fake | 181.29 us |
| order_by_100_rows/postgres_18 | 356.34 us |
| adapter_overhead_select_100_rows/core | 160.30 us |
| adapter_overhead_select_100_rows/sqlx | 178.32 us |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 217.14 us |
| core_parsed_vs_prepared_point_select/prepared_reuse | 195.54 us |
| transaction_history_point_select/1 | 4.42 us |
| transaction_history_point_select/100 | 4.40 us |
| transaction_history_point_select/10,000 | 4.39 us |
| transaction_history_point_select/100,000 | 4.38 us |
| mvcc_old_snapshot_read/1 | 8.33 us |
| mvcc_old_snapshot_read/100 | 9.60 us |
| mvcc_old_snapshot_read/10,000 | 672.08 us |
| point_lookup_index_vs_scan/heap_scan/100 | 198.05 us |
| point_lookup_index_vs_scan/unique_index/100 | 198.11 us |
| point_lookup_index_vs_scan/heap_scan/10,000 | 19.39 ms |
| point_lookup_index_vs_scan/unique_index/10,000 | 19.30 ms |
| concurrent_uncontended_reads/sequential | 46.32 us |
| concurrent_uncontended_reads/parallel | 43.37 us |
| concurrent_same_row_contention/wait_then_rollback | 1.44 ms |
| foreign_key_insert/pg_fake | 54.17 us |
| foreign_key_insert/postgres_18 | 1.44 ms |
| selective_inner_join/pg_fake | 109.63 us |
| selective_inner_join/postgres_18 | 270.10 us |
| many_match_inner_join/pg_fake | 363.54 us |
| many_match_inner_join/postgres_18 | 347.25 us |
| derived_and_scalar_subquery_100_rows/pg_fake | 11.28 ms |
| derived_and_scalar_subquery_100_rows/postgres_18 | 372.93 us |
| correlated_exists_100_rows/pg_fake | 21.65 ms |
| correlated_exists_100_rows/postgres_18 | 355.24 us |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | pg_fake | postgres_18 | 58.41x slower |
| insert_row | pg_fake | postgres_18 | 24.52x slower |
| insert_row_with_defaults | pg_fake | postgres_18 | 19.69x slower |
| update_row | pg_fake | postgres_18 | 9.55x faster |
| delete_row | pg_fake | postgres_18 | 4.79x faster |
| transaction_insert | pg_fake | postgres_18 | 30.12x slower |
| transaction_repeatable_read_select_for_update | pg_fake | postgres_18 | 24.47x slower |
| select_100_rows | pg_fake | postgres_18 | 8.08x slower |
| limit_offset_ordered_100_rows | pg_fake | postgres_18 | 1.55x slower |
| order_by_100_rows | pg_fake | postgres_18 | 1.97x slower |
| adapter_overhead_select_100_rows | core | sqlx | 1.11x slower |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 1.11x faster |
| transaction_history_point_select | 1 | 100 | 1.00x faster |
| transaction_history_point_select | 1 | 10,000 | 1.01x faster |
| transaction_history_point_select | 1 | 100,000 | 1.01x faster |
| mvcc_old_snapshot_read | 1 | 100 | 1.15x slower |
| mvcc_old_snapshot_read | 1 | 10,000 | 80.72x slower |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 1.00x slower |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 1.01x faster |
| concurrent_uncontended_reads | sequential | parallel | 1.07x faster |
| foreign_key_insert | pg_fake | postgres_18 | 26.63x slower |
| selective_inner_join | pg_fake | postgres_18 | 2.46x slower |
| many_match_inner_join | pg_fake | postgres_18 | 1.05x faster |
| derived_and_scalar_subquery_100_rows | pg_fake | postgres_18 | 30.26x faster |
| correlated_exists_100_rows | pg_fake | postgres_18 | 60.94x faster |
