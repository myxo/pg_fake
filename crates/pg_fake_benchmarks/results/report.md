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
| recorded_at | 2026-10-03T08:13:50Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 42.56 us | -17.05% |
| tier1_insert_row/postgres_18 | 94.13 us | -0.75% |
| tier1_update_row/pg_fake | 31.24 us | -13.03% |
| tier1_update_row/postgres_18 | 94.30 us | +0.52% |
| tier1_transaction_insert/pg_fake | 43.48 us | -20.62% |
| tier1_transaction_insert/postgres_18 | 137.45 us | +0.99% |
| tier1_select_100_rows/pg_fake | 29.20 us | -19.13% |
| tier1_select_100_rows/postgres_18 | 58.54 us | +1.17% |
| tier1_select_where_100_rows/pg_fake | 15.77 us | -33.79% |
| tier1_select_where_100_rows/postgres_18 | 34.54 us | +0.01% |
| tier1_select_where_indexed_100_rows/pg_fake | 7.70 us | -50.99% |
| tier1_select_where_indexed_100_rows/postgres_18 | 34.56 us | +9.98% |
| tier1_limit_offset_ordered_100_rows/pg_fake | 35.42 us | -13.93% |
| tier1_limit_offset_ordered_100_rows/postgres_18 | 42.36 us | +0.92% |
| tier1_order_by_100_rows/pg_fake | 40.33 us | -9.60% |
| tier1_order_by_100_rows/postgres_18 | 66.39 us | +1.34% |
| tier1_selective_inner_join/pg_fake | 93.02 us | -8.12% |
| tier1_selective_inner_join/postgres_18 | 39.79 us | +0.50% |
| tier1_many_match_inner_join/pg_fake | 162.23 us | -3.93% |
| tier1_many_match_inner_join/postgres_18 | 65.72 us | -0.54% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 2.21x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 3.02x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 3.16x |
| tier1_select_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.00x |
| tier1_select_where_100_rows | postgres_18 | pg_fake | 🟢 ↑ 2.19x |
| tier1_select_where_indexed_100_rows | postgres_18 | pg_fake | 🟢 ↑ 4.49x |
| tier1_limit_offset_ordered_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.20x |
| tier1_order_by_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.65x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.34x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🔴 ↓ 2.47x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_core_snapshot_100_rows/pg_fake | 14.99 us | -4.89% |
| tier2_session_settings_roundtrip/pg_fake | 34.48 us | -50.20% |
| tier2_session_settings_roundtrip/postgres_18 | 139.65 us | +8.05% |
| tier2_nested_savepoint_release/pg_fake | 63.81 us | -38.80% |
| tier2_nested_savepoint_release/postgres_18 | 187.84 us | -0.08% |
| tier2_nested_savepoint_rollback/pg_fake | 64.28 us | -39.62% |
| tier2_nested_savepoint_rollback/postgres_18 | 190.66 us | +0.57% |
| tier2_runtime_temporal_100_rows/pg_fake | 306.05 us | -1.36% |
| tier2_runtime_temporal_100_rows/postgres_18 | 117.37 us | -0.18% |
| tier2_runtime_patterns_100_rows/pg_fake | 238.80 us | -1.51% |
| tier2_runtime_patterns_100_rows/postgres_18 | 117.76 us | +0.27% |
| tier2_create_table/pg_fake | 39.27 us | -32.79% |
| tier2_create_table/postgres_18 | 947.44 us | -16.92% |
| tier2_transactional_ddl_create_rollback/pg_fake | 29.68 us | -31.73% |
| tier2_transactional_ddl_create_rollback/postgres_18 | 1.42 ms | -18.95% |
| tier2_sqlx_migration_chain/pg_fake | 945.98 us | -11.70% |
| tier2_sqlx_migration_chain/postgres_18 | 6.56 ms | +2.57% |
| tier2_alter_table_rewrite_100_rows/pg_fake | 312.10 us | -3.32% |
| tier2_alter_table_rewrite_100_rows/postgres_18 | 330.15 us | -11.67% |
| tier2_insert_row_returning/pg_fake | 48.64 us | -15.87% |
| tier2_insert_row_returning/postgres_18 | 98.70 us | +0.39% |
| tier2_insert_row_with_defaults/pg_fake | 42.92 us | -32.69% |
| tier2_insert_row_with_defaults/postgres_18 | 96.66 us | +1.25% |
| tier2_insert_on_conflict_do_nothing/pg_fake | 20.45 us | -27.79% |
| tier2_insert_on_conflict_do_nothing/postgres_18 | 30.60 us | -9.09% |
| tier2_insert_on_conflict_conflict_free/pg_fake | 49.83 us | -21.28% |
| tier2_insert_on_conflict_conflict_free/postgres_18 | 68.37 us | -0.24% |
| tier2_insert_on_conflict_do_update/pg_fake | 32.95 us | -17.11% |
| tier2_insert_on_conflict_do_update/postgres_18 | 35.61 us | +0.84% |
| tier2_update_from_row/pg_fake | 37.17 us | -11.55% |
| tier2_update_from_row/postgres_18 | 102.33 us | +0.57% |
| tier2_delete_row/pg_fake | 16.75 us | -28.53% |
| tier2_delete_row/postgres_18 | 119.44 us | +0.57% |
| tier2_sequence_nextval/pg_fake | 20.49 us | -26.75% |
| tier2_sequence_nextval/postgres_18 | 28.37 us | -0.71% |
| tier2_serial_identity_insert/pg_fake | 20.10 us | -27.15% |
| tier2_serial_identity_insert/postgres_18 | 33.46 us | +0.52% |
| tier2_uuid_temporal_select/pg_fake | 25.27 us | -28.59% |
| tier2_uuid_temporal_select/postgres_18 | 31.08 us | -0.16% |
| tier2_offset_datetime_bind_store_fetch/pg_fake | 42.08 us | -12.25% |
| tier2_offset_datetime_bind_store_fetch/postgres_18 | 36.23 us | -0.35% |
| tier2_bigint_uuid_array_bind_store_fetch/pg_fake | 47.85 us | -11.90% |
| tier2_bigint_uuid_array_bind_store_fetch/postgres_18 | 36.06 us | -3.15% |
| tier2_uuid_any_100_rows/pg_fake | 58.61 us | -2.45% |
| tier2_uuid_any_100_rows/postgres_18 | 65.97 us | +23.94% |
| tier2_array_containment_100_rows/pg_fake | 114.24 us | -6.25% |
| tier2_array_containment_100_rows/postgres_18 | 41.71 us | +0.22% |
| tier2_json_insert_returning/pg_fake | 35.01 us | -26.14% |
| tier2_json_insert_returning/postgres_18 | 115.89 us | -19.70% |
| tier2_jsonb_insert_returning/pg_fake | 40.66 us | -19.56% |
| tier2_jsonb_insert_returning/postgres_18 | 145.41 us | -0.10% |
| tier2_jsonb_extraction/pg_fake | 116.10 us | -3.57% |
| tier2_jsonb_extraction/postgres_18 | 66.58 us | +0.26% |
| tier2_jsonb_containment/pg_fake | 265.65 us | -1.86% |
| tier2_jsonb_containment/postgres_18 | 43.69 us | +0.07% |
| tier2_window_row_number_100_rows/pg_fake | 130.90 us | -10.44% |
| tier2_window_row_number_100_rows/postgres_18 | 84.12 us | +0.26% |
| tier2_ordered_string_agg_100_rows/pg_fake | 243.06 us | -2.22% |
| tier2_ordered_string_agg_100_rows/postgres_18 | 70.88 us | -30.85% |
| tier2_transaction_repeatable_read_select_for_update/pg_fake | 52.45 us | -19.00% |
| tier2_transaction_repeatable_read_select_for_update/postgres_18 | 86.74 us | -0.25% |
| tier2_adapter_overhead_select_100_rows/core | 32.76 us | +0.42% |
| tier2_adapter_overhead_select_100_rows/sqlx | 30.81 us | -15.67% |
| tier2_core_parsed_vs_prepared_point_select/parse_and_analyze | 19.29 us | -3.90% |
| tier2_core_parsed_vs_prepared_point_select/prepared_reuse | 594.94 ns | -7.48% |
| tier2_point_lookup_index_vs_scan/heap_scan/100 | 3.97 us | -0.86% |
| tier2_point_lookup_index_vs_scan/unique_index/100 | 628.04 ns | -1.68% |
| tier2_point_lookup_index_vs_scan/heap_scan/10,000 | 359.88 us | +1.81% |
| tier2_point_lookup_index_vs_scan/unique_index/10,000 | 701.00 ns | -6.21% |
| tier2_concurrent_uncontended_reads/sequential | 13.35 us | -54.20% |
| tier2_concurrent_uncontended_reads/parallel | 11.10 us | -43.99% |
| tier2_foreign_key_insert/pg_fake | 71.20 us | -20.63% |
| tier2_foreign_key_insert/postgres_18 | 179.29 us | -6.75% |
| tier2_derived_and_scalar_subquery_100_rows/pg_fake | 451.22 us | +19.81% |
| tier2_derived_and_scalar_subquery_100_rows/postgres_18 | 205.83 us | +136.60% |
| tier2_materialized_cte_100_rows/pg_fake | 594.73 us | +378.99% |
| tier2_materialized_cte_100_rows/postgres_18 | 4.18 ms | +5226.96% |
| tier2_derived_source_join_100_rows/pg_fake | 725.33 us | +367.02% |
| tier2_derived_source_join_100_rows/postgres_18 | 3.44 ms | +4552.46% |
| tier2_correlated_exists_100_rows/pg_fake | 394.30 us | +387.33% |
| tier2_correlated_exists_100_rows/postgres_18 | 457.67 us | +448.56% |
| tier2_global_aggregate_100_rows/pg_fake | 36.33 us | -19.52% |
| tier2_global_aggregate_100_rows/postgres_18 | 44.05 us | +8.88% |
| tier2_grouped_aggregate_100_rows/pg_fake | 182.73 us | -0.82% |
| tier2_grouped_aggregate_100_rows/postgres_18 | 51.22 us | +4.85% |
| tier2_select_distinct_100_rows/pg_fake | 49.42 us | -6.70% |
| tier2_select_distinct_100_rows/postgres_18 | 45.40 us | -0.05% |
| tier2_union_all_100_rows/pg_fake | 365.32 us | -13.41% |
| tier2_union_all_100_rows/postgres_18 | 85.91 us | -1.13% |
| tier2_union_100_rows/pg_fake | 387.30 us | -13.30% |
| tier2_union_100_rows/postgres_18 | 87.32 us | +0.73% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 4.05x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 2.94x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 2.97x |
| tier2_runtime_temporal_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.61x |
| tier2_runtime_patterns_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.03x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 24.12x |
| tier2_transactional_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 47.77x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 6.94x |
| tier2_alter_table_rewrite_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.06x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 2.03x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.25x |
| tier2_insert_on_conflict_do_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.50x |
| tier2_insert_on_conflict_conflict_free | postgres_18 | pg_fake | 🟢 ↑ 1.37x |
| tier2_insert_on_conflict_do_update | postgres_18 | pg_fake | 🟢 ↑ 1.08x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 2.75x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 7.13x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.38x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.66x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.23x |
| tier2_offset_datetime_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.16x |
| tier2_bigint_uuid_array_bind_store_fetch | postgres_18 | pg_fake | 🔴 ↓ 1.33x |
| tier2_uuid_any_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.13x |
| tier2_array_containment_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.74x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.31x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.58x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.74x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 6.08x |
| tier2_window_row_number_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.56x |
| tier2_ordered_string_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.43x |
| tier2_transaction_repeatable_read_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.65x |
| tier2_adapter_overhead_select_100_rows | core | sqlx | 🟢 ↑ 1.06x |
| tier2_core_parsed_vs_prepared_point_select | parse_and_analyze | prepared_reuse | 🟢 ↑ 32.42x |
| tier2_point_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 6.31x |
| tier2_point_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 513.38x |
| tier2_concurrent_uncontended_reads | sequential | parallel | 🟢 ↑ 1.20x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.52x |
| tier2_derived_and_scalar_subquery_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.19x |
| tier2_materialized_cte_100_rows | postgres_18 | pg_fake | 🟢 ↑ 7.02x |
| tier2_derived_source_join_100_rows | postgres_18 | pg_fake | 🟢 ↑ 4.75x |
| tier2_correlated_exists_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.16x |
| tier2_global_aggregate_100_rows | postgres_18 | pg_fake | 🟢 ↑ 1.21x |
| tier2_grouped_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 3.57x |
| tier2_select_distinct_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.09x |
| tier2_union_all_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.25x |
| tier2_union_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.44x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_transaction_local_guc_roundtrip/pg_fake | 61.39 us | -31.45% |
| tier3_transaction_local_guc_roundtrip/postgres_18 | 137.80 us | +1.16% |
| tier3_skip_locked_queue_100_rows/pg_fake | 199.78 us | +5.25% |
| tier3_skip_locked_queue_100_rows/postgres_18 | 68.63 us | +41.30% |
| tier3_serializable_uncontended_read/pg_fake | 19.05 us | -48.67% |
| tier3_serializable_uncontended_read/postgres_18 | 86.56 us | -0.09% |
| tier3_serializable_write_skew/pg_fake | 134.86 us | -29.20% |
| tier3_serializable_write_skew/postgres_18 | 306.04 us | +0.77% |
| tier3_lateral_latest_per_parent_100_rows/pg_fake | 1.73 ms | +1.25% |
| tier3_lateral_latest_per_parent_100_rows/postgres_18 | 662.58 us | +34.47% |
| tier3_migration_table_lock_two_relations/pg_fake | 24.74 us | -18.30% |
| tier3_migration_table_lock_two_relations/postgres_18 | 83.90 us | -0.46% |
| tier3_procedural_trigger_insert_update/pg_fake | 68.28 us | -24.99% |
| tier3_procedural_trigger_insert_update/postgres_18 | 123.82 us | +0.66% |
| tier3_partial_unique_index_100_rows/pg_fake | 68.71 us | -20.54% |
| tier3_partial_unique_index_100_rows/postgres_18 | 627.74 us | +11.95% |
| tier3_temporary_table_on_commit_drop/pg_fake | 16.66 us | -27.52% |
| tier3_temporary_table_on_commit_drop/postgres_18 | 252.94 us | +13.78% |
| tier3_catalog_regclass_lookup/pg_fake | 56.83 us | -10.01% |
| tier3_catalog_regclass_lookup/postgres_18 | 32.87 us | -0.83% |
| tier3_ordered_filtered_array_agg_100_rows/pg_fake | 197.78 us | -5.64% |
| tier3_ordered_filtered_array_agg_100_rows/postgres_18 | 43.84 us | -0.20% |
| tier3_correlated_unnest_100_rows/pg_fake | 188.53 us | -10.55% |
| tier3_correlated_unnest_100_rows/postgres_18 | 160.30 us | -1.06% |
| tier3_hashed_advisory_lock_acquisition/pg_fake | 44.52 us | -14.97% |
| tier3_hashed_advisory_lock_acquisition/postgres_18 | 79.38 us | +0.27% |
| tier3_jsonb_join_group/pg_fake | 752.70 us | +0.11% |
| tier3_jsonb_join_group/postgres_18 | 355.51 us | +6.57% |
| tier3_window_rank_100_rows/pg_fake | 305.66 us | -7.61% |
| tier3_window_rank_100_rows/postgres_18 | 109.43 us | -0.05% |
| tier3_window_offset_100_rows/pg_fake | 573.74 us | -6.57% |
| tier3_window_offset_100_rows/postgres_18 | 125.78 us | -0.70% |
| tier3_window_moving_aggregate_100_rows/pg_fake | 400.50 us | -0.52% |
| tier3_window_moving_aggregate_100_rows/postgres_18 | 151.01 us | -10.15% |
| tier3_nested_filtered_view_100_rows/pg_fake | 355.40 us | -5.64% |
| tier3_nested_filtered_view_100_rows/postgres_18 | 35.57 us | -0.20% |
| tier3_transaction_history_point_select/1 | 356.42 ns | -3.09% |
| tier3_transaction_history_point_select/100 | 355.28 ns | -2.64% |
| tier3_transaction_history_point_select/10,000 | 355.87 ns | -1.82% |
| tier3_transaction_history_point_select/100,000 | 357.76 ns | -3.07% |
| tier3_mvcc_old_snapshot_read/1 | 636.44 ns | -2.11% |
| tier3_mvcc_old_snapshot_read/100 | 1.90 us | -1.02% |
| tier3_mvcc_old_snapshot_read/10,000 | 673.70 us | +0.28% |
| tier3_concurrent_same_row_contention/wait_then_rollback | 1.86 ms | +5.27% |
| tier3_data_modifying_cte_update_100_rows/pg_fake | 1.90 ms | +319.70% |
| tier3_data_modifying_cte_update_100_rows/postgres_18 | 370.67 us | +250.11% |
| tier3_recursive_cte_numeric_series_100_rows/pg_fake | 149.78 us | +3.88% |
| tier3_recursive_cte_numeric_series_100_rows/postgres_18 | 68.91 us | -11.32% |
| tier3_recursive_cte_branching_traversal_127_rows/pg_fake | 248.51 us | -1.63% |
| tier3_recursive_cte_branching_traversal_127_rows/postgres_18 | 113.11 us | -7.55% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_transaction_local_guc_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 2.24x |
| tier3_skip_locked_queue_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.91x |
| tier3_serializable_uncontended_read | postgres_18 | pg_fake | 🟢 ↑ 4.54x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 2.27x |
| tier3_lateral_latest_per_parent_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.62x |
| tier3_migration_table_lock_two_relations | postgres_18 | pg_fake | 🟢 ↑ 3.39x |
| tier3_procedural_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.81x |
| tier3_partial_unique_index_100_rows | postgres_18 | pg_fake | 🟢 ↑ 9.14x |
| tier3_temporary_table_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 15.18x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.73x |
| tier3_ordered_filtered_array_agg_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.51x |
| tier3_correlated_unnest_100_rows | postgres_18 | pg_fake | 🔴 ↓ 1.18x |
| tier3_hashed_advisory_lock_acquisition | postgres_18 | pg_fake | 🟢 ↑ 1.78x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 2.12x |
| tier3_window_rank_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.79x |
| tier3_window_offset_100_rows | postgres_18 | pg_fake | 🔴 ↓ 4.56x |
| tier3_window_moving_aggregate_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.65x |
| tier3_nested_filtered_view_100_rows | postgres_18 | pg_fake | 🔴 ↓ 9.99x |
| tier3_transaction_history_point_select | 1 | 100 | 🟢 ↑ 1.00x |
| tier3_transaction_history_point_select | 1 | 10,000 | 🟢 ↑ 1.00x |
| tier3_transaction_history_point_select | 1 | 100,000 | 🔴 ↓ 1.00x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 2.98x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1058.54x |
| tier3_data_modifying_cte_update_100_rows | postgres_18 | pg_fake | 🔴 ↓ 5.12x |
| tier3_recursive_cte_numeric_series_100_rows | postgres_18 | pg_fake | 🔴 ↓ 2.17x |
| tier3_recursive_cte_branching_traversal_127_rows | postgres_18 | pg_fake | 🔴 ↓ 2.20x |
