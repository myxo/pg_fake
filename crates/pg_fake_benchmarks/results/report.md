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
| recorded_at | 2026-08-18T05:41:09Z |
| rust | rustc 1.96.0 (ac68faa20 2026-05-25) |

## Benchmarks

| Benchmark | Average |
| --- | ---: |
| create_table/pg_fake | 18.95 us |
| create_table/postgres_18 | 816.26 us |
| insert_row/pg_fake | 31.99 us |
| insert_row/postgres_18 | 84.94 us |
| insert_row_returning/pg_fake | 48.35 us |
| insert_row_returning/postgres_18 | 91.95 us |
| insert_row_with_defaults/pg_fake | 32.91 us |
| insert_row_with_defaults/postgres_18 | 87.38 us |
| update_row/pg_fake | 26.90 us |
| update_row/postgres_18 | 87.21 us |
| update_from_row/pg_fake | 29.34 us |
| update_from_row/postgres_18 | 94.74 us |
| delete_row/pg_fake | 18.47 us |
| delete_row/postgres_18 | 107.56 us |
| sequence_nextval/pg_fake | 17.90 us |
| sequence_nextval/postgres_18 | 26.22 us |
| transaction_insert/pg_fake | 41.78 us |
| transaction_insert/postgres_18 | 124.20 us |
| transaction_repeatable_read_select_for_update/pg_fake | 29.20 us |
| transaction_repeatable_read_select_for_update/postgres_18 | 78.25 us |
| select_100_rows/pg_fake | 41.07 us |
| select_100_rows/postgres_18 | 57.26 us |
| limit_offset_ordered_100_rows/pg_fake | 176.03 us |
| limit_offset_ordered_100_rows/postgres_18 | 38.71 us |
| order_by_100_rows/pg_fake | 181.67 us |
| order_by_100_rows/postgres_18 | 64.45 us |
| adapter_overhead_select_100_rows/core | 165.20 us |
| adapter_overhead_select_100_rows/sqlx | 178.24 us |
| core_parsed_vs_prepared_point_select/parse_and_analyze | 213.76 us |
| core_parsed_vs_prepared_point_select/prepared_reuse | 199.56 us |
| transaction_history_point_select/1 | 4.98 us |
| transaction_history_point_select/100 | 5.22 us |
| transaction_history_point_select/10,000 | 4.92 us |
| transaction_history_point_select/100,000 | 5.07 us |
| mvcc_old_snapshot_read/1 | 9.42 us |
| mvcc_old_snapshot_read/100 | 11.06 us |
| mvcc_old_snapshot_read/10,000 | 688.59 us |
| point_lookup_index_vs_scan/heap_scan/100 | 200.99 us |
| point_lookup_index_vs_scan/unique_index/100 | 203.66 us |
| point_lookup_index_vs_scan/heap_scan/10,000 | 20.27 ms |
| point_lookup_index_vs_scan/unique_index/10,000 | 23.11 ms |
| concurrent_uncontended_reads/sequential | 49.64 us |
| concurrent_uncontended_reads/parallel | 47.55 us |
| concurrent_same_row_contention/wait_then_rollback | 1.64 ms |
| foreign_key_insert/pg_fake | 59.40 us |
| foreign_key_insert/postgres_18 | 169.00 us |
| selective_inner_join/pg_fake | 126.11 us |
| selective_inner_join/postgres_18 | 38.52 us |
| many_match_inner_join/pg_fake | 383.18 us |
| many_match_inner_join/postgres_18 | 64.56 us |
| derived_and_scalar_subquery_100_rows/pg_fake | 11.75 ms |
| derived_and_scalar_subquery_100_rows/postgres_18 | 65.61 us |
| correlated_exists_100_rows/pg_fake | 23.40 ms |
| correlated_exists_100_rows/postgres_18 | 62.20 us |
| global_aggregate_100_rows/pg_fake | 341.94 us |
| global_aggregate_100_rows/postgres_18 | 37.30 us |
| grouped_aggregate_100_rows/pg_fake | 268.17 us |
| grouped_aggregate_100_rows/postgres_18 | 41.66 us |
| select_distinct_100_rows/pg_fake | 111.52 us |
| select_distinct_100_rows/postgres_18 | 35.75 us |

## Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| create_table | postgres_18 | pg_fake | 43.08x faster |
| insert_row | postgres_18 | pg_fake | 2.66x faster |
| insert_row_returning | postgres_18 | pg_fake | 1.90x faster |
| insert_row_with_defaults | postgres_18 | pg_fake | 2.65x faster |
| update_row | postgres_18 | pg_fake | 3.24x faster |
| update_from_row | postgres_18 | pg_fake | 3.23x faster |
| delete_row | postgres_18 | pg_fake | 5.82x faster |
| sequence_nextval | postgres_18 | pg_fake | 1.46x faster |
| transaction_insert | postgres_18 | pg_fake | 2.97x faster |
| transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 2.68x faster |
| select_100_rows | postgres_18 | pg_fake | 1.39x faster |
| limit_offset_ordered_100_rows | postgres_18 | pg_fake | 4.55x slower |
| order_by_100_rows | postgres_18 | pg_fake | 2.82x slower |
| adapter_overhead_select_100_rows | core | sqlx | 1.08x slower |
| core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 1.07x faster |
| transaction_history_point_select | 1 | 100 | 1.05x slower |
| transaction_history_point_select | 1 | 10,000 | 1.01x faster |
| transaction_history_point_select | 1 | 100,000 | 1.02x slower |
| mvcc_old_snapshot_read | 1 | 100 | 1.17x slower |
| mvcc_old_snapshot_read | 1 | 10,000 | 73.08x slower |
| point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 1.01x slower |
| point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 1.14x slower |
| concurrent_uncontended_reads | sequential | parallel | 1.04x faster |
| foreign_key_insert | postgres_18 | pg_fake | 2.85x faster |
| selective_inner_join | postgres_18 | pg_fake | 3.27x slower |
| many_match_inner_join | postgres_18 | pg_fake | 5.94x slower |
| derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 179.07x slower |
| correlated_exists_100_rows | postgres_18 | pg_fake | 376.13x slower |
| global_aggregate_100_rows | postgres_18 | pg_fake | 9.17x slower |
| grouped_aggregate_100_rows | postgres_18 | pg_fake | 6.44x slower |
| select_distinct_100_rows | postgres_18 | pg_fake | 3.12x slower |
