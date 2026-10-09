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
| recorded_at | 2026-10-09T08:54:08Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 38.35 us | +1.06% |
| tier1_insert_row/postgres_18 | 74.89 us | +3.04% |
| tier1_update_row/pg_fake | 44.67 us | +2.75% |
| tier1_update_row/postgres_18 | 96.56 us | +7.15% |
| tier1_transaction_insert/pg_fake | 41.36 us | -1.22% |
| tier1_transaction_insert/postgres_18 | 116.13 us | -0.75% |
| tier1_select_100/pg_fake | 24.68 us | +1.18% |
| tier1_select_100/postgres_18 | 58.77 us | -0.45% |
| tier1_select_where_100/pg_fake | 15.05 us | +3.65% |
| tier1_select_where_100/postgres_18 | 34.23 us | +0.35% |
| tier1_select_where_indexed_100/pg_fake | 8.45 us | +1.57% |
| tier1_select_where_indexed_100/postgres_18 | 34.23 us | -0.11% |
| tier1_limit_offset_ordered_100/pg_fake | 23.39 us | +1.87% |
| tier1_limit_offset_ordered_100/postgres_18 | 41.95 us | -0.81% |
| tier1_order_by_100/pg_fake | 35.41 us | +0.39% |
| tier1_order_by_100/postgres_18 | 66.98 us | +1.46% |
| tier1_selective_inner_join/pg_fake | 18.15 us | +7.69% |
| tier1_selective_inner_join/postgres_18 | 40.22 us | +2.37% |
| tier1_many_match_inner_join/pg_fake | 25.06 us | +2.17% |
| tier1_many_match_inner_join/postgres_18 | 67.75 us | +1.78% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.95x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 2.16x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.81x |
| tier1_select_100 | postgres_18 | pg_fake | 🟢 ↑ 2.38x |
| tier1_select_where_100 | postgres_18 | pg_fake | 🟢 ↑ 2.28x |
| tier1_select_where_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 4.05x |
| tier1_limit_offset_ordered_100 | postgres_18 | pg_fake | 🟢 ↑ 1.79x |
| tier1_order_by_100 | postgres_18 | pg_fake | 🟢 ↑ 1.89x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.22x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.70x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_insert_bound_row/pg_fake | 28.77 us | +2.09% |
| tier2_insert_bound_row/postgres_18 | 35.45 us | -4.61% |
| tier2_update_bound_row/pg_fake | 35.68 us | +5.59% |
| tier2_update_bound_row/postgres_18 | 35.63 us | +0.87% |
| tier2_core_snapshot_100/pg_fake | 16.03 us | +9.12% |
| tier2_core_snapshot_1k/pg_fake | 147.15 us | N/A |
| tier2_session_settings_roundtrip/pg_fake | 35.61 us | +5.66% |
| tier2_session_settings_roundtrip/postgres_18 | 154.34 us | +10.83% |
| tier2_nested_savepoint_release/pg_fake | 66.01 us | +6.01% |
| tier2_nested_savepoint_release/postgres_18 | 183.58 us | -2.53% |
| tier2_nested_savepoint_rollback/pg_fake | 78.18 us | +23.29% |
| tier2_nested_savepoint_rollback/postgres_18 | 250.03 us | +31.49% |
| tier2_runtime_temporal_100/pg_fake | 307.84 us | +2.67% |
| tier2_runtime_temporal_100/postgres_18 | 119.09 us | +0.86% |
| tier2_runtime_patterns_100/pg_fake | 233.81 us | +1.49% |
| tier2_runtime_patterns_100/postgres_18 | 117.59 us | -0.73% |
| tier2_create_table/pg_fake | 37.42 us | +2.61% |
| tier2_create_table/postgres_18 | 955.19 us | -4.89% |
| tier2_ddl_create_rollback/pg_fake | 29.68 us | +0.80% |
| tier2_ddl_create_rollback/postgres_18 | 1.32 ms | +0.02% |
| tier2_sqlx_migration_chain/pg_fake | 980.48 us | -6.13% |
| tier2_sqlx_migration_chain/postgres_18 | 6.66 ms | +1.47% |
| tier2_alter_table_rewrite_100/pg_fake | 322.83 us | -0.06% |
| tier2_alter_table_rewrite_100/postgres_18 | 373.12 us | +17.23% |
| tier2_insert_row_returning/pg_fake | 45.91 us | +0.99% |
| tier2_insert_row_returning/postgres_18 | 100.48 us | +1.74% |
| tier2_insert_row_with_defaults/pg_fake | 40.87 us | +2.55% |
| tier2_insert_row_with_defaults/postgres_18 | 94.44 us | -1.50% |
| tier2_on_conflict_nothing/pg_fake | 19.94 us | +1.28% |
| tier2_on_conflict_nothing/postgres_18 | 31.03 us | +2.42% |
| tier2_on_conflict_no_conflict/pg_fake | 47.20 us | +0.62% |
| tier2_on_conflict_no_conflict/postgres_18 | 67.74 us | -0.78% |
| tier2_on_conflict_update/pg_fake | 31.66 us | +0.67% |
| tier2_on_conflict_update/postgres_18 | 35.14 us | -1.01% |
| tier2_update_from_row/pg_fake | 58.04 us | +8.50% |
| tier2_update_from_row/postgres_18 | 103.40 us | +1.02% |
| tier2_delete_row/pg_fake | 28.11 us | +0.52% |
| tier2_delete_row/postgres_18 | 116.22 us | -2.08% |
| tier2_sequence_nextval/pg_fake | 20.20 us | -0.58% |
| tier2_sequence_nextval/postgres_18 | 28.38 us | +0.15% |
| tier2_serial_identity_insert/pg_fake | 19.98 us | +1.27% |
| tier2_serial_identity_insert/postgres_18 | 33.21 us | +1.01% |
| tier2_uuid_temporal_select/pg_fake | 24.66 us | -1.55% |
| tier2_uuid_temporal_select/postgres_18 | 31.03 us | +1.35% |
| tier2_offset_datetime_roundtrip/pg_fake | 41.77 us | +0.44% |
| tier2_offset_datetime_roundtrip/postgres_18 | 36.70 us | +1.21% |
| tier2_int8_uuid_array_roundtrip/pg_fake | 48.00 us | +0.82% |
| tier2_int8_uuid_array_roundtrip/postgres_18 | 36.88 us | -0.71% |
| tier2_uuid_any_100/pg_fake | 63.67 us | +12.19% |
| tier2_uuid_any_100/postgres_18 | 61.42 us | +16.33% |
| tier2_array_containment_100/pg_fake | 115.66 us | +4.35% |
| tier2_array_containment_100/postgres_18 | 41.21 us | -0.91% |
| tier2_json_insert_returning/pg_fake | 34.69 us | +1.17% |
| tier2_json_insert_returning/postgres_18 | 144.97 us | -0.86% |
| tier2_jsonb_insert_returning/pg_fake | 40.22 us | +1.31% |
| tier2_jsonb_insert_returning/postgres_18 | 157.36 us | +9.78% |
| tier2_jsonb_extraction/pg_fake | 116.19 us | +4.62% |
| tier2_jsonb_extraction/postgres_18 | 66.00 us | -1.16% |
| tier2_jsonb_containment/pg_fake | 280.65 us | +4.72% |
| tier2_jsonb_containment/postgres_18 | 44.39 us | +1.51% |
| tier2_window_row_number_100/pg_fake | 132.55 us | +6.72% |
| tier2_window_row_number_100/postgres_18 | 85.37 us | +1.45% |
| tier2_ordered_string_agg_100/pg_fake | 242.46 us | -1.41% |
| tier2_ordered_string_agg_100/postgres_18 | 71.99 us | +2.05% |
| tier2_rr_select_for_update/pg_fake | 52.39 us | +1.40% |
| tier2_rr_select_for_update/postgres_18 | 84.78 us | -0.75% |
| tier2_adapter_select_100/core | 13.83 us | -0.56% |
| tier2_adapter_select_100/sqlx | 25.95 us | +2.88% |
| tier2_core_parse_vs_prepare/parse_and_analyze | 19.03 us | -3.04% |
| tier2_core_parse_vs_prepare/prepared_reuse | 517.10 ns | -3.51% |
| tier2_lookup_index_vs_scan/heap_scan/100 | 3.32 us | -0.19% |
| tier2_lookup_index_vs_scan/unique_index/100 | 563.52 ns | -2.45% |
| tier2_lookup_index_vs_scan/heap_scan/10,000 | 293.47 us | -1.13% |
| tier2_lookup_index_vs_scan/unique_index/10,000 | 628.01 ns | -1.56% |
| tier2_lookup_unique_100/pg_fake | 8.30 us | N/A |
| tier2_lookup_unique_100/postgres_18 | 38.18 us | N/A |
| tier2_lookup_unique_1k/pg_fake | 8.88 us | N/A |
| tier2_lookup_unique_1k/postgres_18 | 32.87 us | N/A |
| tier2_lookup_nonuniq_100/pg_fake | 8.02 us | N/A |
| tier2_lookup_nonuniq_100/postgres_18 | 51.39 us | N/A |
| tier2_lookup_nonuniq_1k/pg_fake | 9.07 us | N/A |
| tier2_lookup_nonuniq_1k/postgres_18 | 33.79 us | N/A |
| tier2_lookup_nonuniq_filter_100/pg_fake | 8.83 us | N/A |
| tier2_lookup_nonuniq_filter_100/postgres_18 | 39.54 us | N/A |
| tier2_lookup_nonuniq_filter_1k/pg_fake | 8.75 us | N/A |
| tier2_lookup_nonuniq_filter_1k/postgres_18 | 34.81 us | N/A |
| tier2_lookup_heap_scan_100/pg_fake | 17.39 us | N/A |
| tier2_lookup_heap_scan_100/postgres_18 | 38.58 us | N/A |
| tier2_lookup_heap_scan_1k/pg_fake | 87.21 us | N/A |
| tier2_lookup_heap_scan_1k/postgres_18 | 65.56 us | N/A |
| tier2_update_indexed_100/pg_fake | 32.42 us | N/A |
| tier2_update_indexed_100/postgres_18 | 40.76 us | N/A |
| tier2_update_indexed_1k/pg_fake | 34.60 us | N/A |
| tier2_update_indexed_1k/postgres_18 | 35.68 us | N/A |
| tier2_delete_indexed_100/pg_fake | 22.09 us | N/A |
| tier2_delete_indexed_100/postgres_18 | 38.45 us | N/A |
| tier2_delete_indexed_1k/pg_fake | 22.27 us | N/A |
| tier2_delete_indexed_1k/postgres_18 | 34.36 us | N/A |
| tier2_concurrent_reads/sequential | 13.88 us | +12.97% |
| tier2_concurrent_reads/parallel | 11.50 us | +3.89% |
| tier2_foreign_key_insert/pg_fake | 71.03 us | +7.52% |
| tier2_foreign_key_insert/postgres_18 | 188.96 us | +3.11% |
| tier2_join_3way_100/pg_fake | 44.19 us | N/A |
| tier2_join_3way_100/postgres_18 | 33.61 us | N/A |
| tier2_join_3way_1k/pg_fake | 41.98 us | N/A |
| tier2_join_3way_1k/postgres_18 | 48.80 us | N/A |
| tier2_join_3way_filtered_100/pg_fake | 46.91 us | N/A |
| tier2_join_3way_filtered_100/postgres_18 | 32.18 us | N/A |
| tier2_join_3way_filtered_1k/pg_fake | 51.19 us | N/A |
| tier2_join_3way_filtered_1k/postgres_18 | 46.78 us | N/A |
| tier2_join_2way_filtered_100/pg_fake | 45.74 us | N/A |
| tier2_join_2way_filtered_100/postgres_18 | 33.60 us | N/A |
| tier2_join_2way_filtered_1k/pg_fake | 46.32 us | N/A |
| tier2_join_2way_filtered_1k/postgres_18 | 46.55 us | N/A |
| tier2_derived_scalar_subq_100/pg_fake | 359.56 us | -7.76% |
| tier2_derived_scalar_subq_100/postgres_18 | 126.24 us | +45.69% |
| tier2_materialized_cte_100/pg_fake | 118.15 us | +4.62% |
| tier2_materialized_cte_100/postgres_18 | 99.06 us | +26.48% |
| tier2_derived_source_join_100/pg_fake | 143.17 us | +0.39% |
| tier2_derived_source_join_100/postgres_18 | 73.98 us | -0.56% |
| tier2_correlated_exists_100/pg_fake | 72.58 us | +2.16% |
| tier2_correlated_exists_100/postgres_18 | 120.18 us | +43.88% |
| tier2_global_aggregate_100/pg_fake | 35.67 us | +1.02% |
| tier2_global_aggregate_100/postgres_18 | 40.93 us | +1.88% |
| tier2_grouped_aggregate_100/pg_fake | 182.24 us | +1.57% |
| tier2_grouped_aggregate_100/postgres_18 | 51.35 us | +4.71% |
| tier2_select_distinct_100/pg_fake | 49.98 us | +2.50% |
| tier2_select_distinct_100/postgres_18 | 45.66 us | +2.97% |
| tier2_union_all_100/pg_fake | 390.85 us | +0.55% |
| tier2_union_all_100/postgres_18 | 87.64 us | +2.14% |
| tier2_union_100/pg_fake | 425.86 us | +0.30% |
| tier2_union_100/postgres_18 | 86.56 us | +0.33% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_insert_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.23x |
| tier2_update_bound_row | postgres_18 | pg_fake | 🔴 ↓ 1.00x |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 4.33x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 2.78x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 3.20x |
| tier2_runtime_temporal_100 | postgres_18 | pg_fake | 🔴 ↓ 2.58x |
| tier2_runtime_patterns_100 | postgres_18 | pg_fake | 🔴 ↓ 1.99x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 25.53x |
| tier2_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 44.54x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 6.79x |
| tier2_alter_table_rewrite_100 | postgres_18 | pg_fake | 🟢 ↑ 1.16x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 2.19x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.31x |
| tier2_on_conflict_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.56x |
| tier2_on_conflict_no_conflict | postgres_18 | pg_fake | 🟢 ↑ 1.44x |
| tier2_on_conflict_update | postgres_18 | pg_fake | 🟢 ↑ 1.11x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 1.78x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 4.14x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.41x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.66x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.26x |
| tier2_offset_datetime_roundtrip | postgres_18 | pg_fake | 🔴 ↓ 1.14x |
| tier2_int8_uuid_array_roundtrip | postgres_18 | pg_fake | 🔴 ↓ 1.30x |
| tier2_uuid_any_100 | postgres_18 | pg_fake | 🔴 ↓ 1.04x |
| tier2_array_containment_100 | postgres_18 | pg_fake | 🔴 ↓ 2.81x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 4.18x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.91x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.76x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 6.32x |
| tier2_window_row_number_100 | postgres_18 | pg_fake | 🔴 ↓ 1.55x |
| tier2_ordered_string_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 3.37x |
| tier2_rr_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.62x |
| tier2_adapter_select_100 | core | sqlx | 🔴 ↓ 1.88x |
| tier2_core_parse_vs_prepare | parse_and_analyze | prepared_reuse | 🟢 ↑ 36.79x |
| tier2_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.89x |
| tier2_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 467.29x |
| tier2_lookup_unique_100 | postgres_18 | pg_fake | 🟢 ↑ 4.60x |
| tier2_lookup_unique_1k | postgres_18 | pg_fake | 🟢 ↑ 3.70x |
| tier2_lookup_nonuniq_100 | postgres_18 | pg_fake | 🟢 ↑ 6.41x |
| tier2_lookup_nonuniq_1k | postgres_18 | pg_fake | 🟢 ↑ 3.72x |
| tier2_lookup_nonuniq_filter_100 | postgres_18 | pg_fake | 🟢 ↑ 4.48x |
| tier2_lookup_nonuniq_filter_1k | postgres_18 | pg_fake | 🟢 ↑ 3.98x |
| tier2_lookup_heap_scan_100 | postgres_18 | pg_fake | 🟢 ↑ 2.22x |
| tier2_lookup_heap_scan_1k | postgres_18 | pg_fake | 🔴 ↓ 1.33x |
| tier2_update_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.26x |
| tier2_update_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.03x |
| tier2_delete_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.74x |
| tier2_delete_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.54x |
| tier2_concurrent_reads | sequential | parallel | 🟢 ↑ 1.21x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.66x |
| tier2_join_3way_100 | postgres_18 | pg_fake | 🔴 ↓ 1.31x |
| tier2_join_3way_1k | postgres_18 | pg_fake | 🟢 ↑ 1.16x |
| tier2_join_3way_filtered_100 | postgres_18 | pg_fake | 🔴 ↓ 1.46x |
| tier2_join_3way_filtered_1k | postgres_18 | pg_fake | 🔴 ↓ 1.09x |
| tier2_join_2way_filtered_100 | postgres_18 | pg_fake | 🔴 ↓ 1.36x |
| tier2_join_2way_filtered_1k | postgres_18 | pg_fake | 🟢 ↑ 1.00x |
| tier2_derived_scalar_subq_100 | postgres_18 | pg_fake | 🔴 ↓ 2.85x |
| tier2_materialized_cte_100 | postgres_18 | pg_fake | 🔴 ↓ 1.19x |
| tier2_derived_source_join_100 | postgres_18 | pg_fake | 🔴 ↓ 1.94x |
| tier2_correlated_exists_100 | postgres_18 | pg_fake | 🟢 ↑ 1.66x |
| tier2_global_aggregate_100 | postgres_18 | pg_fake | 🟢 ↑ 1.15x |
| tier2_grouped_aggregate_100 | postgres_18 | pg_fake | 🔴 ↓ 3.55x |
| tier2_select_distinct_100 | postgres_18 | pg_fake | 🔴 ↓ 1.09x |
| tier2_union_all_100 | postgres_18 | pg_fake | 🔴 ↓ 4.46x |
| tier2_union_100 | postgres_18 | pg_fake | 🔴 ↓ 4.92x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_tx_local_guc/pg_fake | 61.81 us | +2.66% |
| tier3_tx_local_guc/postgres_18 | 138.86 us | +2.76% |
| tier3_skip_locked_queue_100/pg_fake | 184.22 us | +0.82% |
| tier3_skip_locked_queue_100/postgres_18 | 66.45 us | +37.52% |
| tier3_serializable_read/pg_fake | 30.74 us | +61.65% |
| tier3_serializable_read/postgres_18 | 94.17 us | +10.32% |
| tier3_serializable_write_skew/pg_fake | 115.28 us | -10.16% |
| tier3_serializable_write_skew/postgres_18 | 323.12 us | +4.81% |
| tier3_lateral_latest_100/pg_fake | 1.68 ms | -1.04% |
| tier3_lateral_latest_100/postgres_18 | 862.72 us | +40.78% |
| tier3_migration_lock_2tables/pg_fake | 91.13 us | +267.71% |
| tier3_migration_lock_2tables/postgres_18 | 98.67 us | +17.97% |
| tier3_trigger_insert_update/pg_fake | 65.72 us | -4.62% |
| tier3_trigger_insert_update/postgres_18 | 123.54 us | +1.12% |
| tier3_partial_unique_index_100/pg_fake | 68.79 us | -2.01% |
| tier3_partial_unique_index_100/postgres_18 | 532.67 us | +8.30% |
| tier3_temp_on_commit_drop/pg_fake | 17.45 us | +6.43% |
| tier3_temp_on_commit_drop/postgres_18 | 375.25 us | +17.61% |
| tier3_catalog_regclass_lookup/pg_fake | 58.84 us | +3.04% |
| tier3_catalog_regclass_lookup/postgres_18 | 33.59 us | +0.51% |
| tier3_array_agg_order_filter_100/pg_fake | 202.43 us | +0.31% |
| tier3_array_agg_order_filter_100/postgres_18 | 43.96 us | -0.38% |
| tier3_correlated_unnest_100/pg_fake | 185.81 us | -2.02% |
| tier3_correlated_unnest_100/postgres_18 | 150.27 us | -7.62% |
| tier3_hashed_advisory_lock/pg_fake | 44.95 us | +1.44% |
| tier3_hashed_advisory_lock/postgres_18 | 78.78 us | -0.13% |
| tier3_jsonb_join_group/pg_fake | 659.45 us | -3.15% |
| tier3_jsonb_join_group/postgres_18 | 340.12 us | -0.43% |
| tier3_window_rank_100/pg_fake | 305.62 us | +4.92% |
| tier3_window_rank_100/postgres_18 | 109.25 us | -0.91% |
| tier3_window_offset_100/pg_fake | 610.81 us | +10.99% |
| tier3_window_offset_100/postgres_18 | 130.24 us | +2.86% |
| tier3_window_moving_agg_100/pg_fake | 407.64 us | +5.21% |
| tier3_window_moving_agg_100/postgres_18 | 151.01 us | -0.66% |
| tier3_nested_filtered_view_100/pg_fake | 360.35 us | -1.58% |
| tier3_nested_filtered_view_100/postgres_18 | 35.78 us | +1.62% |
| tier3_tx_history_lookup/1 | 335.84 ns | -4.57% |
| tier3_tx_history_lookup/100 | 334.87 ns | -4.80% |
| tier3_tx_history_lookup/10,000 | 336.75 ns | -4.15% |
| tier3_tx_history_lookup/100,000 | 337.71 ns | -3.69% |
| tier3_mvcc_old_snapshot_read/1 | 556.00 ns | -1.88% |
| tier3_mvcc_old_snapshot_read/100 | 1.81 us | -3.32% |
| tier3_mvcc_old_snapshot_read/10,000 | 672.08 us | +0.39% |
| tier3_same_row_contention/wait_then_rollback | 1.84 ms | +9.48% |
| tier3_cte_update_100/pg_fake | 437.29 us | -2.47% |
| tier3_cte_update_100/postgres_18 | 105.07 us | -1.41% |
| tier3_recursive_series_100/pg_fake | 133.87 us | -0.85% |
| tier3_recursive_series_100/postgres_18 | 74.82 us | +1.39% |
| tier3_recursive_tree_127/pg_fake | 230.91 us | -0.29% |
| tier3_recursive_tree_127/postgres_18 | 124.61 us | +0.84% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_tx_local_guc | postgres_18 | pg_fake | 🟢 ↑ 2.25x |
| tier3_skip_locked_queue_100 | postgres_18 | pg_fake | 🔴 ↓ 2.77x |
| tier3_serializable_read | postgres_18 | pg_fake | 🟢 ↑ 3.06x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 2.80x |
| tier3_lateral_latest_100 | postgres_18 | pg_fake | 🔴 ↓ 1.95x |
| tier3_migration_lock_2tables | postgres_18 | pg_fake | 🟢 ↑ 1.08x |
| tier3_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.88x |
| tier3_partial_unique_index_100 | postgres_18 | pg_fake | 🟢 ↑ 7.74x |
| tier3_temp_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 21.50x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.75x |
| tier3_array_agg_order_filter_100 | postgres_18 | pg_fake | 🔴 ↓ 4.60x |
| tier3_correlated_unnest_100 | postgres_18 | pg_fake | 🔴 ↓ 1.24x |
| tier3_hashed_advisory_lock | postgres_18 | pg_fake | 🟢 ↑ 1.75x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 1.94x |
| tier3_window_rank_100 | postgres_18 | pg_fake | 🔴 ↓ 2.80x |
| tier3_window_offset_100 | postgres_18 | pg_fake | 🔴 ↓ 4.69x |
| tier3_window_moving_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 2.70x |
| tier3_nested_filtered_view_100 | postgres_18 | pg_fake | 🔴 ↓ 10.07x |
| tier3_tx_history_lookup | 1 | 100 | 🟢 ↑ 1.00x |
| tier3_tx_history_lookup | 1 | 10,000 | 🔴 ↓ 1.00x |
| tier3_tx_history_lookup | 1 | 100,000 | 🔴 ↓ 1.01x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.25x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1208.79x |
| tier3_cte_update_100 | postgres_18 | pg_fake | 🔴 ↓ 4.16x |
| tier3_recursive_series_100 | postgres_18 | pg_fake | 🔴 ↓ 1.79x |
| tier3_recursive_tree_127 | postgres_18 | pg_fake | 🔴 ↓ 1.85x |
