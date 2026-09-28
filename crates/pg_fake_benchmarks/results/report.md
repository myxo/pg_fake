# Benchmark results

## Environment

| Property | Value |
| --- | --- |
| architecture | aarch64 |
| cpu | Apple M2 |
| criterion | 0.5 |
| logical_cpus | 8 |
| os | macos |
| os_version | Darwin 24.6.0 |
| performance_levels | level 0: 4 physical / 4 logical; level 1: 4 physical / 4 logical |
| physical_cores | 8 |
| postgres_target | 18 |
| recorded_at | 2026-09-28T05:31:34Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 56.20 us | -9.03% |
| tier1_insert_row/postgres_18 | 92.77 us | -0.38% |
| tier1_update_row/pg_fake | 47.70 us | -11.57% |
| tier1_update_row/postgres_18 | 93.87 us | +2.36% |
| tier1_transaction_insert/pg_fake | 51.63 us | -20.59% |
| tier1_transaction_insert/postgres_18 | 135.83 us | -0.30% |
| tier1_select_100_rows/pg_fake | 31.61 us | -6.87% |
| tier1_select_100_rows/postgres_18 | 58.42 us | -2.17% |
| tier1_select_where_100_rows/pg_fake | 17.99 us | -8.99% |
| tier1_select_where_100_rows/postgres_18 | 33.55 us | -3.19% |
| tier1_select_where_indexed_100_rows/pg_fake | 12.40 us | -4.63% |
| tier1_select_where_indexed_100_rows/postgres_18 | 33.89 us | +6.97% |
| tier1_limit_offset_ordered_100_rows/pg_fake | 75.99 us | -9.07% |
| tier1_limit_offset_ordered_100_rows/postgres_18 | 42.20 us | -1.12% |
| tier1_order_by_100_rows/pg_fake | 78.78 us | -36.87% |
| tier1_order_by_100_rows/postgres_18 | 65.90 us | -1.72% |
| tier1_selective_inner_join/pg_fake | 107.81 us | +1.11% |
| tier1_selective_inner_join/postgres_18 | 39.09 us | -4.57% |
| tier1_many_match_inner_join/pg_fake | 176.19 us | +6.14% |
| tier1_many_match_inner_join/postgres_18 | 66.53 us | -1.39% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.65x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 1.97x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.63x |
| tier1_select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.85x |
| tier1_select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.87x |
| tier1_select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.73x |
| tier1_limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.80x |
| tier1_order_by_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.20x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.76x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.65x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_core_snapshot_100_rows/pg_fake | 15.12 us | +1.23% |
| tier2_session_settings_roundtrip/pg_fake | 68.87 us | -3.12% |
| tier2_session_settings_roundtrip/postgres_18 | 138.93 us | -1.14% |
| tier2_nested_savepoint_release/pg_fake | 95.92 us | -19.69% |
| tier2_nested_savepoint_release/postgres_18 | 186.70 us | -1.29% |
| tier2_nested_savepoint_rollback/pg_fake | 100.67 us | -17.04% |
| tier2_nested_savepoint_rollback/postgres_18 | 187.97 us | -2.15% |
| tier2_runtime_temporal_100_rows/pg_fake | 957.49 us | -22.36% |
| tier2_runtime_temporal_100_rows/postgres_18 | 116.64 us | -1.00% |
| tier2_runtime_patterns_100_rows/pg_fake | 274.74 us | -93.50% |
| tier2_runtime_patterns_100_rows/postgres_18 | 116.48 us | -1.27% |
| tier2_create_table/pg_fake | 63.28 us | -69.44% |
| tier2_create_table/postgres_18 | 888.91 us | -6.88% |
| tier2_transactional_ddl_create_rollback/pg_fake | 46.71 us | -17.47% |
| tier2_transactional_ddl_create_rollback/postgres_18 | 1.30 ms | -2.60% |
| tier2_sqlx_migration_chain/pg_fake | 1.41 ms | -98.04% |
| tier2_sqlx_migration_chain/postgres_18 | 6.27 ms | -4.39% |
| tier2_alter_table_rewrite_100_rows/pg_fake | 336.42 us | -1.37% |
| tier2_alter_table_rewrite_100_rows/postgres_18 | 353.46 us | -6.90% |
| tier2_insert_row_returning/pg_fake | 65.74 us | -11.82% |
| tier2_insert_row_returning/postgres_18 | 98.03 us | -0.16% |
| tier2_insert_row_with_defaults/pg_fake | 61.09 us | -3.96% |
| tier2_insert_row_with_defaults/postgres_18 | 95.23 us | +0.10% |
| tier2_insert_on_conflict_do_nothing/pg_fake | 35.19 us | -17.07% |
| tier2_insert_on_conflict_do_nothing/postgres_18 | 30.90 us | +1.51% |
| tier2_insert_on_conflict_conflict_free/pg_fake | 79.45 us | -16.08% |
| tier2_insert_on_conflict_conflict_free/postgres_18 | 67.97 us | +0.35% |
| tier2_insert_on_conflict_do_update/pg_fake | 46.12 us | -15.65% |
| tier2_insert_on_conflict_do_update/postgres_18 | 35.65 us | +0.80% |
| tier2_update_from_row/pg_fake | 52.44 us | -10.72% |
| tier2_update_from_row/postgres_18 | 101.63 us | +0.07% |
| tier2_delete_row/pg_fake | 33.38 us | -15.40% |
| tier2_delete_row/postgres_18 | 119.20 us | -0.04% |
| tier2_sequence_nextval/pg_fake | 38.11 us | -12.86% |
| tier2_sequence_nextval/postgres_18 | 28.50 us | -1.25% |
| tier2_serial_identity_insert/pg_fake | 38.08 us | -11.09% |
| tier2_serial_identity_insert/postgres_18 | 32.37 us | -1.90% |
| tier2_uuid_temporal_select/pg_fake | 44.52 us | -8.57% |
| tier2_uuid_temporal_select/postgres_18 | 34.74 us | +8.06% |
| tier2_offset_datetime_bind_store_fetch/pg_fake | 56.38 us | -7.86% |
| tier2_offset_datetime_bind_store_fetch/postgres_18 | 36.32 us | -0.20% |
| tier2_bigint_uuid_array_bind_store_fetch/pg_fake | 62.37 us | -8.58% |
| tier2_bigint_uuid_array_bind_store_fetch/postgres_18 | 37.19 us | -0.49% |
| tier2_uuid_any_100_rows/pg_fake | 67.66 us | -97.79% |
| tier2_uuid_any_100_rows/postgres_18 | 58.18 us | +8.40% |
| tier2_array_containment_100_rows/pg_fake | 133.76 us | +9.77% |
| tier2_array_containment_100_rows/postgres_18 | 40.95 us | -2.01% |
| tier2_json_insert_returning/pg_fake | 68.40 us | -12.46% |
| tier2_json_insert_returning/postgres_18 | 145.01 us | -0.33% |
| tier2_jsonb_insert_returning/pg_fake | 73.48 us | -13.20% |
| tier2_jsonb_insert_returning/postgres_18 | 141.98 us | -3.36% |
| tier2_jsonb_extraction/pg_fake | 126.62 us | +2.04% |
| tier2_jsonb_extraction/postgres_18 | 66.24 us | -1.63% |
| tier2_jsonb_containment/pg_fake | 272.03 us | -1.66% |
| tier2_jsonb_containment/postgres_18 | 43.02 us | -1.29% |
| tier2_window_row_number_100_rows/pg_fake | 185.52 us | +0.20% |
| tier2_window_row_number_100_rows/postgres_18 | 83.40 us | -1.70% |
| tier2_ordered_string_agg_100_rows/pg_fake | 270.19 us | -2.25% |
| tier2_ordered_string_agg_100_rows/postgres_18 | 70.90 us | -2.36% |
| tier2_transaction_repeatable_read_select_for_update/pg_fake | 67.75 us | -99.90% |
| tier2_transaction_repeatable_read_select_for_update/postgres_18 | 84.87 us | -0.76% |
| tier2_adapter_overhead_select_100_rows/core | 68.23 us | -36.26% |
| tier2_adapter_overhead_select_100_rows/sqlx | 69.26 us | -38.90% |
| tier2_core_parsed_vs_prepared_point_select/parse_and_analyze | 18.88 us | +1.72% |
| tier2_core_parsed_vs_prepared_point_select/prepared_reuse | 563.04 ns | +27.70% |
| tier2_point_lookup_index_vs_scan/heap_scan/100 | 3.52 us | +29.03% |
| tier2_point_lookup_index_vs_scan/unique_index/100 | 610.34 ns | +26.67% |
| tier2_point_lookup_index_vs_scan/heap_scan/10,000 | 310.86 us | +18.73% |
| tier2_point_lookup_index_vs_scan/unique_index/10,000 | 667.73 ns | +25.70% |
| tier2_concurrent_uncontended_reads/sequential | 18.98 us | +2.10% |
| tier2_concurrent_uncontended_reads/parallel | 16.97 us | -1.55% |
| tier2_foreign_key_insert/pg_fake | 143.33 us | +28.05% |
| tier2_foreign_key_insert/postgres_18 | 181.67 us | -0.11% |
| tier2_derived_and_scalar_subquery_100_rows/pg_fake | 452.48 us | +19.50% |
| tier2_derived_and_scalar_subquery_100_rows/postgres_18 | 85.38 us | -2.01% |
| tier2_materialized_cte_100_rows/pg_fake | 129.30 us | -98.56% |
| tier2_materialized_cte_100_rows/postgres_18 | 78.85 us | -0.75% |
| tier2_correlated_exists_100_rows/pg_fake | 88.72 us | -1.94% |
| tier2_correlated_exists_100_rows/postgres_18 | 83.42 us | +12.33% |
| tier2_global_aggregate_100_rows/pg_fake | 41.79 us | -2.12% |
| tier2_global_aggregate_100_rows/postgres_18 | 39.63 us | -4.47% |
| tier2_grouped_aggregate_100_rows/pg_fake | 202.44 us | -0.01% |
| tier2_grouped_aggregate_100_rows/postgres_18 | 49.32 us | -1.34% |
| tier2_select_distinct_100_rows/pg_fake | 63.11 us | -4.95% |
| tier2_select_distinct_100_rows/postgres_18 | 44.89 us | -1.57% |
| tier2_union_all_100_rows/pg_fake | 466.62 us | +7.45% |
| tier2_union_all_100_rows/postgres_18 | 87.11 us | +0.12% |
| tier2_union_100_rows/pg_fake | 497.92 us | +4.74% |
| tier2_union_100_rows/postgres_18 | 119.55 us | +37.19% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 2.02x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 1.95x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 1.87x |
| tier2_runtime_temporal_100_rows | postgres_18 | pg_fake | 🔴 ↓ 8.21x |
| tier2_runtime_patterns_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.36x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 14.05x |
| tier2_transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 27.88x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 4.45x |
| tier2_alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.05x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 1.49x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 1.56x |
| tier2_insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🔴 ↓ 1.14x |
| tier2_insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🔴 ↓ 1.17x |
| tier2_insert_on_conflict_do_update | postgres_18 | pg_fake | 🔴 ↓ 1.29x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 1.94x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 3.57x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🔴 ↓ 1.34x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🔴 ↓ 1.18x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🔴 ↓ 1.28x |
| tier2_offset_datetime_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.55x |
| tier2_bigint_uuid_array_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.68x |
| tier2_uuid_any_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.16x |
| tier2_array_containment_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.27x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.12x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 1.93x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.91x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 6.32x |
| tier2_window_row_number_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.22x |
| tier2_ordered_string_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.81x |
| tier2_transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.25x |
| tier2_adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.02x |
| tier2_core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 33.52x |
| tier2_point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.77x |
| tier2_point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 465.55x |
| tier2_concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.12x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 1.27x |
| tier2_derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.30x |
| tier2_materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.64x |
| tier2_correlated_exists_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.06x |
| tier2_global_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.05x |
| tier2_grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.10x |
| tier2_select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.41x |
| tier2_union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.36x |
| tier2_union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.17x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_transaction_local_guc_roundtrip/pg_fake | 95.83 us | -12.30% |
| tier3_transaction_local_guc_roundtrip/postgres_18 | 136.52 us | -0.68% |
| tier3_skip_locked_queue_100_rows/pg_fake | 197.93 us | -99.73% |
| tier3_skip_locked_queue_100_rows/postgres_18 | 48.73 us | -0.44% |
| tier3_serializable_uncontended_read/pg_fake | 27.60 us | N/A |
| tier3_serializable_uncontended_read/postgres_18 | 86.50 us | N/A |
| tier3_serializable_write_skew/pg_fake | 181.68 us | N/A |
| tier3_serializable_write_skew/postgres_18 | 302.62 us | N/A |
| tier3_lateral_latest_per_parent_100_rows/pg_fake | 7.42 ms | +4.92% |
| tier3_lateral_latest_per_parent_100_rows/postgres_18 | 632.56 us | +270.55% |
| tier3_migration_table_lock_two_relations/pg_fake | 39.22 us | -7.53% |
| tier3_migration_table_lock_two_relations/postgres_18 | 82.00 us | -2.70% |
| tier3_procedural_trigger_insert_update/pg_fake | 99.84 us | -99.44% |
| tier3_procedural_trigger_insert_update/postgres_18 | 122.21 us | -0.19% |
| tier3_partial_unique_index_100_rows/pg_fake | 99.70 us | -7.76% |
| tier3_partial_unique_index_100_rows/postgres_18 | 551.66 us | +0.94% |
| tier3_temporary_table_on_commit_drop/pg_fake | 29.47 us | -21.93% |
| tier3_temporary_table_on_commit_drop/postgres_18 | 258.24 us | -23.20% |
| tier3_catalog_regclass_lookup/pg_fake | 73.11 us | -6.97% |
| tier3_catalog_regclass_lookup/postgres_18 | 32.90 us | -1.93% |
| tier3_ordered_filtered_array_agg_100_rows/pg_fake | 213.66 us | -0.63% |
| tier3_ordered_filtered_array_agg_100_rows/postgres_18 | 43.53 us | -1.12% |
| tier3_correlated_unnest_100_rows/pg_fake | 214.00 us | -53.71% |
| tier3_correlated_unnest_100_rows/postgres_18 | 145.19 us | -5.35% |
| tier3_hashed_advisory_lock_acquisition/pg_fake | 54.19 us | -99.85% |
| tier3_hashed_advisory_lock_acquisition/postgres_18 | 76.36 us | -4.14% |
| tier3_jsonb_join_group/pg_fake | 763.92 us | +2.04% |
| tier3_jsonb_join_group/postgres_18 | 330.75 us | -6.79% |
| tier3_window_rank_100_rows/pg_fake | 516.36 us | +2.11% |
| tier3_window_rank_100_rows/postgres_18 | 108.33 us | -0.96% |
| tier3_window_offset_100_rows/pg_fake | 1.06 ms | +2.42% |
| tier3_window_offset_100_rows/postgres_18 | 124.71 us | -0.91% |
| tier3_window_moving_aggregate_100_rows/pg_fake | 613.21 us | +1.24% |
| tier3_window_moving_aggregate_100_rows/postgres_18 | 149.68 us | -1.00% |
| tier3_nested_filtered_view_100_rows/pg_fake | 420.03 us | -14.64% |
| tier3_nested_filtered_view_100_rows/postgres_18 | 34.72 us | -2.34% |
| tier3_transaction_history_point_select/1 | 348.51 ns | +29.18% |
| tier3_transaction_history_point_select/100 | 350.43 ns | +30.54% |
| tier3_transaction_history_point_select/10,000 | 347.77 ns | +25.52% |
| tier3_transaction_history_point_select/100,000 | 348.92 ns | +28.86% |
| tier3_mvcc_old_snapshot_read/1 | 628.59 ns | +26.14% |
| tier3_mvcc_old_snapshot_read/100 | 1.90 us | +0.84% |
| tier3_mvcc_old_snapshot_read/10,000 | 665.63 us | +0.10% |
| tier3_concurrent_same_row_contention/wait_then_rollback | 1.99 ms | -5.45% |
| tier3_data_modifying_cte_update_100_rows/pg_fake | 460.26 us | -79.09% |
| tier3_data_modifying_cte_update_100_rows/postgres_18 | 103.75 us | -2.06% |
| tier3_recursive_cte_numeric_series_100_rows/pg_fake | 1.04 ms | -58.85% |
| tier3_recursive_cte_numeric_series_100_rows/postgres_18 | 74.10 us | -1.41% |
| tier3_recursive_cte_branching_traversal_127_rows/pg_fake | 330.38 us | -95.96% |
| tier3_recursive_cte_branching_traversal_127_rows/postgres_18 | 121.62 us | -0.89% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_transaction_local_guc_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 1.42x |
| tier3_skip_locked_queue_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.06x |
| tier3_serializable_uncontended_read | postgres_18 | pg_fake | 🟢 ↑ 3.13x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 1.67x |
| tier3_lateral_latest_per_parent_100_rows | postgres_18 | pg_fake | 🔴 ↓ 11.74x |
| tier3_migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 2.09x |
| tier3_procedural_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.22x |
| tier3_partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 5.53x |
| tier3_temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 8.76x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 2.22x |
| tier3_ordered_filtered_array_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.91x |
| tier3_correlated_unnest_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.47x |
| tier3_hashed_advisory_lock_acquisition | postgres_18 | pg_fake | 🟢 ↑ 1.41x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 2.31x |
| tier3_window_rank_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.77x |
| tier3_window_offset_100_rows | postgres_18 | pg_fake | 🔴 ↓ 8.53x |
| tier3_window_moving_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.10x |
| tier3_nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 12.10x |
| tier3_transaction_history_point_select | 1 | 100 | 🔴 ↓ 1.01x |
| tier3_transaction_history_point_select | 1 | 10,000 | 🟢 ↑ 1.00x |
| tier3_transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.00x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.03x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1058.93x |
| tier3_data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.44x |
| tier3_recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 14.00x |
| tier3_recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 2.72x |
