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
| recorded_at | 2026-10-04T22:36:49Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 37.95 us | -10.83% |
| tier1_insert_row/postgres_18 | 72.68 us | -22.79% |
| tier1_update_row/pg_fake | 43.48 us | +39.16% |
| tier1_update_row/postgres_18 | 90.12 us | -4.43% |
| tier1_transaction_insert/pg_fake | 41.86 us | -3.72% |
| tier1_transaction_insert/postgres_18 | 117.02 us | -14.87% |
| tier1_select_100_rows/pg_fake | 24.39 us | -16.49% |
| tier1_select_100_rows/postgres_18 | 59.03 us | +0.84% |
| tier1_select_where_100_rows/pg_fake | 14.52 us | -7.97% |
| tier1_select_where_100_rows/postgres_18 | 34.11 us | -1.24% |
| tier1_select_where_indexed_100_rows/pg_fake | 8.32 us | +8.04% |
| tier1_select_where_indexed_100_rows/postgres_18 | 34.26 us | -0.86% |
| tier1_limit_offset_ordered_100_rows/pg_fake | 22.97 us | -35.17% |
| tier1_limit_offset_ordered_100_rows/postgres_18 | 42.29 us | -0.18% |
| tier1_order_by_100_rows/pg_fake | 35.27 us | -12.55% |
| tier1_order_by_100_rows/postgres_18 | 66.02 us | -0.55% |
| tier1_selective_inner_join/pg_fake | 16.85 us | -81.88% |
| tier1_selective_inner_join/postgres_18 | 39.29 us | -1.25% |
| tier1_many_match_inner_join/pg_fake | 24.52 us | -84.88% |
| tier1_many_match_inner_join/postgres_18 | 66.57 us | +1.28% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.92x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 2.07x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.80x |
| tier1_select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.42x |
| tier1_select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.35x |
| tier1_select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 4.12x |
| tier1_limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.84x |
| tier1_order_by_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.87x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.33x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.71x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_insert_bound_row/pg_fake | 28.18 us | N/A |
| tier2_insert_bound_row/postgres_18 | 37.17 us | N/A |
| tier2_update_bound_row/pg_fake | 33.79 us | N/A |
| tier2_update_bound_row/postgres_18 | 35.32 us | N/A |
| tier2_core_snapshot_100_rows/pg_fake | 14.69 us | -2.00% |
| tier2_session_settings_roundtrip/pg_fake | 33.70 us | -2.24% |
| tier2_session_settings_roundtrip/postgres_18 | 139.26 us | -0.28% |
| tier2_nested_savepoint_release/pg_fake | 62.27 us | -2.41% |
| tier2_nested_savepoint_release/postgres_18 | 188.34 us | +0.27% |
| tier2_nested_savepoint_rollback/pg_fake | 63.41 us | -1.35% |
| tier2_nested_savepoint_rollback/postgres_18 | 190.15 us | -0.27% |
| tier2_runtime_temporal_100_rows/pg_fake | 299.84 us | -2.03% |
| tier2_runtime_temporal_100_rows/postgres_18 | 118.07 us | +0.60% |
| tier2_runtime_patterns_100_rows/pg_fake | 230.38 us | -3.53% |
| tier2_runtime_patterns_100_rows/postgres_18 | 118.45 us | +0.59% |
| tier2_create_table/pg_fake | 36.47 us | -7.14% |
| tier2_create_table/postgres_18 | 1.00 ms | +6.00% |
| tier2_transactional_ddl_create_rollback/pg_fake | 29.44 us | -0.81% |
| tier2_transactional_ddl_create_rollback/postgres_18 | 1.32 ms | -6.80% |
| tier2_sqlx_migration_chain/pg_fake | 1.04 ms | +10.41% |
| tier2_sqlx_migration_chain/postgres_18 | 6.56 ms | +0.04% |
| tier2_alter_table_rewrite_100_rows/pg_fake | 323.03 us | +3.50% |
| tier2_alter_table_rewrite_100_rows/postgres_18 | 318.26 us | -3.60% |
| tier2_insert_row_returning/pg_fake | 45.46 us | -6.54% |
| tier2_insert_row_returning/postgres_18 | 98.76 us | +0.06% |
| tier2_insert_row_with_defaults/pg_fake | 39.85 us | -7.16% |
| tier2_insert_row_with_defaults/postgres_18 | 95.88 us | -0.81% |
| tier2_insert_on_conflict_do_nothing/pg_fake | 19.69 us | -3.72% |
| tier2_insert_on_conflict_do_nothing/postgres_18 | 30.30 us | -0.99% |
| tier2_insert_on_conflict_conflict_free/pg_fake | 46.90 us | -5.86% |
| tier2_insert_on_conflict_conflict_free/postgres_18 | 68.27 us | -0.14% |
| tier2_insert_on_conflict_do_update/pg_fake | 31.45 us | -4.55% |
| tier2_insert_on_conflict_do_update/postgres_18 | 35.50 us | -0.30% |
| tier2_update_from_row/pg_fake | 53.49 us | +43.91% |
| tier2_update_from_row/postgres_18 | 102.36 us | +0.03% |
| tier2_delete_row/pg_fake | 27.96 us | +66.92% |
| tier2_delete_row/postgres_18 | 118.69 us | -0.62% |
| tier2_sequence_nextval/pg_fake | 20.31 us | -0.85% |
| tier2_sequence_nextval/postgres_18 | 28.34 us | -0.12% |
| tier2_serial_identity_insert/pg_fake | 19.73 us | -1.82% |
| tier2_serial_identity_insert/postgres_18 | 32.88 us | -1.74% |
| tier2_uuid_temporal_select/pg_fake | 25.05 us | -0.88% |
| tier2_uuid_temporal_select/postgres_18 | 30.62 us | -1.47% |
| tier2_offset_datetime_bind_store_fetch/pg_fake | 41.58 us | -1.19% |
| tier2_offset_datetime_bind_store_fetch/postgres_18 | 36.26 us | +0.08% |
| tier2_bigint_uuid_array_bind_store_fetch/pg_fake | 47.61 us | -0.49% |
| tier2_bigint_uuid_array_bind_store_fetch/postgres_18 | 37.15 us | +3.03% |
| tier2_uuid_any_100_rows/pg_fake | 56.75 us | -3.17% |
| tier2_uuid_any_100_rows/postgres_18 | 52.79 us | -19.97% |
| tier2_array_containment_100_rows/pg_fake | 110.83 us | -2.98% |
| tier2_array_containment_100_rows/postgres_18 | 41.59 us | -0.27% |
| tier2_json_insert_returning/pg_fake | 34.29 us | -2.06% |
| tier2_json_insert_returning/postgres_18 | 146.22 us | +26.17% |
| tier2_jsonb_insert_returning/pg_fake | 39.70 us | -2.37% |
| tier2_jsonb_insert_returning/postgres_18 | 143.35 us | -1.42% |
| tier2_jsonb_extraction/pg_fake | 111.06 us | -4.34% |
| tier2_jsonb_extraction/postgres_18 | 66.78 us | +0.29% |
| tier2_jsonb_containment/pg_fake | 268.00 us | +0.88% |
| tier2_jsonb_containment/postgres_18 | 43.73 us | +0.08% |
| tier2_window_row_number_100_rows/pg_fake | 124.20 us | -5.12% |
| tier2_window_row_number_100_rows/postgres_18 | 84.14 us | +0.03% |
| tier2_ordered_string_agg_100_rows/pg_fake | 245.92 us | +1.18% |
| tier2_ordered_string_agg_100_rows/postgres_18 | 70.54 us | -0.48% |
| tier2_transaction_repeatable_read_select_for_update/pg_fake | 51.66 us | -1.49% |
| tier2_transaction_repeatable_read_select_for_update/postgres_18 | 85.43 us | -1.51% |
| tier2_adapter_overhead_select_100_rows/core | 13.91 us | -57.53% |
| tier2_adapter_overhead_select_100_rows/sqlx | 25.23 us | -18.11% |
| tier2_core_parsed_vs_prepared_point_select/parse_and_analyze | 19.62 us | +1.73% |
| tier2_core_parsed_vs_prepared_point_select/prepared_reuse | 535.91 ns | -9.92% |
| tier2_point_lookup_index_vs_scan/heap_scan/100 | 3.33 us | -16.07% |
| tier2_point_lookup_index_vs_scan/unique_index/100 | 577.68 ns | -8.02% |
| tier2_point_lookup_index_vs_scan/heap_scan/10,000 | 296.83 us | -17.52% |
| tier2_point_lookup_index_vs_scan/unique_index/10,000 | 637.97 ns | -8.99% |
| tier2_concurrent_uncontended_reads/sequential | 12.28 us | -8.00% |
| tier2_concurrent_uncontended_reads/parallel | 11.07 us | -0.27% |
| tier2_foreign_key_insert/pg_fake | 66.07 us | -7.21% |
| tier2_foreign_key_insert/postgres_18 | 183.26 us | +2.22% |
| tier2_derived_and_scalar_subquery_100_rows/pg_fake | 389.79 us | -13.61% |
| tier2_derived_and_scalar_subquery_100_rows/postgres_18 | 86.65 us | -57.90% |
| tier2_materialized_cte_100_rows/pg_fake | 112.93 us | -81.01% |
| tier2_materialized_cte_100_rows/postgres_18 | 78.33 us | -98.13% |
| tier2_derived_source_join_100_rows/pg_fake | 142.61 us | -80.34% |
| tier2_derived_source_join_100_rows/postgres_18 | 74.40 us | -97.84% |
| tier2_correlated_exists_100_rows/pg_fake | 71.05 us | -81.98% |
| tier2_correlated_exists_100_rows/postgres_18 | 83.52 us | -81.75% |
| tier2_global_aggregate_100_rows/pg_fake | 35.31 us | -2.81% |
| tier2_global_aggregate_100_rows/postgres_18 | 40.17 us | -8.81% |
| tier2_grouped_aggregate_100_rows/pg_fake | 179.43 us | -1.80% |
| tier2_grouped_aggregate_100_rows/postgres_18 | 49.04 us | -4.25% |
| tier2_select_distinct_100_rows/pg_fake | 48.77 us | -1.33% |
| tier2_select_distinct_100_rows/postgres_18 | 44.34 us | -2.34% |
| tier2_union_all_100_rows/pg_fake | 388.71 us | +6.40% |
| tier2_union_all_100_rows/postgres_18 | 85.81 us | -0.12% |
| tier2_union_100_rows/pg_fake | 424.59 us | +9.63% |
| tier2_union_100_rows/postgres_18 | 86.28 us | -1.19% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_insert_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.32x |
| tier2_update_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.05x |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 4.13x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 3.02x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 3.00x |
| tier2_runtime_temporal_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.54x |
| tier2_runtime_patterns_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.94x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 27.54x |
| tier2_transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 44.89x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 6.28x |
| tier2_alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.01x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 2.17x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.41x |
| tier2_insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.54x |
| tier2_insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🟢 ↑ 1.46x |
| tier2_insert_on_conflict_do_update | postgres_18 | pg_fake | 🟢 ↑ 1.13x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 1.91x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 4.25x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.40x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.67x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.22x |
| tier2_offset_datetime_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.15x |
| tier2_bigint_uuid_array_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.28x |
| tier2_uuid_any_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.07x |
| tier2_array_containment_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.66x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 4.26x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.61x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.66x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 6.13x |
| tier2_window_row_number_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.48x |
| tier2_ordered_string_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.49x |
| tier2_transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.65x |
| tier2_adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.81x |
| tier2_core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 36.62x |
| tier2_point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.76x |
| tier2_point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 465.27x |
| tier2_concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.11x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.77x |
| tier2_derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.50x |
| tier2_materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.44x |
| tier2_derived_source_join_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.92x |
| tier2_correlated_exists_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.18x |
| tier2_global_aggregate_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.14x |
| tier2_grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.66x |
| tier2_select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.10x |
| tier2_union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.53x |
| tier2_union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.92x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_transaction_local_guc_roundtrip/pg_fake | 60.21 us | -1.93% |
| tier3_transaction_local_guc_roundtrip/postgres_18 | 135.13 us | -1.93% |
| tier3_skip_locked_queue_100_rows/pg_fake | 182.72 us | -8.54% |
| tier3_skip_locked_queue_100_rows/postgres_18 | 48.32 us | -29.60% |
| tier3_serializable_uncontended_read/pg_fake | 19.02 us | -0.17% |
| tier3_serializable_uncontended_read/postgres_18 | 85.36 us | -1.38% |
| tier3_serializable_write_skew/pg_fake | 128.31 us | -4.86% |
| tier3_serializable_write_skew/postgres_18 | 308.28 us | +0.73% |
| tier3_lateral_latest_per_parent_100_rows/pg_fake | 1.70 ms | -1.97% |
| tier3_lateral_latest_per_parent_100_rows/postgres_18 | 612.81 us | -7.51% |
| tier3_migration_table_lock_two_relations/pg_fake | 24.78 us | +0.16% |
| tier3_migration_table_lock_two_relations/postgres_18 | 83.64 us | -0.31% |
| tier3_procedural_trigger_insert_update/pg_fake | 68.90 us | +0.91% |
| tier3_procedural_trigger_insert_update/postgres_18 | 122.18 us | -1.33% |
| tier3_partial_unique_index_100_rows/pg_fake | 70.20 us | +2.16% |
| tier3_partial_unique_index_100_rows/postgres_18 | 491.87 us | -21.64% |
| tier3_temporary_table_on_commit_drop/pg_fake | 16.40 us | -1.57% |
| tier3_temporary_table_on_commit_drop/postgres_18 | 319.06 us | +26.14% |
| tier3_catalog_regclass_lookup/pg_fake | 57.11 us | +0.49% |
| tier3_catalog_regclass_lookup/postgres_18 | 33.42 us | +1.69% |
| tier3_ordered_filtered_array_agg_100_rows/pg_fake | 201.80 us | +2.03% |
| tier3_ordered_filtered_array_agg_100_rows/postgres_18 | 44.13 us | +0.66% |
| tier3_correlated_unnest_100_rows/pg_fake | 189.64 us | +0.59% |
| tier3_correlated_unnest_100_rows/postgres_18 | 162.67 us | +1.47% |
| tier3_hashed_advisory_lock_acquisition/pg_fake | 44.31 us | -0.46% |
| tier3_hashed_advisory_lock_acquisition/postgres_18 | 78.88 us | -0.63% |
| tier3_jsonb_join_group/pg_fake | 680.89 us | -9.54% |
| tier3_jsonb_join_group/postgres_18 | 341.58 us | -3.92% |
| tier3_window_rank_100_rows/pg_fake | 291.27 us | -4.71% |
| tier3_window_rank_100_rows/postgres_18 | 110.25 us | +0.75% |
| tier3_window_offset_100_rows/pg_fake | 550.34 us | -4.08% |
| tier3_window_offset_100_rows/postgres_18 | 126.62 us | +0.67% |
| tier3_window_moving_aggregate_100_rows/pg_fake | 387.47 us | -3.26% |
| tier3_window_moving_aggregate_100_rows/postgres_18 | 152.01 us | +0.66% |
| tier3_nested_filtered_view_100_rows/pg_fake | 366.15 us | +3.02% |
| tier3_nested_filtered_view_100_rows/postgres_18 | 35.21 us | -1.00% |
| tier3_transaction_history_point_select/1 | 351.92 ns | -1.26% |
| tier3_transaction_history_point_select/100 | 351.75 ns | -0.99% |
| tier3_transaction_history_point_select/10,000 | 351.34 ns | -1.27% |
| tier3_transaction_history_point_select/100,000 | 350.66 ns | -1.98% |
| tier3_mvcc_old_snapshot_read/1 | 566.65 ns | -10.97% |
| tier3_mvcc_old_snapshot_read/100 | 1.87 us | -1.47% |
| tier3_mvcc_old_snapshot_read/10,000 | 669.47 us | -0.63% |
| tier3_concurrent_same_row_contention/wait_then_rollback | 1.68 ms | -10.01% |
| tier3_data_modifying_cte_update_100_rows/pg_fake | 448.37 us | -76.40% |
| tier3_data_modifying_cte_update_100_rows/postgres_18 | 106.57 us | -71.25% |
| tier3_recursive_cte_numeric_series_100_rows/pg_fake | 135.03 us | -9.85% |
| tier3_recursive_cte_numeric_series_100_rows/postgres_18 | 73.79 us | +7.09% |
| tier3_recursive_cte_branching_traversal_127_rows/pg_fake | 231.58 us | -6.81% |
| tier3_recursive_cte_branching_traversal_127_rows/postgres_18 | 123.57 us | +9.25% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_transaction_local_guc_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 2.24x |
| tier3_skip_locked_queue_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.78x |
| tier3_serializable_uncontended_read | postgres_18 | pg_fake | 🟢 ↑ 4.49x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 2.40x |
| tier3_lateral_latest_per_parent_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.77x |
| tier3_migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 3.37x |
| tier3_procedural_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.77x |
| tier3_partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 7.01x |
| tier3_temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 19.46x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.71x |
| tier3_ordered_filtered_array_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.57x |
| tier3_correlated_unnest_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.17x |
| tier3_hashed_advisory_lock_acquisition | postgres_18 | pg_fake | 🟢 ↑ 1.78x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 1.99x |
| tier3_window_rank_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.64x |
| tier3_window_offset_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.35x |
| tier3_window_moving_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.55x |
| tier3_nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 10.40x |
| tier3_transaction_history_point_select | 1 | 100 | 🟢 ↑ 1.00x |
| tier3_transaction_history_point_select | 1 | 10,000 | 🟢 ↑ 1.00x |
| tier3_transaction_history_point_select | 1 | 100,000 | 🟢 ↑ 1.00x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.30x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1181.46x |
| tier3_data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.21x |
| tier3_recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.83x |
| tier3_recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 1.87x |
