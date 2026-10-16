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
| recorded_at | 2026-10-16T07:10:00Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 28.54 us | +0.28% |
| tier1_insert_row/postgres_18 | 36.34 us | -0.29% |
| tier1_update_row/pg_fake | 34.93 us | +0.50% |
| tier1_update_row/postgres_18 | 35.86 us | -0.24% |
| tier1_transaction_insert/pg_fake | 34.93 us | +2.34% |
| tier1_transaction_insert/postgres_18 | 80.33 us | +0.05% |
| tier1_select_100/pg_fake | 26.90 us | +0.31% |
| tier1_select_100/postgres_18 | 60.43 us | +1.00% |
| tier1_select_where_100/pg_fake | 15.30 us | +2.11% |
| tier1_select_where_100/postgres_18 | 34.89 us | -0.40% |
| tier1_select_where_indexed_100/pg_fake | 7.71 us | +0.03% |
| tier1_select_where_indexed_100/postgres_18 | 35.03 us | +15.06% |
| tier1_limit_offset_ordered_100/pg_fake | 65.61 us | +0.75% |
| tier1_limit_offset_ordered_100/postgres_18 | 43.15 us | -11.26% |
| tier1_order_by_100/pg_fake | 37.84 us | +1.75% |
| tier1_order_by_100/postgres_18 | 67.89 us | +0.29% |
| tier1_selective_inner_join/pg_fake | 16.79 us | +0.43% |
| tier1_selective_inner_join/postgres_18 | 40.39 us | -0.29% |
| tier1_many_match_inner_join/pg_fake | 23.05 us | -1.91% |
| tier1_many_match_inner_join/postgres_18 | 66.48 us | -0.11% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.27x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 1.03x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.30x |
| tier1_select_100 | postgres_18 | pg_fake | 🟢 ↑ 2.25x |
| tier1_select_where_100 | postgres_18 | pg_fake | 🟢 ↑ 2.28x |
| tier1_select_where_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 4.54x |
| tier1_limit_offset_ordered_100 | postgres_18 | pg_fake | 🔴 ↓ 1.52x |
| tier1_order_by_100 | postgres_18 | pg_fake | 🟢 ↑ 1.79x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.41x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.88x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_insert_bound_row/pg_fake | 28.44 us | -0.33% |
| tier2_insert_bound_row/postgres_18 | 35.07 us | -1.44% |
| tier2_update_bound_row/pg_fake | 35.12 us | +0.60% |
| tier2_update_bound_row/postgres_18 | 35.44 us | -0.41% |
| tier2_core_snapshot_100/pg_fake | 14.37 us | -16.48% |
| tier2_core_snapshot_1k/pg_fake | 142.55 us | -5.36% |
| tier2_session_settings_roundtrip/pg_fake | 34.93 us | +5.78% |
| tier2_session_settings_roundtrip/postgres_18 | 138.54 us | -1.22% |
| tier2_nested_savepoint_release/pg_fake | 64.80 us | +0.20% |
| tier2_nested_savepoint_release/postgres_18 | 187.20 us | +0.16% |
| tier2_nested_savepoint_rollback/pg_fake | 64.23 us | +1.80% |
| tier2_nested_savepoint_rollback/postgres_18 | 188.74 us | -0.23% |
| tier2_runtime_temporal_100/pg_fake | 297.20 us | -0.28% |
| tier2_runtime_temporal_100/postgres_18 | 117.85 us | +0.21% |
| tier2_runtime_patterns_100/pg_fake | 234.14 us | -0.58% |
| tier2_runtime_patterns_100/postgres_18 | 117.75 us | +0.23% |
| tier2_create_table/pg_fake | 36.74 us | -0.95% |
| tier2_create_table/postgres_18 | 876.49 us | -3.89% |
| tier2_ddl_create_rollback/pg_fake | 29.60 us | +0.67% |
| tier2_ddl_create_rollback/postgres_18 | 1.20 ms | +5.58% |
| tier2_sqlx_migration_chain/pg_fake | 939.95 us | +0.27% |
| tier2_sqlx_migration_chain/postgres_18 | 6.09 ms | +1.70% |
| tier2_alter_table_rewrite_100/pg_fake | 284.70 us | -2.17% |
| tier2_alter_table_rewrite_100/postgres_18 | 392.18 us | +14.50% |
| tier2_insert_row_returning/pg_fake | 46.03 us | +1.08% |
| tier2_insert_row_returning/postgres_18 | 98.11 us | +0.24% |
| tier2_insert_row_with_defaults/pg_fake | 40.37 us | +0.51% |
| tier2_insert_row_with_defaults/postgres_18 | 94.88 us | -0.17% |
| tier2_on_conflict_nothing/pg_fake | 19.87 us | +0.29% |
| tier2_on_conflict_nothing/postgres_18 | 30.73 us | -0.66% |
| tier2_on_conflict_no_conflict/pg_fake | 47.64 us | -0.06% |
| tier2_on_conflict_no_conflict/postgres_18 | 68.26 us | +0.14% |
| tier2_on_conflict_update/pg_fake | 31.62 us | -0.12% |
| tier2_on_conflict_update/postgres_18 | 35.22 us | +0.10% |
| tier2_update_from_row/pg_fake | 53.42 us | +0.95% |
| tier2_update_from_row/postgres_18 | 102.62 us | -1.01% |
| tier2_delete_row/pg_fake | 28.04 us | -0.63% |
| tier2_delete_row/postgres_18 | 104.26 us | -0.83% |
| tier2_sequence_nextval/pg_fake | 20.32 us | -1.98% |
| tier2_sequence_nextval/postgres_18 | 28.49 us | -0.81% |
| tier2_serial_identity_insert/pg_fake | 19.93 us | -8.90% |
| tier2_serial_identity_insert/postgres_18 | 34.09 us | -3.89% |
| tier2_uuid_temporal_select/pg_fake | 25.18 us | -9.16% |
| tier2_uuid_temporal_select/postgres_18 | 30.75 us | -7.72% |
| tier2_offset_datetime_roundtrip/pg_fake | 42.59 us | -3.91% |
| tier2_offset_datetime_roundtrip/postgres_18 | 36.98 us | -5.35% |
| tier2_int8_uuid_array_roundtrip/pg_fake | 48.09 us | +1.71% |
| tier2_int8_uuid_array_roundtrip/postgres_18 | 37.60 us | +3.14% |
| tier2_uuid_any_100/pg_fake | 56.27 us | -63.58% |
| tier2_uuid_any_100/postgres_18 | 52.88 us | -39.90% |
| tier2_array_containment_100/pg_fake | 111.81 us | -0.12% |
| tier2_array_containment_100/postgres_18 | 41.63 us | -1.87% |
| tier2_json_insert_returning/pg_fake | 34.66 us | -0.37% |
| tier2_json_insert_returning/postgres_18 | 93.64 us | +0.45% |
| tier2_jsonb_insert_returning/pg_fake | 40.31 us | +0.28% |
| tier2_jsonb_insert_returning/postgres_18 | 93.77 us | +0.46% |
| tier2_jsonb_extraction/pg_fake | 112.52 us | +1.42% |
| tier2_jsonb_extraction/postgres_18 | 67.46 us | +1.60% |
| tier2_jsonb_containment/pg_fake | 260.40 us | +0.74% |
| tier2_jsonb_containment/postgres_18 | 44.27 us | +1.09% |
| tier2_window_row_number_100/pg_fake | 126.88 us | -0.69% |
| tier2_window_row_number_100/postgres_18 | 84.03 us | +0.61% |
| tier2_ordered_string_agg_100/pg_fake | 241.28 us | -0.44% |
| tier2_ordered_string_agg_100/postgres_18 | 70.58 us | +0.69% |
| tier2_rr_select_for_update/pg_fake | 52.47 us | +0.51% |
| tier2_rr_select_for_update/postgres_18 | 85.80 us | +0.44% |
| tier2_adapter_select_100/core | 13.35 us | +2.43% |
| tier2_adapter_select_100/sqlx | 24.69 us | +0.23% |
| tier2_core_parse_vs_prepare/parse_and_analyze | 19.32 us | +0.38% |
| tier2_core_parse_vs_prepare/prepared_reuse | 537.89 ns | +1.50% |
| tier2_lookup_index_vs_scan/heap_scan/100 | 3.44 us | +0.43% |
| tier2_lookup_index_vs_scan/unique_index/100 | 584.77 ns | +1.77% |
| tier2_lookup_index_vs_scan/heap_scan/10,000 | 307.35 us | +0.79% |
| tier2_lookup_index_vs_scan/unique_index/10,000 | 646.94 ns | +1.42% |
| tier2_lookup_unique_100/pg_fake | 7.98 us | -3.88% |
| tier2_lookup_unique_100/postgres_18 | 35.31 us | -0.56% |
| tier2_lookup_unique_1k/pg_fake | 7.88 us | -2.77% |
| tier2_lookup_unique_1k/postgres_18 | 31.33 us | +1.78% |
| tier2_lookup_nonuniq_100/pg_fake | 7.69 us | -6.29% |
| tier2_lookup_nonuniq_100/postgres_18 | 35.48 us | +0.74% |
| tier2_lookup_nonuniq_1k/pg_fake | 7.73 us | +6.18% |
| tier2_lookup_nonuniq_1k/postgres_18 | 31.34 us | +3.84% |
| tier2_lookup_nonuniq_filter_100/pg_fake | 7.57 us | -4.05% |
| tier2_lookup_nonuniq_filter_100/postgres_18 | 35.51 us | -2.08% |
| tier2_lookup_nonuniq_filter_1k/pg_fake | 7.37 us | +16.71% |
| tier2_lookup_nonuniq_filter_1k/postgres_18 | 31.92 us | +0.65% |
| tier2_lookup_heap_scan_100/pg_fake | 14.83 us | -2.25% |
| tier2_lookup_heap_scan_100/postgres_18 | 34.92 us | -1.23% |
| tier2_lookup_heap_scan_1k/pg_fake | 73.86 us | -7.18% |
| tier2_lookup_heap_scan_1k/postgres_18 | 59.84 us | -3.00% |
| tier2_update_indexed_100/pg_fake | 29.79 us | -3.70% |
| tier2_update_indexed_100/postgres_18 | 37.72 us | -0.19% |
| tier2_update_indexed_1k/pg_fake | 30.57 us | -1.22% |
| tier2_update_indexed_1k/postgres_18 | 33.29 us | +3.34% |
| tier2_delete_indexed_100/pg_fake | 21.32 us | +2.99% |
| tier2_delete_indexed_100/postgres_18 | 36.06 us | -1.14% |
| tier2_delete_indexed_1k/pg_fake | 21.23 us | -0.89% |
| tier2_delete_indexed_1k/postgres_18 | 32.19 us | -1.05% |
| tier2_concurrent_reads/sequential | 12.81 us | +9.66% |
| tier2_concurrent_reads/parallel | 10.76 us | +0.37% |
| tier2_foreign_key_insert/pg_fake | 66.40 us | +0.71% |
| tier2_foreign_key_insert/postgres_18 | 180.60 us | -0.24% |
| tier2_join_3way_100/pg_fake | 7.44 us | +1.53% |
| tier2_join_3way_100/postgres_18 | 37.56 us | -0.39% |
| tier2_join_3way_1k/pg_fake | 7.48 us | -1.72% |
| tier2_join_3way_1k/postgres_18 | 36.18 us | +0.60% |
| tier2_join_3way_filtered_100/pg_fake | 7.52 us | +1.50% |
| tier2_join_3way_filtered_100/postgres_18 | 38.36 us | -1.47% |
| tier2_join_3way_filtered_1k/pg_fake | 7.52 us | -1.57% |
| tier2_join_3way_filtered_1k/postgres_18 | 36.07 us | +0.54% |
| tier2_join_2way_filtered_100/pg_fake | 48.15 us | +0.50% |
| tier2_join_2way_filtered_100/postgres_18 | 36.74 us | -0.91% |
| tier2_join_2way_filtered_1k/pg_fake | 48.18 us | -0.10% |
| tier2_join_2way_filtered_1k/postgres_18 | 34.19 us | +0.37% |
| tier2_join_profile_100/pg_fake | 12.49 us | +7.41% |
| tier2_join_profile_100/postgres_18 | 59.67 us | -1.31% |
| tier2_join_profile_1k/pg_fake | 12.34 us | +2.20% |
| tier2_join_profile_1k/postgres_18 | 43.05 us | -0.65% |
| tier2_join_profile_3k/pg_fake | 12.36 us | +2.72% |
| tier2_join_profile_3k/postgres_18 | 42.83 us | -1.73% |
| tier2_join_members_100/pg_fake | 31.33 us | -0.92% |
| tier2_join_members_100/postgres_18 | 72.41 us | -0.09% |
| tier2_join_members_1k/pg_fake | 33.32 us | -0.68% |
| tier2_join_members_1k/postgres_18 | 60.00 us | +0.70% |
| tier2_join_members_3k/pg_fake | 34.44 us | +0.49% |
| tier2_join_members_3k/postgres_18 | 59.45 us | +0.27% |
| tier2_derived_scalar_subq_100/pg_fake | 343.98 us | -0.65% |
| tier2_derived_scalar_subq_100/postgres_18 | 85.07 us | -0.38% |
| tier2_materialized_cte_100/pg_fake | 109.94 us | +0.67% |
| tier2_materialized_cte_100/postgres_18 | 78.16 us | -0.02% |
| tier2_derived_source_join_100/pg_fake | 139.82 us | +0.54% |
| tier2_derived_source_join_100/postgres_18 | 74.33 us | +0.45% |
| tier2_correlated_exists_100/pg_fake | 68.58 us | +0.11% |
| tier2_correlated_exists_100/postgres_18 | 82.40 us | -0.21% |
| tier2_global_aggregate_100/pg_fake | 35.42 us | +3.14% |
| tier2_global_aggregate_100/postgres_18 | 40.06 us | -0.23% |
| tier2_grouped_aggregate_100/pg_fake | 173.03 us | -0.58% |
| tier2_grouped_aggregate_100/postgres_18 | 49.20 us | -0.25% |
| tier2_select_distinct_100/pg_fake | 47.06 us | -5.56% |
| tier2_select_distinct_100/postgres_18 | 45.09 us | -0.63% |
| tier2_union_all_100/pg_fake | 384.44 us | -1.34% |
| tier2_union_all_100/postgres_18 | 85.22 us | -0.23% |
| tier2_union_100/pg_fake | 396.31 us | -5.49% |
| tier2_union_100/postgres_18 | 85.47 us | -0.27% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_insert_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.23x |
| tier2_update_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.01x |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 3.97x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 2.89x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 2.94x |
| tier2_runtime_temporal_100 | postgres_18 | pg_fake | 🔴 ↓ 2.52x |
| tier2_runtime_patterns_100 | postgres_18 | pg_fake | 🔴 ↓ 1.99x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 23.86x |
| tier2_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 40.56x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 6.47x |
| tier2_alter_table_rewrite_100 | postgres_18 | pg_fake | 🟢 ↑ 1.38x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 2.13x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.35x |
| tier2_on_conflict_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.55x |
| tier2_on_conflict_no_conflict | postgres_18 | pg_fake | 🟢 ↑ 1.43x |
| tier2_on_conflict_update | postgres_18 | pg_fake | 🟢 ↑ 1.11x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 1.92x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 3.72x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.40x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.71x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.22x |
| tier2_offset_datetime_roundtrip | postgres_18 | pg_fake | 🔴 ↓ 1.15x |
| tier2_int8_uuid_array_roundtrip | postgres_18 | pg_fake | 🔴 ↓ 1.28x |
| tier2_uuid_any_100 | postgres_18 | pg_fake | 🔴 ↓ 1.06x |
| tier2_array_containment_100 | postgres_18 | pg_fake | 🔴 ↓ 2.69x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.70x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.33x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.67x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 5.88x |
| tier2_window_row_number_100 | postgres_18 | pg_fake | 🔴 ↓ 1.51x |
| tier2_ordered_string_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 3.42x |
| tier2_rr_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.64x |
| tier2_adapter_select_100 | core | sqlx | 🔴 ↓ 1.85x |
| tier2_core_parse_vs_prepare | parse_and_analyze | prepared_reuse | 🟢 ↑ 35.92x |
| tier2_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.89x |
| tier2_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 475.07x |
| tier2_lookup_unique_100 | postgres_18 | pg_fake | 🟢 ↑ 4.42x |
| tier2_lookup_unique_1k | postgres_18 | pg_fake | 🟢 ↑ 3.98x |
| tier2_lookup_nonuniq_100 | postgres_18 | pg_fake | 🟢 ↑ 4.61x |
| tier2_lookup_nonuniq_1k | postgres_18 | pg_fake | 🟢 ↑ 4.06x |
| tier2_lookup_nonuniq_filter_100 | postgres_18 | pg_fake | 🟢 ↑ 4.69x |
| tier2_lookup_nonuniq_filter_1k | postgres_18 | pg_fake | 🟢 ↑ 4.33x |
| tier2_lookup_heap_scan_100 | postgres_18 | pg_fake | 🟢 ↑ 2.36x |
| tier2_lookup_heap_scan_1k | postgres_18 | pg_fake | 🔴 ↓ 1.23x |
| tier2_update_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.27x |
| tier2_update_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.09x |
| tier2_delete_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.69x |
| tier2_delete_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.52x |
| tier2_concurrent_reads | sequential | parallel | 🟢 ↑ 1.19x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.72x |
| tier2_join_3way_100 | postgres_18 | pg_fake | 🟢 ↑ 5.05x |
| tier2_join_3way_1k | postgres_18 | pg_fake | 🟢 ↑ 4.84x |
| tier2_join_3way_filtered_100 | postgres_18 | pg_fake | 🟢 ↑ 5.10x |
| tier2_join_3way_filtered_1k | postgres_18 | pg_fake | 🟢 ↑ 4.79x |
| tier2_join_2way_filtered_100 | postgres_18 | pg_fake | 🔴 ↓ 1.31x |
| tier2_join_2way_filtered_1k | postgres_18 | pg_fake | 🔴 ↓ 1.41x |
| tier2_join_profile_100 | postgres_18 | pg_fake | 🟢 ↑ 4.78x |
| tier2_join_profile_1k | postgres_18 | pg_fake | 🟢 ↑ 3.49x |
| tier2_join_profile_3k | postgres_18 | pg_fake | 🟢 ↑ 3.47x |
| tier2_join_members_100 | postgres_18 | pg_fake | 🟢 ↑ 2.31x |
| tier2_join_members_1k | postgres_18 | pg_fake | 🟢 ↑ 1.80x |
| tier2_join_members_3k | postgres_18 | pg_fake | 🟢 ↑ 1.73x |
| tier2_derived_scalar_subq_100 | postgres_18 | pg_fake | 🔴 ↓ 4.04x |
| tier2_materialized_cte_100 | postgres_18 | pg_fake | 🔴 ↓ 1.41x |
| tier2_derived_source_join_100 | postgres_18 | pg_fake | 🔴 ↓ 1.88x |
| tier2_correlated_exists_100 | postgres_18 | pg_fake | 🟢 ↑ 1.20x |
| tier2_global_aggregate_100 | postgres_18 | pg_fake | 🟢 ↑ 1.13x |
| tier2_grouped_aggregate_100 | postgres_18 | pg_fake | 🔴 ↓ 3.52x |
| tier2_select_distinct_100 | postgres_18 | pg_fake | 🔴 ↓ 1.04x |
| tier2_union_all_100 | postgres_18 | pg_fake | 🔴 ↓ 4.51x |
| tier2_union_100 | postgres_18 | pg_fake | 🔴 ↓ 4.64x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_tx_local_guc/pg_fake | 60.97 us | +0.46% |
| tier3_tx_local_guc/postgres_18 | 136.08 us | -0.61% |
| tier3_skip_locked_queue_100/pg_fake | 180.44 us | -0.07% |
| tier3_skip_locked_queue_100/postgres_18 | 48.35 us | -0.11% |
| tier3_serializable_read/pg_fake | 19.90 us | +6.64% |
| tier3_serializable_read/postgres_18 | 85.40 us | -0.51% |
| tier3_serializable_write_skew/pg_fake | 129.67 us | +0.22% |
| tier3_serializable_write_skew/postgres_18 | 299.49 us | +0.46% |
| tier3_lateral_latest_100/pg_fake | 1.70 ms | +0.94% |
| tier3_lateral_latest_100/postgres_18 | 605.84 us | +0.98% |
| tier3_migration_lock_2tables/pg_fake | 24.91 us | +0.93% |
| tier3_migration_lock_2tables/postgres_18 | 83.34 us | -0.05% |
| tier3_trigger_insert_update/pg_fake | 65.85 us | -0.33% |
| tier3_trigger_insert_update/postgres_18 | 122.79 us | -0.45% |
| tier3_partial_unique_index_100/pg_fake | 65.36 us | +0.75% |
| tier3_partial_unique_index_100/postgres_18 | 530.39 us | -0.70% |
| tier3_temp_on_commit_drop/pg_fake | 16.56 us | +0.10% |
| tier3_temp_on_commit_drop/postgres_18 | 215.41 us | +3.32% |
| tier3_catalog_regclass_lookup/pg_fake | 56.87 us | -4.44% |
| tier3_catalog_regclass_lookup/postgres_18 | 32.96 us | -9.00% |
| tier3_array_agg_order_filter_100/pg_fake | 204.44 us | +3.95% |
| tier3_array_agg_order_filter_100/postgres_18 | 44.15 us | -2.36% |
| tier3_correlated_unnest_100/pg_fake | 176.17 us | -3.89% |
| tier3_correlated_unnest_100/postgres_18 | 155.93 us | +3.15% |
| tier3_hashed_advisory_lock/pg_fake | 45.53 us | -1.05% |
| tier3_hashed_advisory_lock/postgres_18 | 78.97 us | +0.03% |
| tier3_jsonb_join_group/pg_fake | 658.16 us | -1.74% |
| tier3_jsonb_join_group/postgres_18 | 376.45 us | -0.63% |
| tier3_window_rank_100/pg_fake | 298.02 us | -2.76% |
| tier3_window_rank_100/postgres_18 | 109.30 us | 0.00% |
| tier3_window_offset_100/pg_fake | 575.56 us | +1.13% |
| tier3_window_offset_100/postgres_18 | 125.51 us | +0.26% |
| tier3_window_moving_agg_100/pg_fake | 393.64 us | +0.04% |
| tier3_window_moving_agg_100/postgres_18 | 151.31 us | +0.40% |
| tier3_nested_filtered_view_100/pg_fake | 358.85 us | +1.16% |
| tier3_nested_filtered_view_100/postgres_18 | 35.61 us | -2.31% |
| tier3_tx_history_lookup/1 | 345.34 ns | +1.00% |
| tier3_tx_history_lookup/100 | 351.77 ns | +3.17% |
| tier3_tx_history_lookup/10,000 | 346.17 ns | +0.95% |
| tier3_tx_history_lookup/100,000 | 348.72 ns | +1.83% |
| tier3_mvcc_old_snapshot_read/1 | 568.32 ns | +1.60% |
| tier3_mvcc_old_snapshot_read/100 | 1.83 us | +0.71% |
| tier3_mvcc_old_snapshot_read/10,000 | 666.42 us | -0.29% |
| tier3_same_row_contention/wait_then_rollback | 1.87 ms | -0.39% |
| tier3_cte_update_100/pg_fake | 426.87 us | -0.77% |
| tier3_cte_update_100/postgres_18 | 102.53 us | -0.16% |
| tier3_recursive_series_100/pg_fake | 131.02 us | +0.21% |
| tier3_recursive_series_100/postgres_18 | 73.72 us | +0.56% |
| tier3_recursive_tree_127/pg_fake | 224.28 us | -0.31% |
| tier3_recursive_tree_127/postgres_18 | 120.57 us | -0.92% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_tx_local_guc | postgres_18 | pg_fake | 🟢 ↑ 2.23x |
| tier3_skip_locked_queue_100 | postgres_18 | pg_fake | 🔴 ↓ 3.73x |
| tier3_serializable_read | postgres_18 | pg_fake | 🟢 ↑ 4.29x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 2.31x |
| tier3_lateral_latest_100 | postgres_18 | pg_fake | 🔴 ↓ 2.80x |
| tier3_migration_lock_2tables | postgres_18 | pg_fake | 🟢 ↑ 3.35x |
| tier3_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.86x |
| tier3_partial_unique_index_100 | postgres_18 | pg_fake | 🟢 ↑ 8.11x |
| tier3_temp_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 13.01x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.73x |
| tier3_array_agg_order_filter_100 | postgres_18 | pg_fake | 🔴 ↓ 4.63x |
| tier3_correlated_unnest_100 | postgres_18 | pg_fake | 🔴 ↓ 1.13x |
| tier3_hashed_advisory_lock | postgres_18 | pg_fake | 🟢 ↑ 1.73x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 1.75x |
| tier3_window_rank_100 | postgres_18 | pg_fake | 🔴 ↓ 2.73x |
| tier3_window_offset_100 | postgres_18 | pg_fake | 🔴 ↓ 4.59x |
| tier3_window_moving_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 2.60x |
| tier3_nested_filtered_view_100 | postgres_18 | pg_fake | 🔴 ↓ 10.08x |
| tier3_tx_history_lookup | 1 | 100 | 🔴 ↓ 1.02x |
| tier3_tx_history_lookup | 1 | 10,000 | 🔴 ↓ 1.00x |
| tier3_tx_history_lookup | 1 | 100,000 | 🔴 ↓ 1.01x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.22x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1172.62x |
| tier3_cte_update_100 | postgres_18 | pg_fake | 🔴 ↓ 4.16x |
| tier3_recursive_series_100 | postgres_18 | pg_fake | 🔴 ↓ 1.78x |
| tier3_recursive_tree_127 | postgres_18 | pg_fake | 🔴 ↓ 1.86x |
