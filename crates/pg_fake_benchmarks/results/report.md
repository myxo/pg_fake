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
| recorded_at | 2026-09-30T05:10:05Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 51.31 us | -8.71% |
| tier1_insert_row/postgres_18 | 94.85 us | +2.24% |
| tier1_update_row/pg_fake | 35.92 us | -24.69% |
| tier1_update_row/postgres_18 | 93.81 us | -0.06% |
| tier1_transaction_insert/pg_fake | 54.77 us | +6.09% |
| tier1_transaction_insert/postgres_18 | 136.10 us | +0.20% |
| tier1_select_100_rows/pg_fake | 36.11 us | +14.26% |
| tier1_select_100_rows/postgres_18 | 57.87 us | -0.95% |
| tier1_select_where_100_rows/pg_fake | 23.82 us | +32.45% |
| tier1_select_where_100_rows/postgres_18 | 34.54 us | +2.95% |
| tier1_select_where_indexed_100_rows/pg_fake | 15.71 us | +26.72% |
| tier1_select_where_indexed_100_rows/postgres_18 | 31.43 us | -7.27% |
| tier1_limit_offset_ordered_100_rows/pg_fake | 41.16 us | -45.84% |
| tier1_limit_offset_ordered_100_rows/postgres_18 | 41.98 us | -0.53% |
| tier1_order_by_100_rows/pg_fake | 44.62 us | -43.37% |
| tier1_order_by_100_rows/postgres_18 | 65.51 us | -0.60% |
| tier1_selective_inner_join/pg_fake | 101.24 us | -6.10% |
| tier1_selective_inner_join/postgres_18 | 39.59 us | +1.29% |
| tier1_many_match_inner_join/pg_fake | 168.86 us | -4.16% |
| tier1_many_match_inner_join/postgres_18 | 66.08 us | -0.68% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.85x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 2.61x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.48x |
| tier1_select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.60x |
| tier1_select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.45x |
| tier1_select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.00x |
| tier1_limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.02x |
| tier1_order_by_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.47x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.56x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.56x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_core_snapshot_100_rows/pg_fake | 15.76 us | +4.26% |
| tier2_session_settings_roundtrip/pg_fake | 69.23 us | +0.53% |
| tier2_session_settings_roundtrip/postgres_18 | 129.24 us | -6.97% |
| tier2_nested_savepoint_release/pg_fake | 104.26 us | +8.70% |
| tier2_nested_savepoint_release/postgres_18 | 187.99 us | +0.69% |
| tier2_nested_savepoint_rollback/pg_fake | 106.46 us | +5.76% |
| tier2_nested_savepoint_rollback/postgres_18 | 189.58 us | +0.86% |
| tier2_runtime_temporal_100_rows/pg_fake | 310.28 us | -67.59% |
| tier2_runtime_temporal_100_rows/postgres_18 | 117.58 us | +0.80% |
| tier2_runtime_patterns_100_rows/pg_fake | 242.47 us | -11.75% |
| tier2_runtime_patterns_100_rows/postgres_18 | 117.45 us | +0.83% |
| tier2_create_table/pg_fake | 58.43 us | -7.67% |
| tier2_create_table/postgres_18 | 1.14 ms | +28.29% |
| tier2_transactional_ddl_create_rollback/pg_fake | 43.48 us | -6.92% |
| tier2_transactional_ddl_create_rollback/postgres_18 | 1.75 ms | +34.33% |
| tier2_sqlx_migration_chain/pg_fake | 1.07 ms | -23.98% |
| tier2_sqlx_migration_chain/postgres_18 | 6.40 ms | +1.97% |
| tier2_alter_table_rewrite_100_rows/pg_fake | 322.80 us | -4.05% |
| tier2_alter_table_rewrite_100_rows/postgres_18 | 373.77 us | +5.75% |
| tier2_insert_row_returning/pg_fake | 57.82 us | -12.05% |
| tier2_insert_row_returning/postgres_18 | 98.32 us | +0.29% |
| tier2_insert_row_with_defaults/pg_fake | 63.77 us | +4.39% |
| tier2_insert_row_with_defaults/postgres_18 | 95.47 us | +0.25% |
| tier2_insert_on_conflict_do_nothing/pg_fake | 28.32 us | -19.53% |
| tier2_insert_on_conflict_do_nothing/postgres_18 | 33.66 us | +8.94% |
| tier2_insert_on_conflict_conflict_free/pg_fake | 63.29 us | -20.33% |
| tier2_insert_on_conflict_conflict_free/postgres_18 | 68.54 us | +0.84% |
| tier2_insert_on_conflict_do_update/pg_fake | 39.75 us | -13.81% |
| tier2_insert_on_conflict_do_update/postgres_18 | 35.31 us | -0.93% |
| tier2_update_from_row/pg_fake | 42.02 us | -19.87% |
| tier2_update_from_row/postgres_18 | 101.74 us | +0.12% |
| tier2_delete_row/pg_fake | 23.44 us | -29.77% |
| tier2_delete_row/postgres_18 | 118.76 us | -0.37% |
| tier2_sequence_nextval/pg_fake | 27.97 us | -26.61% |
| tier2_sequence_nextval/postgres_18 | 28.58 us | +0.27% |
| tier2_serial_identity_insert/pg_fake | 27.59 us | -27.55% |
| tier2_serial_identity_insert/postgres_18 | 33.28 us | +2.83% |
| tier2_uuid_temporal_select/pg_fake | 35.39 us | -20.50% |
| tier2_uuid_temporal_select/postgres_18 | 31.13 us | -10.40% |
| tier2_offset_datetime_bind_store_fetch/pg_fake | 47.96 us | -14.93% |
| tier2_offset_datetime_bind_store_fetch/postgres_18 | 36.36 us | +0.10% |
| tier2_bigint_uuid_array_bind_store_fetch/pg_fake | 54.31 us | -12.93% |
| tier2_bigint_uuid_array_bind_store_fetch/postgres_18 | 37.23 us | +0.10% |
| tier2_uuid_any_100_rows/pg_fake | 60.08 us | -11.21% |
| tier2_uuid_any_100_rows/postgres_18 | 53.23 us | -8.50% |
| tier2_array_containment_100_rows/pg_fake | 121.86 us | -8.90% |
| tier2_array_containment_100_rows/postgres_18 | 41.62 us | +1.63% |
| tier2_json_insert_returning/pg_fake | 47.40 us | -30.70% |
| tier2_json_insert_returning/postgres_18 | 144.33 us | -0.47% |
| tier2_jsonb_insert_returning/pg_fake | 50.55 us | -31.21% |
| tier2_jsonb_insert_returning/postgres_18 | 145.55 us | +2.51% |
| tier2_jsonb_extraction/pg_fake | 120.40 us | -4.91% |
| tier2_jsonb_extraction/postgres_18 | 66.41 us | +0.26% |
| tier2_jsonb_containment/pg_fake | 270.70 us | -0.49% |
| tier2_jsonb_containment/postgres_18 | 43.66 us | +1.49% |
| tier2_window_row_number_100_rows/pg_fake | 146.15 us | -21.22% |
| tier2_window_row_number_100_rows/postgres_18 | 83.89 us | +0.60% |
| tier2_ordered_string_agg_100_rows/pg_fake | 248.57 us | -8.00% |
| tier2_ordered_string_agg_100_rows/postgres_18 | 102.50 us | +44.57% |
| tier2_transaction_repeatable_read_select_for_update/pg_fake | 64.75 us | -4.43% |
| tier2_transaction_repeatable_read_select_for_update/postgres_18 | 86.95 us | +2.45% |
| tier2_adapter_overhead_select_100_rows/core | 32.62 us | -52.19% |
| tier2_adapter_overhead_select_100_rows/sqlx | 36.54 us | -47.25% |
| tier2_core_parsed_vs_prepared_point_select/parse_and_analyze | 20.07 us | +6.34% |
| tier2_core_parsed_vs_prepared_point_select/prepared_reuse | 643.03 ns | +14.21% |
| tier2_point_lookup_index_vs_scan/heap_scan/100 | 4.00 us | +13.48% |
| tier2_point_lookup_index_vs_scan/unique_index/100 | 638.75 ns | +4.65% |
| tier2_point_lookup_index_vs_scan/heap_scan/10,000 | 353.49 us | +13.71% |
| tier2_point_lookup_index_vs_scan/unique_index/10,000 | 747.38 ns | +11.93% |
| tier2_concurrent_uncontended_reads/sequential | 29.15 us | +53.63% |
| tier2_concurrent_uncontended_reads/parallel | 19.81 us | +16.74% |
| tier2_foreign_key_insert/pg_fake | 89.71 us | -37.41% |
| tier2_foreign_key_insert/postgres_18 | 192.27 us | +5.83% |
| tier2_derived_and_scalar_subquery_100_rows/pg_fake | 376.60 us | -16.77% |
| tier2_derived_and_scalar_subquery_100_rows/postgres_18 | 87.00 us | +1.90% |
| tier2_materialized_cte_100_rows/pg_fake | 124.16 us | -3.97% |
| tier2_materialized_cte_100_rows/postgres_18 | 78.43 us | -0.54% |
| tier2_derived_source_join_100_rows/pg_fake | 155.31 us | N/A |
| tier2_derived_source_join_100_rows/postgres_18 | 73.98 us | N/A |
| tier2_correlated_exists_100_rows/pg_fake | 80.91 us | -8.80% |
| tier2_correlated_exists_100_rows/postgres_18 | 83.43 us | +0.02% |
| tier2_global_aggregate_100_rows/pg_fake | 45.14 us | +8.01% |
| tier2_global_aggregate_100_rows/postgres_18 | 40.46 us | +2.08% |
| tier2_grouped_aggregate_100_rows/pg_fake | 184.24 us | -8.99% |
| tier2_grouped_aggregate_100_rows/postgres_18 | 48.85 us | -0.95% |
| tier2_select_distinct_100_rows/pg_fake | 52.97 us | -16.06% |
| tier2_select_distinct_100_rows/postgres_18 | 45.43 us | +1.20% |
| tier2_union_all_100_rows/pg_fake | 421.87 us | -9.59% |
| tier2_union_all_100_rows/postgres_18 | 86.89 us | -0.25% |
| tier2_union_100_rows/pg_fake | 446.71 us | -10.29% |
| tier2_union_100_rows/postgres_18 | 86.69 us | -27.49% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 1.87x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 1.80x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 1.78x |
| tier2_runtime_temporal_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.64x |
| tier2_runtime_patterns_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.06x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 19.52x |
| tier2_transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 40.24x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 5.97x |
| tier2_alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.16x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 1.70x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 1.50x |
| tier2_insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.19x |
| tier2_insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🟢 ↑ 1.08x |
| tier2_insert_on_conflict_do_update | postgres_18 | pg_fake | 🔴 ↓ 1.13x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 2.42x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 5.07x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.02x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.21x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🔴 ↓ 1.14x |
| tier2_offset_datetime_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.32x |
| tier2_bigint_uuid_array_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.46x |
| tier2_uuid_any_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.13x |
| tier2_array_containment_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.93x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.04x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.88x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.81x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 6.20x |
| tier2_window_row_number_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.74x |
| tier2_ordered_string_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.43x |
| tier2_transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.34x |
| tier2_adapter_overhead_select_100_rows | core | sqlx | 🔴 ↓ 1.12x |
| tier2_core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 31.21x |
| tier2_point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 6.26x |
| tier2_point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 472.97x |
| tier2_concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.47x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.14x |
| tier2_derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.33x |
| tier2_materialized_cte_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.58x |
| tier2_derived_source_join_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.10x |
| tier2_correlated_exists_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.03x |
| tier2_global_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.12x |
| tier2_grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.77x |
| tier2_select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.17x |
| tier2_union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.86x |
| tier2_union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.15x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_transaction_local_guc_roundtrip/pg_fake | 89.56 us | -6.54% |
| tier3_transaction_local_guc_roundtrip/postgres_18 | 136.21 us | -0.22% |
| tier3_skip_locked_queue_100_rows/pg_fake | 189.82 us | -4.10% |
| tier3_skip_locked_queue_100_rows/postgres_18 | 48.57 us | -0.32% |
| tier3_serializable_uncontended_read/pg_fake | 37.12 us | +34.48% |
| tier3_serializable_uncontended_read/postgres_18 | 86.64 us | +0.16% |
| tier3_serializable_write_skew/pg_fake | 190.49 us | +4.85% |
| tier3_serializable_write_skew/postgres_18 | 303.69 us | +0.35% |
| tier3_lateral_latest_per_parent_100_rows/pg_fake | 1.71 ms | -76.94% |
| tier3_lateral_latest_per_parent_100_rows/postgres_18 | 492.73 us | -22.11% |
| tier3_migration_table_lock_two_relations/pg_fake | 30.29 us | -22.78% |
| tier3_migration_table_lock_two_relations/postgres_18 | 84.29 us | +2.79% |
| tier3_procedural_trigger_insert_update/pg_fake | 91.03 us | -8.82% |
| tier3_procedural_trigger_insert_update/postgres_18 | 123.01 us | +0.65% |
| tier3_partial_unique_index_100_rows/pg_fake | 86.48 us | -13.26% |
| tier3_partial_unique_index_100_rows/postgres_18 | 560.75 us | +1.65% |
| tier3_temporary_table_on_commit_drop/pg_fake | 22.99 us | -21.99% |
| tier3_temporary_table_on_commit_drop/postgres_18 | 222.31 us | -13.91% |
| tier3_catalog_regclass_lookup/pg_fake | 63.15 us | -13.63% |
| tier3_catalog_regclass_lookup/postgres_18 | 33.14 us | +0.74% |
| tier3_ordered_filtered_array_agg_100_rows/pg_fake | 209.60 us | -1.90% |
| tier3_ordered_filtered_array_agg_100_rows/postgres_18 | 43.93 us | +0.92% |
| tier3_correlated_unnest_100_rows/pg_fake | 210.76 us | -1.51% |
| tier3_correlated_unnest_100_rows/postgres_18 | 162.02 us | +11.59% |
| tier3_hashed_advisory_lock_acquisition/pg_fake | 52.36 us | -3.38% |
| tier3_hashed_advisory_lock_acquisition/postgres_18 | 79.17 us | +3.68% |
| tier3_jsonb_join_group/pg_fake | 751.89 us | -1.57% |
| tier3_jsonb_join_group/postgres_18 | 333.58 us | +0.85% |
| tier3_window_rank_100_rows/pg_fake | 330.83 us | -35.93% |
| tier3_window_rank_100_rows/postgres_18 | 109.49 us | +1.07% |
| tier3_window_offset_100_rows/pg_fake | 614.07 us | -42.26% |
| tier3_window_offset_100_rows/postgres_18 | 126.66 us | +1.57% |
| tier3_window_moving_aggregate_100_rows/pg_fake | 402.59 us | -34.35% |
| tier3_window_moving_aggregate_100_rows/postgres_18 | 168.07 us | +12.28% |
| tier3_nested_filtered_view_100_rows/pg_fake | 376.65 us | -10.33% |
| tier3_nested_filtered_view_100_rows/postgres_18 | 35.64 us | +2.65% |
| tier3_transaction_history_point_select/1 | 367.80 ns | +5.53% |
| tier3_transaction_history_point_select/100 | 364.90 ns | +4.13% |
| tier3_transaction_history_point_select/10,000 | 362.46 ns | +4.23% |
| tier3_transaction_history_point_select/100,000 | 369.07 ns | +5.77% |
| tier3_mvcc_old_snapshot_read/1 | 650.15 ns | +3.43% |
| tier3_mvcc_old_snapshot_read/100 | 1.92 us | +0.77% |
| tier3_mvcc_old_snapshot_read/10,000 | 671.85 us | +0.93% |
| tier3_concurrent_same_row_contention/wait_then_rollback | 1.77 ms | -10.95% |
| tier3_data_modifying_cte_update_100_rows/pg_fake | 452.59 us | -1.67% |
| tier3_data_modifying_cte_update_100_rows/postgres_18 | 105.87 us | +2.05% |
| tier3_recursive_cte_numeric_series_100_rows/pg_fake | 144.19 us | -86.10% |
| tier3_recursive_cte_numeric_series_100_rows/postgres_18 | 77.70 us | +4.85% |
| tier3_recursive_cte_branching_traversal_127_rows/pg_fake | 252.64 us | -23.53% |
| tier3_recursive_cte_branching_traversal_127_rows/postgres_18 | 122.35 us | +0.60% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_transaction_local_guc_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 1.52x |
| tier3_skip_locked_queue_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.91x |
| tier3_serializable_uncontended_read | postgres_18 | pg_fake | 🟢 ↑ 2.33x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 1.59x |
| tier3_lateral_latest_per_parent_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.47x |
| tier3_migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 2.78x |
| tier3_procedural_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.35x |
| tier3_partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 6.48x |
| tier3_temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 9.67x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.91x |
| tier3_ordered_filtered_array_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.77x |
| tier3_correlated_unnest_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.30x |
| tier3_hashed_advisory_lock_acquisition | postgres_18 | pg_fake | 🟢 ↑ 1.51x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 2.25x |
| tier3_window_rank_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.02x |
| tier3_window_offset_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.85x |
| tier3_window_moving_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.40x |
| tier3_nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 10.57x |
| tier3_transaction_history_point_select | 1 | 100 | 🟢 ↑ 1.01x |
| tier3_transaction_history_point_select | 1 | 10,000 | 🟢 ↑ 1.01x |
| tier3_transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.00x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 2.95x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1033.38x |
| tier3_data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.27x |
| tier3_recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.86x |
| tier3_recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 2.06x |
