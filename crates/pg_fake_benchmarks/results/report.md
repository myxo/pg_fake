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
| recorded_at | 2026-10-15T20:53:52Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 38.20 us | -0.38% |
| tier1_insert_row/postgres_18 | 72.65 us | -2.99% |
| tier1_update_row/pg_fake | 43.34 us | -2.98% |
| tier1_update_row/postgres_18 | 90.04 us | -6.75% |
| tier1_transaction_insert/pg_fake | 60.38 us | +45.99% |
| tier1_transaction_insert/postgres_18 | 116.85 us | +0.62% |
| tier1_select_100/pg_fake | 23.87 us | -3.27% |
| tier1_select_100/postgres_18 | 59.36 us | +1.00% |
| tier1_select_where_100/pg_fake | 14.83 us | -1.41% |
| tier1_select_where_100/postgres_18 | 34.62 us | +1.13% |
| tier1_select_where_indexed_100/pg_fake | 8.64 us | +2.26% |
| tier1_select_where_indexed_100/postgres_18 | 34.54 us | +0.90% |
| tier1_limit_offset_ordered_100/pg_fake | 23.18 us | -0.90% |
| tier1_limit_offset_ordered_100/postgres_18 | 42.35 us | +0.95% |
| tier1_order_by_100/pg_fake | 34.91 us | -1.41% |
| tier1_order_by_100/postgres_18 | 66.29 us | -1.03% |
| tier1_selective_inner_join/pg_fake | 17.19 us | -5.28% |
| tier1_selective_inner_join/postgres_18 | 39.18 us | -2.61% |
| tier1_many_match_inner_join/pg_fake | 24.19 us | -3.44% |
| tier1_many_match_inner_join/postgres_18 | 65.21 us | -3.75% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.90x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 2.08x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 1.94x |
| tier1_select_100 | postgres_18 | pg_fake | 🟢 ↑ 2.49x |
| tier1_select_where_100 | postgres_18 | pg_fake | 🟢 ↑ 2.33x |
| tier1_select_where_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 4.00x |
| tier1_limit_offset_ordered_100 | postgres_18 | pg_fake | 🟢 ↑ 1.83x |
| tier1_order_by_100 | postgres_18 | pg_fake | 🟢 ↑ 1.90x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.28x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.70x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_insert_bound_row/pg_fake | 28.69 us | -0.29% |
| tier2_insert_bound_row/postgres_18 | 36.26 us | +2.28% |
| tier2_update_bound_row/pg_fake | 35.07 us | -1.72% |
| tier2_update_bound_row/postgres_18 | 35.30 us | -0.92% |
| tier2_core_snapshot_100/pg_fake | 16.81 us | +4.87% |
| tier2_core_snapshot_1k/pg_fake | 149.53 us | +1.62% |
| tier2_session_settings_roundtrip/pg_fake | 34.30 us | -3.67% |
| tier2_session_settings_roundtrip/postgres_18 | 134.88 us | -12.61% |
| tier2_nested_savepoint_release/pg_fake | 64.42 us | -2.40% |
| tier2_nested_savepoint_release/postgres_18 | 186.30 us | +1.48% |
| tier2_nested_savepoint_rollback/pg_fake | 62.97 us | -19.46% |
| tier2_nested_savepoint_rollback/postgres_18 | 193.26 us | -22.70% |
| tier2_runtime_temporal_100/pg_fake | 297.23 us | -3.45% |
| tier2_runtime_temporal_100/postgres_18 | 117.98 us | -0.93% |
| tier2_runtime_patterns_100/pg_fake | 235.78 us | +0.84% |
| tier2_runtime_patterns_100/postgres_18 | 118.05 us | +0.39% |
| tier2_create_table/pg_fake | 36.94 us | -1.28% |
| tier2_create_table/postgres_18 | 990.08 us | +3.65% |
| tier2_ddl_create_rollback/pg_fake | 30.09 us | +1.39% |
| tier2_ddl_create_rollback/postgres_18 | 1.39 ms | +5.14% |
| tier2_sqlx_migration_chain/pg_fake | 950.31 us | -3.08% |
| tier2_sqlx_migration_chain/postgres_18 | 6.97 ms | +4.70% |
| tier2_alter_table_rewrite_100/pg_fake | 295.74 us | -8.39% |
| tier2_alter_table_rewrite_100/postgres_18 | 430.70 us | +15.43% |
| tier2_insert_row_returning/pg_fake | 46.03 us | +0.27% |
| tier2_insert_row_returning/postgres_18 | 98.04 us | -2.43% |
| tier2_insert_row_with_defaults/pg_fake | 40.30 us | -1.38% |
| tier2_insert_row_with_defaults/postgres_18 | 95.24 us | +0.85% |
| tier2_on_conflict_nothing/pg_fake | 19.68 us | -1.29% |
| tier2_on_conflict_nothing/postgres_18 | 30.45 us | -1.88% |
| tier2_on_conflict_no_conflict/pg_fake | 47.60 us | +0.86% |
| tier2_on_conflict_no_conflict/postgres_18 | 68.02 us | +0.41% |
| tier2_on_conflict_update/pg_fake | 31.79 us | +0.41% |
| tier2_on_conflict_update/postgres_18 | 35.82 us | +1.91% |
| tier2_update_from_row/pg_fake | 53.57 us | -7.70% |
| tier2_update_from_row/postgres_18 | 101.08 us | -2.25% |
| tier2_delete_row/pg_fake | 28.28 us | +0.62% |
| tier2_delete_row/postgres_18 | 119.22 us | +2.58% |
| tier2_sequence_nextval/pg_fake | 20.42 us | +1.11% |
| tier2_sequence_nextval/postgres_18 | 27.46 us | -3.26% |
| tier2_serial_identity_insert/pg_fake | 20.26 us | +1.41% |
| tier2_serial_identity_insert/postgres_18 | 32.92 us | -0.87% |
| tier2_uuid_temporal_select/pg_fake | 25.42 us | +3.08% |
| tier2_uuid_temporal_select/postgres_18 | 30.01 us | -3.31% |
| tier2_offset_datetime_roundtrip/pg_fake | 44.36 us | +6.22% |
| tier2_offset_datetime_roundtrip/postgres_18 | 54.32 us | +47.99% |
| tier2_int8_uuid_array_roundtrip/pg_fake | 49.04 us | +2.15% |
| tier2_int8_uuid_array_roundtrip/postgres_18 | 50.14 us | +35.95% |
| tier2_uuid_any_100/pg_fake | 57.70 us | -9.37% |
| tier2_uuid_any_100/postgres_18 | 68.41 us | +11.39% |
| tier2_array_containment_100/pg_fake | 115.90 us | +0.21% |
| tier2_array_containment_100/postgres_18 | 41.47 us | +0.63% |
| tier2_json_insert_returning/pg_fake | 34.71 us | +0.06% |
| tier2_json_insert_returning/postgres_18 | 144.93 us | -0.03% |
| tier2_jsonb_insert_returning/pg_fake | 40.14 us | -0.19% |
| tier2_jsonb_insert_returning/postgres_18 | 146.24 us | -7.07% |
| tier2_jsonb_extraction/pg_fake | 114.13 us | -1.77% |
| tier2_jsonb_extraction/postgres_18 | 66.60 us | +0.90% |
| tier2_jsonb_containment/pg_fake | 270.18 us | -3.73% |
| tier2_jsonb_containment/postgres_18 | 44.16 us | -0.52% |
| tier2_window_row_number_100/pg_fake | 129.06 us | -2.63% |
| tier2_window_row_number_100/postgres_18 | 84.33 us | -1.21% |
| tier2_ordered_string_agg_100/pg_fake | 242.19 us | -0.11% |
| tier2_ordered_string_agg_100/postgres_18 | 71.57 us | -0.59% |
| tier2_rr_select_for_update/pg_fake | 52.23 us | -0.30% |
| tier2_rr_select_for_update/postgres_18 | 85.90 us | +1.32% |
| tier2_adapter_select_100/core | 14.07 us | +1.68% |
| tier2_adapter_select_100/sqlx | 25.15 us | -3.10% |
| tier2_core_parse_vs_prepare/parse_and_analyze | 19.26 us | +1.24% |
| tier2_core_parse_vs_prepare/prepared_reuse | 534.67 ns | +3.40% |
| tier2_lookup_index_vs_scan/heap_scan/100 | 3.42 us | +2.88% |
| tier2_lookup_index_vs_scan/unique_index/100 | 576.39 ns | +2.28% |
| tier2_lookup_index_vs_scan/heap_scan/10,000 | 303.52 us | +3.42% |
| tier2_lookup_index_vs_scan/unique_index/10,000 | 651.58 ns | +3.75% |
| tier2_lookup_unique_100/pg_fake | 7.84 us | -5.52% |
| tier2_lookup_unique_100/postgres_18 | 35.11 us | -8.04% |
| tier2_lookup_unique_1k/pg_fake | 7.60 us | -14.42% |
| tier2_lookup_unique_1k/postgres_18 | 31.15 us | -5.23% |
| tier2_lookup_nonuniq_100/pg_fake | 7.64 us | -4.69% |
| tier2_lookup_nonuniq_100/postgres_18 | 35.52 us | -30.88% |
| tier2_lookup_nonuniq_1k/pg_fake | 7.49 us | -17.49% |
| tier2_lookup_nonuniq_1k/postgres_18 | 31.01 us | -8.23% |
| tier2_lookup_nonuniq_filter_100/pg_fake | 7.43 us | -15.79% |
| tier2_lookup_nonuniq_filter_100/postgres_18 | 35.84 us | -9.35% |
| tier2_lookup_nonuniq_filter_1k/pg_fake | 7.20 us | -17.76% |
| tier2_lookup_nonuniq_filter_1k/postgres_18 | 31.67 us | -9.03% |
| tier2_lookup_heap_scan_100/pg_fake | 15.75 us | -9.46% |
| tier2_lookup_heap_scan_100/postgres_18 | 34.75 us | -9.92% |
| tier2_lookup_heap_scan_1k/pg_fake | 86.34 us | -1.00% |
| tier2_lookup_heap_scan_1k/postgres_18 | 95.83 us | +46.17% |
| tier2_update_indexed_100/pg_fake | 30.58 us | -5.66% |
| tier2_update_indexed_100/postgres_18 | 52.22 us | +28.09% |
| tier2_update_indexed_1k/pg_fake | 29.93 us | -13.50% |
| tier2_update_indexed_1k/postgres_18 | 33.21 us | -6.90% |
| tier2_delete_indexed_100/pg_fake | 20.91 us | -5.35% |
| tier2_delete_indexed_100/postgres_18 | 36.16 us | -5.96% |
| tier2_delete_indexed_1k/pg_fake | 20.78 us | -6.71% |
| tier2_delete_indexed_1k/postgres_18 | 32.11 us | -6.53% |
| tier2_concurrent_reads/sequential | 11.49 us | -17.19% |
| tier2_concurrent_reads/parallel | 10.75 us | -6.52% |
| tier2_foreign_key_insert/pg_fake | 66.33 us | -6.62% |
| tier2_foreign_key_insert/postgres_18 | 179.26 us | -5.14% |
| tier2_join_3way_100/pg_fake | 7.42 us | -83.21% |
| tier2_join_3way_100/postgres_18 | 37.17 us | +10.59% |
| tier2_join_3way_1k/pg_fake | 7.51 us | -82.11% |
| tier2_join_3way_1k/postgres_18 | 35.74 us | -26.76% |
| tier2_join_3way_filtered_100/pg_fake | 7.53 us | -83.95% |
| tier2_join_3way_filtered_100/postgres_18 | 38.05 us | +18.24% |
| tier2_join_3way_filtered_1k/pg_fake | 7.59 us | -85.18% |
| tier2_join_3way_filtered_1k/postgres_18 | 35.97 us | -23.11% |
| tier2_join_2way_filtered_100/pg_fake | 48.10 us | +5.17% |
| tier2_join_2way_filtered_100/postgres_18 | 36.46 us | +8.50% |
| tier2_join_2way_filtered_1k/pg_fake | 48.13 us | +3.90% |
| tier2_join_2way_filtered_1k/postgres_18 | 33.42 us | -28.20% |
| tier2_join_profile_100/pg_fake | 11.69 us | N/A |
| tier2_join_profile_100/postgres_18 | 59.99 us | N/A |
| tier2_join_profile_1k/pg_fake | 12.10 us | N/A |
| tier2_join_profile_1k/postgres_18 | 42.86 us | N/A |
| tier2_join_profile_3k/pg_fake | 12.35 us | N/A |
| tier2_join_profile_3k/postgres_18 | 43.36 us | N/A |
| tier2_join_members_100/pg_fake | 31.18 us | N/A |
| tier2_join_members_100/postgres_18 | 73.78 us | N/A |
| tier2_join_members_1k/pg_fake | 33.83 us | N/A |
| tier2_join_members_1k/postgres_18 | 60.41 us | N/A |
| tier2_join_members_3k/pg_fake | 34.98 us | N/A |
| tier2_join_members_3k/postgres_18 | 60.29 us | N/A |
| tier2_derived_scalar_subq_100/pg_fake | 359.95 us | +0.11% |
| tier2_derived_scalar_subq_100/postgres_18 | 85.58 us | -32.21% |
| tier2_materialized_cte_100/pg_fake | 112.19 us | -5.04% |
| tier2_materialized_cte_100/postgres_18 | 78.05 us | -21.21% |
| tier2_derived_source_join_100/pg_fake | 141.69 us | -1.04% |
| tier2_derived_source_join_100/postgres_18 | 74.65 us | +0.91% |
| tier2_correlated_exists_100/pg_fake | 70.01 us | -3.55% |
| tier2_correlated_exists_100/postgres_18 | 68.02 us | -43.40% |
| tier2_global_aggregate_100/pg_fake | 35.91 us | +0.69% |
| tier2_global_aggregate_100/postgres_18 | 40.26 us | -1.62% |
| tier2_grouped_aggregate_100/pg_fake | 174.03 us | -4.51% |
| tier2_grouped_aggregate_100/postgres_18 | 48.95 us | -4.69% |
| tier2_select_distinct_100/pg_fake | 49.97 us | -0.02% |
| tier2_select_distinct_100/postgres_18 | 44.73 us | -2.02% |
| tier2_union_all_100/pg_fake | 390.67 us | -0.05% |
| tier2_union_all_100/postgres_18 | 86.63 us | -1.15% |
| tier2_union_100/pg_fake | 433.50 us | +1.79% |
| tier2_union_100/postgres_18 | 85.63 us | -1.08% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_insert_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.26x |
| tier2_update_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.01x |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 3.93x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 2.89x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 3.07x |
| tier2_runtime_temporal_100 | postgres_18 | pg_fake | 🔴 ↓ 2.52x |
| tier2_runtime_patterns_100 | postgres_18 | pg_fake | 🔴 ↓ 2.00x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 26.80x |
| tier2_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 46.19x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 7.34x |
| tier2_alter_table_rewrite_100 | postgres_18 | pg_fake | 🟢 ↑ 1.46x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 2.13x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.36x |
| tier2_on_conflict_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.55x |
| tier2_on_conflict_no_conflict | postgres_18 | pg_fake | 🟢 ↑ 1.43x |
| tier2_on_conflict_update | postgres_18 | pg_fake | 🟢 ↑ 1.13x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 1.89x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 4.22x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.34x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.62x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.18x |
| tier2_offset_datetime_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 1.22x |
| tier2_int8_uuid_array_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 1.02x |
| tier2_uuid_any_100 | postgres_18 | pg_fake | 🟢 ↑ 1.19x |
| tier2_array_containment_100 | postgres_18 | pg_fake | 🔴 ↓ 2.79x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 4.18x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 3.64x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.71x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 6.12x |
| tier2_window_row_number_100 | postgres_18 | pg_fake | 🔴 ↓ 1.53x |
| tier2_ordered_string_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 3.38x |
| tier2_rr_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.64x |
| tier2_adapter_select_100 | core | sqlx | 🔴 ↓ 1.79x |
| tier2_core_parse_vs_prepare | parse_and_analyze | prepared_reuse | 🟢 ↑ 36.03x |
| tier2_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.93x |
| tier2_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 465.81x |
| tier2_lookup_unique_100 | postgres_18 | pg_fake | 🟢 ↑ 4.48x |
| tier2_lookup_unique_1k | postgres_18 | pg_fake | 🟢 ↑ 4.10x |
| tier2_lookup_nonuniq_100 | postgres_18 | pg_fake | 🟢 ↑ 4.65x |
| tier2_lookup_nonuniq_1k | postgres_18 | pg_fake | 🟢 ↑ 4.14x |
| tier2_lookup_nonuniq_filter_100 | postgres_18 | pg_fake | 🟢 ↑ 4.82x |
| tier2_lookup_nonuniq_filter_1k | postgres_18 | pg_fake | 🟢 ↑ 4.40x |
| tier2_lookup_heap_scan_100 | postgres_18 | pg_fake | 🟢 ↑ 2.21x |
| tier2_lookup_heap_scan_1k | postgres_18 | pg_fake | 🟢 ↑ 1.11x |
| tier2_update_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.71x |
| tier2_update_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.11x |
| tier2_delete_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.73x |
| tier2_delete_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.55x |
| tier2_concurrent_reads | sequential | parallel | 🟢 ↑ 1.07x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.70x |
| tier2_join_3way_100 | postgres_18 | pg_fake | 🟢 ↑ 5.01x |
| tier2_join_3way_1k | postgres_18 | pg_fake | 🟢 ↑ 4.76x |
| tier2_join_3way_filtered_100 | postgres_18 | pg_fake | 🟢 ↑ 5.06x |
| tier2_join_3way_filtered_1k | postgres_18 | pg_fake | 🟢 ↑ 4.74x |
| tier2_join_2way_filtered_100 | postgres_18 | pg_fake | 🔴 ↓ 1.32x |
| tier2_join_2way_filtered_1k | postgres_18 | pg_fake | 🔴 ↓ 1.44x |
| tier2_join_profile_100 | postgres_18 | pg_fake | 🟢 ↑ 5.13x |
| tier2_join_profile_1k | postgres_18 | pg_fake | 🟢 ↑ 3.54x |
| tier2_join_profile_3k | postgres_18 | pg_fake | 🟢 ↑ 3.51x |
| tier2_join_members_100 | postgres_18 | pg_fake | 🟢 ↑ 2.37x |
| tier2_join_members_1k | postgres_18 | pg_fake | 🟢 ↑ 1.79x |
| tier2_join_members_3k | postgres_18 | pg_fake | 🟢 ↑ 1.72x |
| tier2_derived_scalar_subq_100 | postgres_18 | pg_fake | 🔴 ↓ 4.21x |
| tier2_materialized_cte_100 | postgres_18 | pg_fake | 🔴 ↓ 1.44x |
| tier2_derived_source_join_100 | postgres_18 | pg_fake | 🔴 ↓ 1.90x |
| tier2_correlated_exists_100 | postgres_18 | pg_fake | 🔴 ↓ 1.03x |
| tier2_global_aggregate_100 | postgres_18 | pg_fake | 🟢 ↑ 1.12x |
| tier2_grouped_aggregate_100 | postgres_18 | pg_fake | 🔴 ↓ 3.56x |
| tier2_select_distinct_100 | postgres_18 | pg_fake | 🔴 ↓ 1.12x |
| tier2_union_all_100 | postgres_18 | pg_fake | 🔴 ↓ 4.51x |
| tier2_union_100 | postgres_18 | pg_fake | 🔴 ↓ 5.06x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_tx_local_guc/pg_fake | 62.12 us | +0.50% |
| tier3_tx_local_guc/postgres_18 | 136.82 us | -1.47% |
| tier3_skip_locked_queue_100/pg_fake | 181.58 us | -1.43% |
| tier3_skip_locked_queue_100/postgres_18 | 48.30 us | -27.31% |
| tier3_serializable_read/pg_fake | 18.19 us | -40.85% |
| tier3_serializable_read/postgres_18 | 85.77 us | -8.92% |
| tier3_serializable_write_skew/pg_fake | 129.23 us | +12.10% |
| tier3_serializable_write_skew/postgres_18 | 301.92 us | -6.56% |
| tier3_lateral_latest_100/pg_fake | 1.69 ms | +0.37% |
| tier3_lateral_latest_100/postgres_18 | 115.19 us | -86.65% |
| tier3_migration_lock_2tables/pg_fake | 24.84 us | -72.74% |
| tier3_migration_lock_2tables/postgres_18 | 83.98 us | -14.89% |
| tier3_trigger_insert_update/pg_fake | 66.02 us | +0.46% |
| tier3_trigger_insert_update/postgres_18 | 122.34 us | -0.98% |
| tier3_partial_unique_index_100/pg_fake | 66.18 us | -3.80% |
| tier3_partial_unique_index_100/postgres_18 | 545.23 us | +2.36% |
| tier3_temp_on_commit_drop/pg_fake | 16.47 us | -5.66% |
| tier3_temp_on_commit_drop/postgres_18 | 249.56 us | -33.49% |
| tier3_catalog_regclass_lookup/pg_fake | 57.33 us | -2.57% |
| tier3_catalog_regclass_lookup/postgres_18 | 33.55 us | -0.11% |
| tier3_array_agg_order_filter_100/pg_fake | 200.98 us | -0.72% |
| tier3_array_agg_order_filter_100/postgres_18 | 44.21 us | +0.57% |
| tier3_correlated_unnest_100/pg_fake | 187.96 us | +1.16% |
| tier3_correlated_unnest_100/postgres_18 | 150.03 us | -0.16% |
| tier3_hashed_advisory_lock/pg_fake | 44.59 us | -0.81% |
| tier3_hashed_advisory_lock/postgres_18 | 77.19 us | -2.02% |
| tier3_jsonb_join_group/pg_fake | 660.14 us | +0.10% |
| tier3_jsonb_join_group/postgres_18 | 359.59 us | +5.73% |
| tier3_window_rank_100/pg_fake | 301.68 us | -1.29% |
| tier3_window_rank_100/postgres_18 | 109.76 us | +0.47% |
| tier3_window_offset_100/pg_fake | 566.84 us | -7.20% |
| tier3_window_offset_100/postgres_18 | 126.14 us | -3.15% |
| tier3_window_moving_agg_100/pg_fake | 394.94 us | -3.11% |
| tier3_window_moving_agg_100/postgres_18 | 151.29 us | +0.19% |
| tier3_nested_filtered_view_100/pg_fake | 359.00 us | -0.37% |
| tier3_nested_filtered_view_100/postgres_18 | 35.62 us | -0.46% |
| tier3_tx_history_lookup/1 | 341.26 ns | +1.61% |
| tier3_tx_history_lookup/100 | 345.83 ns | +3.27% |
| tier3_tx_history_lookup/10,000 | 345.95 ns | +2.73% |
| tier3_tx_history_lookup/100,000 | 347.39 ns | +2.87% |
| tier3_mvcc_old_snapshot_read/1 | 565.74 ns | +1.75% |
| tier3_mvcc_old_snapshot_read/100 | 1.82 us | +0.62% |
| tier3_mvcc_old_snapshot_read/10,000 | 669.35 us | -0.41% |
| tier3_same_row_contention/wait_then_rollback | 1.95 ms | +6.17% |
| tier3_cte_update_100/pg_fake | 436.90 us | -0.09% |
| tier3_cte_update_100/postgres_18 | 109.95 us | +4.65% |
| tier3_recursive_series_100/pg_fake | 131.98 us | -1.41% |
| tier3_recursive_series_100/postgres_18 | 73.13 us | -2.26% |
| tier3_recursive_tree_127/pg_fake | 224.90 us | -2.60% |
| tier3_recursive_tree_127/postgres_18 | 120.17 us | -3.57% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_tx_local_guc | postgres_18 | pg_fake | 🟢 ↑ 2.20x |
| tier3_skip_locked_queue_100 | postgres_18 | pg_fake | 🔴 ↓ 3.76x |
| tier3_serializable_read | postgres_18 | pg_fake | 🟢 ↑ 4.72x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 2.34x |
| tier3_lateral_latest_100 | postgres_18 | pg_fake | 🔴 ↓ 14.65x |
| tier3_migration_lock_2tables | postgres_18 | pg_fake | 🟢 ↑ 3.38x |
| tier3_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.85x |
| tier3_partial_unique_index_100 | postgres_18 | pg_fake | 🟢 ↑ 8.24x |
| tier3_temp_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 15.16x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.71x |
| tier3_array_agg_order_filter_100 | postgres_18 | pg_fake | 🔴 ↓ 4.55x |
| tier3_correlated_unnest_100 | postgres_18 | pg_fake | 🔴 ↓ 1.25x |
| tier3_hashed_advisory_lock | postgres_18 | pg_fake | 🟢 ↑ 1.73x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 1.84x |
| tier3_window_rank_100 | postgres_18 | pg_fake | 🔴 ↓ 2.75x |
| tier3_window_offset_100 | postgres_18 | pg_fake | 🔴 ↓ 4.49x |
| tier3_window_moving_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 2.61x |
| tier3_nested_filtered_view_100 | postgres_18 | pg_fake | 🔴 ↓ 10.08x |
| tier3_tx_history_lookup | 1 | 100 | 🔴 ↓ 1.01x |
| tier3_tx_history_lookup | 1 | 10,000 | 🔴 ↓ 1.01x |
| tier3_tx_history_lookup | 1 | 100,000 | 🔴 ↓ 1.02x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.22x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1183.13x |
| tier3_cte_update_100 | postgres_18 | pg_fake | 🔴 ↓ 3.97x |
| tier3_recursive_series_100 | postgres_18 | pg_fake | 🔴 ↓ 1.80x |
| tier3_recursive_tree_127 | postgres_18 | pg_fake | 🔴 ↓ 1.87x |
