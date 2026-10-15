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
| recorded_at | 2026-10-15T22:10:47Z |
| rust | rustc 1.98.1 (48a229cea 2026-09-01) |

## Tier 1: essential operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier1_insert_row/pg_fake | 28.46 us | -25.51% |
| tier1_insert_row/postgres_18 | 36.45 us | -49.83% |
| tier1_update_row/pg_fake | 34.76 us | -19.81% |
| tier1_update_row/postgres_18 | 35.95 us | -60.08% |
| tier1_transaction_insert/pg_fake | 34.13 us | -43.47% |
| tier1_transaction_insert/postgres_18 | 80.29 us | -31.29% |
| tier1_select_100/pg_fake | 26.82 us | +12.35% |
| tier1_select_100/postgres_18 | 59.84 us | +0.80% |
| tier1_select_where_100/pg_fake | 14.98 us | +1.01% |
| tier1_select_where_100/postgres_18 | 35.03 us | +1.18% |
| tier1_select_where_indexed_100/pg_fake | 7.70 us | -10.85% |
| tier1_select_where_indexed_100/postgres_18 | 30.44 us | -11.85% |
| tier1_limit_offset_ordered_100/pg_fake | 65.12 us | +180.91% |
| tier1_limit_offset_ordered_100/postgres_18 | 48.62 us | +14.82% |
| tier1_order_by_100/pg_fake | 37.19 us | +6.52% |
| tier1_order_by_100/postgres_18 | 67.70 us | +2.12% |
| tier1_selective_inner_join/pg_fake | 16.71 us | -2.76% |
| tier1_selective_inner_join/postgres_18 | 40.51 us | +3.40% |
| tier1_many_match_inner_join/pg_fake | 23.50 us | -2.87% |
| tier1_many_match_inner_join/postgres_18 | 66.55 us | +2.06% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier1_insert_row | postgres_18 | pg_fake | 🟢 ↑ 1.28x |
| tier1_update_row | postgres_18 | pg_fake | 🟢 ↑ 1.03x |
| tier1_transaction_insert | postgres_18 | pg_fake | 🟢 ↑ 2.35x |
| tier1_select_100 | postgres_18 | pg_fake | 🟢 ↑ 2.23x |
| tier1_select_where_100 | postgres_18 | pg_fake | 🟢 ↑ 2.34x |
| tier1_select_where_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 3.95x |
| tier1_limit_offset_ordered_100 | postgres_18 | pg_fake | 🔴 ↓ 1.34x |
| tier1_order_by_100 | postgres_18 | pg_fake | 🟢 ↑ 1.82x |
| tier1_selective_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.42x |
| tier1_many_match_inner_join | postgres_18 | pg_fake | 🟢 ↑ 2.83x |

## Tier 2: important operations

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier2_insert_bound_row/pg_fake | 28.53 us | -0.55% |
| tier2_insert_bound_row/postgres_18 | 35.59 us | -1.85% |
| tier2_update_bound_row/pg_fake | 34.91 us | -0.46% |
| tier2_update_bound_row/postgres_18 | 35.58 us | +0.81% |
| tier2_core_snapshot_100/pg_fake | 17.21 us | +2.35% |
| tier2_core_snapshot_1k/pg_fake | 150.62 us | +0.73% |
| tier2_session_settings_roundtrip/pg_fake | 33.02 us | -3.75% |
| tier2_session_settings_roundtrip/postgres_18 | 140.25 us | +3.98% |
| tier2_nested_savepoint_release/pg_fake | 64.67 us | +0.39% |
| tier2_nested_savepoint_release/postgres_18 | 186.89 us | +0.32% |
| tier2_nested_savepoint_rollback/pg_fake | 63.10 us | +0.21% |
| tier2_nested_savepoint_rollback/postgres_18 | 189.16 us | -2.12% |
| tier2_runtime_temporal_100/pg_fake | 298.03 us | +0.27% |
| tier2_runtime_temporal_100/postgres_18 | 117.61 us | -0.31% |
| tier2_runtime_patterns_100/pg_fake | 235.51 us | -0.11% |
| tier2_runtime_patterns_100/postgres_18 | 117.48 us | -0.49% |
| tier2_create_table/pg_fake | 37.09 us | +0.40% |
| tier2_create_table/postgres_18 | 911.92 us | -7.89% |
| tier2_ddl_create_rollback/pg_fake | 29.40 us | -2.28% |
| tier2_ddl_create_rollback/postgres_18 | 1.14 ms | -18.18% |
| tier2_sqlx_migration_chain/pg_fake | 937.43 us | -1.35% |
| tier2_sqlx_migration_chain/postgres_18 | 5.98 ms | -14.19% |
| tier2_alter_table_rewrite_100/pg_fake | 291.03 us | -1.59% |
| tier2_alter_table_rewrite_100/postgres_18 | 342.52 us | -20.47% |
| tier2_insert_row_returning/pg_fake | 45.54 us | -1.07% |
| tier2_insert_row_returning/postgres_18 | 97.87 us | -0.18% |
| tier2_insert_row_with_defaults/pg_fake | 40.16 us | -0.35% |
| tier2_insert_row_with_defaults/postgres_18 | 95.04 us | -0.21% |
| tier2_on_conflict_nothing/pg_fake | 19.81 us | +0.66% |
| tier2_on_conflict_nothing/postgres_18 | 30.94 us | +1.60% |
| tier2_on_conflict_no_conflict/pg_fake | 47.67 us | +0.14% |
| tier2_on_conflict_no_conflict/postgres_18 | 68.16 us | +0.21% |
| tier2_on_conflict_update/pg_fake | 31.66 us | -0.42% |
| tier2_on_conflict_update/postgres_18 | 35.19 us | -1.76% |
| tier2_update_from_row/pg_fake | 52.92 us | -1.21% |
| tier2_update_from_row/postgres_18 | 103.67 us | +2.57% |
| tier2_delete_row/pg_fake | 28.21 us | -0.23% |
| tier2_delete_row/postgres_18 | 105.13 us | -11.82% |
| tier2_sequence_nextval/pg_fake | 20.73 us | +1.54% |
| tier2_sequence_nextval/postgres_18 | 28.72 us | +4.61% |
| tier2_serial_identity_insert/pg_fake | 21.88 us | +7.96% |
| tier2_serial_identity_insert/postgres_18 | 35.47 us | +7.74% |
| tier2_uuid_temporal_select/pg_fake | 27.72 us | +9.03% |
| tier2_uuid_temporal_select/postgres_18 | 33.32 us | +11.04% |
| tier2_offset_datetime_roundtrip/pg_fake | 44.32 us | -0.10% |
| tier2_offset_datetime_roundtrip/postgres_18 | 39.07 us | -28.07% |
| tier2_int8_uuid_array_roundtrip/pg_fake | 47.28 us | -3.57% |
| tier2_int8_uuid_array_roundtrip/postgres_18 | 36.46 us | -27.29% |
| tier2_uuid_any_100/pg_fake | 154.49 us | +167.74% |
| tier2_uuid_any_100/postgres_18 | 87.98 us | +28.61% |
| tier2_array_containment_100/pg_fake | 111.95 us | -3.41% |
| tier2_array_containment_100/postgres_18 | 42.42 us | +2.30% |
| tier2_json_insert_returning/pg_fake | 34.79 us | +0.23% |
| tier2_json_insert_returning/postgres_18 | 93.22 us | -35.68% |
| tier2_jsonb_insert_returning/pg_fake | 40.19 us | +0.13% |
| tier2_jsonb_insert_returning/postgres_18 | 93.33 us | -36.18% |
| tier2_jsonb_extraction/pg_fake | 110.95 us | -2.79% |
| tier2_jsonb_extraction/postgres_18 | 66.40 us | -0.30% |
| tier2_jsonb_containment/pg_fake | 258.50 us | -4.32% |
| tier2_jsonb_containment/postgres_18 | 43.79 us | -0.83% |
| tier2_window_row_number_100/pg_fake | 127.76 us | -1.00% |
| tier2_window_row_number_100/postgres_18 | 83.53 us | -0.95% |
| tier2_ordered_string_agg_100/pg_fake | 242.35 us | +0.07% |
| tier2_ordered_string_agg_100/postgres_18 | 70.10 us | -2.05% |
| tier2_rr_select_for_update/pg_fake | 52.20 us | -0.06% |
| tier2_rr_select_for_update/postgres_18 | 85.42 us | -0.56% |
| tier2_adapter_select_100/core | 13.04 us | -7.33% |
| tier2_adapter_select_100/sqlx | 24.64 us | -2.04% |
| tier2_core_parse_vs_prepare/parse_and_analyze | 19.25 us | -0.08% |
| tier2_core_parse_vs_prepare/prepared_reuse | 529.96 ns | -0.88% |
| tier2_lookup_index_vs_scan/heap_scan/100 | 3.43 us | +0.27% |
| tier2_lookup_index_vs_scan/unique_index/100 | 574.61 ns | -0.31% |
| tier2_lookup_index_vs_scan/heap_scan/10,000 | 304.92 us | +0.46% |
| tier2_lookup_index_vs_scan/unique_index/10,000 | 637.87 ns | -2.10% |
| tier2_lookup_unique_100/pg_fake | 8.30 us | +5.87% |
| tier2_lookup_unique_100/postgres_18 | 35.51 us | +1.13% |
| tier2_lookup_unique_1k/pg_fake | 8.10 us | +6.65% |
| tier2_lookup_unique_1k/postgres_18 | 30.78 us | -1.17% |
| tier2_lookup_nonuniq_100/pg_fake | 8.21 us | +7.41% |
| tier2_lookup_nonuniq_100/postgres_18 | 35.22 us | -0.86% |
| tier2_lookup_nonuniq_1k/pg_fake | 7.28 us | -2.83% |
| tier2_lookup_nonuniq_1k/postgres_18 | 30.18 us | -2.68% |
| tier2_lookup_nonuniq_filter_100/pg_fake | 7.89 us | +6.11% |
| tier2_lookup_nonuniq_filter_100/postgres_18 | 36.27 us | +1.19% |
| tier2_lookup_nonuniq_filter_1k/pg_fake | 6.31 us | -12.28% |
| tier2_lookup_nonuniq_filter_1k/postgres_18 | 31.71 us | +0.14% |
| tier2_lookup_heap_scan_100/pg_fake | 15.17 us | -3.67% |
| tier2_lookup_heap_scan_100/postgres_18 | 35.35 us | +1.72% |
| tier2_lookup_heap_scan_1k/pg_fake | 79.57 us | -7.84% |
| tier2_lookup_heap_scan_1k/postgres_18 | 61.70 us | -35.62% |
| tier2_update_indexed_100/pg_fake | 30.93 us | +1.16% |
| tier2_update_indexed_100/postgres_18 | 37.80 us | -27.62% |
| tier2_update_indexed_1k/pg_fake | 30.95 us | +3.39% |
| tier2_update_indexed_1k/postgres_18 | 32.22 us | -3.00% |
| tier2_delete_indexed_100/pg_fake | 20.70 us | -1.00% |
| tier2_delete_indexed_100/postgres_18 | 36.48 us | +0.89% |
| tier2_delete_indexed_1k/pg_fake | 21.42 us | +3.11% |
| tier2_delete_indexed_1k/postgres_18 | 32.54 us | +1.32% |
| tier2_concurrent_reads/sequential | 11.68 us | +1.68% |
| tier2_concurrent_reads/parallel | 10.72 us | -0.26% |
| tier2_foreign_key_insert/pg_fake | 65.93 us | -0.61% |
| tier2_foreign_key_insert/postgres_18 | 181.03 us | +0.99% |
| tier2_join_3way_100/pg_fake | 7.33 us | -1.20% |
| tier2_join_3way_100/postgres_18 | 37.71 us | +1.46% |
| tier2_join_3way_1k/pg_fake | 7.61 us | +1.32% |
| tier2_join_3way_1k/postgres_18 | 35.97 us | +0.64% |
| tier2_join_3way_filtered_100/pg_fake | 7.41 us | -1.52% |
| tier2_join_3way_filtered_100/postgres_18 | 38.93 us | +2.30% |
| tier2_join_3way_filtered_1k/pg_fake | 7.64 us | +0.77% |
| tier2_join_3way_filtered_1k/postgres_18 | 35.88 us | -0.24% |
| tier2_join_2way_filtered_100/pg_fake | 47.91 us | -0.40% |
| tier2_join_2way_filtered_100/postgres_18 | 37.08 us | +1.71% |
| tier2_join_2way_filtered_1k/pg_fake | 48.23 us | +0.20% |
| tier2_join_2way_filtered_1k/postgres_18 | 34.06 us | +1.90% |
| tier2_join_profile_100/pg_fake | 11.63 us | -0.54% |
| tier2_join_profile_100/postgres_18 | 60.46 us | +0.79% |
| tier2_join_profile_1k/pg_fake | 12.08 us | -0.15% |
| tier2_join_profile_1k/postgres_18 | 43.33 us | +1.10% |
| tier2_join_profile_3k/pg_fake | 12.03 us | -2.59% |
| tier2_join_profile_3k/postgres_18 | 43.58 us | +0.50% |
| tier2_join_members_100/pg_fake | 31.62 us | +1.43% |
| tier2_join_members_100/postgres_18 | 72.47 us | -1.78% |
| tier2_join_members_1k/pg_fake | 33.55 us | -0.82% |
| tier2_join_members_1k/postgres_18 | 59.58 us | -1.37% |
| tier2_join_members_3k/pg_fake | 34.28 us | -2.01% |
| tier2_join_members_3k/postgres_18 | 59.30 us | -1.65% |
| tier2_derived_scalar_subq_100/pg_fake | 346.23 us | -3.81% |
| tier2_derived_scalar_subq_100/postgres_18 | 85.39 us | -0.23% |
| tier2_materialized_cte_100/pg_fake | 109.20 us | -2.66% |
| tier2_materialized_cte_100/postgres_18 | 78.18 us | +0.17% |
| tier2_derived_source_join_100/pg_fake | 139.07 us | -1.84% |
| tier2_derived_source_join_100/postgres_18 | 73.99 us | -0.88% |
| tier2_correlated_exists_100/pg_fake | 68.50 us | -2.16% |
| tier2_correlated_exists_100/postgres_18 | 82.58 us | +21.41% |
| tier2_global_aggregate_100/pg_fake | 34.34 us | -4.38% |
| tier2_global_aggregate_100/postgres_18 | 40.15 us | -0.27% |
| tier2_grouped_aggregate_100/pg_fake | 174.04 us | +0.01% |
| tier2_grouped_aggregate_100/postgres_18 | 49.32 us | +0.76% |
| tier2_select_distinct_100/pg_fake | 49.83 us | -0.29% |
| tier2_select_distinct_100/postgres_18 | 45.38 us | +1.44% |
| tier2_union_all_100/pg_fake | 389.67 us | -0.26% |
| tier2_union_all_100/postgres_18 | 85.41 us | -1.40% |
| tier2_union_100/pg_fake | 419.32 us | -3.27% |
| tier2_union_100/postgres_18 | 85.70 us | +0.08% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier2_insert_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.25x |
| tier2_update_bound_row | postgres_18 | pg_fake | 🟢 ↑ 1.02x |
| tier2_session_settings_roundtrip | postgres_18 | pg_fake | 🟢 ↑ 4.25x |
| tier2_nested_savepoint_release | postgres_18 | pg_fake | 🟢 ↑ 2.89x |
| tier2_nested_savepoint_rollback | postgres_18 | pg_fake | 🟢 ↑ 3.00x |
| tier2_runtime_temporal_100 | postgres_18 | pg_fake | 🔴 ↓ 2.53x |
| tier2_runtime_patterns_100 | postgres_18 | pg_fake | 🔴 ↓ 2.00x |
| tier2_create_table | postgres_18 | pg_fake | 🟢 ↑ 24.59x |
| tier2_ddl_create_rollback | postgres_18 | pg_fake | 🟢 ↑ 38.67x |
| tier2_sqlx_migration_chain | postgres_18 | pg_fake | 🟢 ↑ 6.38x |
| tier2_alter_table_rewrite_100 | postgres_18 | pg_fake | 🟢 ↑ 1.18x |
| tier2_insert_row_returning | postgres_18 | pg_fake | 🟢 ↑ 2.15x |
| tier2_insert_row_with_defaults | postgres_18 | pg_fake | 🟢 ↑ 2.37x |
| tier2_on_conflict_nothing | postgres_18 | pg_fake | 🟢 ↑ 1.56x |
| tier2_on_conflict_no_conflict | postgres_18 | pg_fake | 🟢 ↑ 1.43x |
| tier2_on_conflict_update | postgres_18 | pg_fake | 🟢 ↑ 1.11x |
| tier2_update_from_row | postgres_18 | pg_fake | 🟢 ↑ 1.96x |
| tier2_delete_row | postgres_18 | pg_fake | 🟢 ↑ 3.73x |
| tier2_sequence_nextval | postgres_18 | pg_fake | 🟢 ↑ 1.39x |
| tier2_serial_identity_insert | postgres_18 | pg_fake | 🟢 ↑ 1.62x |
| tier2_uuid_temporal_select | postgres_18 | pg_fake | 🟢 ↑ 1.20x |
| tier2_offset_datetime_roundtrip | postgres_18 | pg_fake | 🔴 ↓ 1.13x |
| tier2_int8_uuid_array_roundtrip | postgres_18 | pg_fake | 🔴 ↓ 1.30x |
| tier2_uuid_any_100 | postgres_18 | pg_fake | 🔴 ↓ 1.76x |
| tier2_array_containment_100 | postgres_18 | pg_fake | 🔴 ↓ 2.64x |
| tier2_json_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.68x |
| tier2_jsonb_insert_returning | postgres_18 | pg_fake | 🟢 ↑ 2.32x |
| tier2_jsonb_extraction | postgres_18 | pg_fake | 🔴 ↓ 1.67x |
| tier2_jsonb_containment | postgres_18 | pg_fake | 🔴 ↓ 5.90x |
| tier2_window_row_number_100 | postgres_18 | pg_fake | 🔴 ↓ 1.53x |
| tier2_ordered_string_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 3.46x |
| tier2_rr_select_for_update | postgres_18 | pg_fake | 🟢 ↑ 1.64x |
| tier2_adapter_select_100 | core | sqlx | 🔴 ↓ 1.89x |
| tier2_core_parse_vs_prepare | parse_and_analyze | prepared_reuse | 🟢 ↑ 36.32x |
| tier2_lookup_index_vs_scan | heap_scan/100 | unique_index/100 | 🟢 ↑ 5.96x |
| tier2_lookup_index_vs_scan | heap_scan/10,000 | unique_index/10,000 | 🟢 ↑ 478.03x |
| tier2_lookup_unique_100 | postgres_18 | pg_fake | 🟢 ↑ 4.28x |
| tier2_lookup_unique_1k | postgres_18 | pg_fake | 🟢 ↑ 3.80x |
| tier2_lookup_nonuniq_100 | postgres_18 | pg_fake | 🟢 ↑ 4.29x |
| tier2_lookup_nonuniq_1k | postgres_18 | pg_fake | 🟢 ↑ 4.15x |
| tier2_lookup_nonuniq_filter_100 | postgres_18 | pg_fake | 🟢 ↑ 4.60x |
| tier2_lookup_nonuniq_filter_1k | postgres_18 | pg_fake | 🟢 ↑ 5.02x |
| tier2_lookup_heap_scan_100 | postgres_18 | pg_fake | 🟢 ↑ 2.33x |
| tier2_lookup_heap_scan_1k | postgres_18 | pg_fake | 🔴 ↓ 1.29x |
| tier2_update_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.22x |
| tier2_update_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.04x |
| tier2_delete_indexed_100 | postgres_18 | pg_fake | 🟢 ↑ 1.76x |
| tier2_delete_indexed_1k | postgres_18 | pg_fake | 🟢 ↑ 1.52x |
| tier2_concurrent_reads | sequential | parallel | 🟢 ↑ 1.09x |
| tier2_foreign_key_insert | postgres_18 | pg_fake | 🟢 ↑ 2.75x |
| tier2_join_3way_100 | postgres_18 | pg_fake | 🟢 ↑ 5.14x |
| tier2_join_3way_1k | postgres_18 | pg_fake | 🟢 ↑ 4.73x |
| tier2_join_3way_filtered_100 | postgres_18 | pg_fake | 🟢 ↑ 5.25x |
| tier2_join_3way_filtered_1k | postgres_18 | pg_fake | 🟢 ↑ 4.69x |
| tier2_join_2way_filtered_100 | postgres_18 | pg_fake | 🔴 ↓ 1.29x |
| tier2_join_2way_filtered_1k | postgres_18 | pg_fake | 🔴 ↓ 1.42x |
| tier2_join_profile_100 | postgres_18 | pg_fake | 🟢 ↑ 5.20x |
| tier2_join_profile_1k | postgres_18 | pg_fake | 🟢 ↑ 3.59x |
| tier2_join_profile_3k | postgres_18 | pg_fake | 🟢 ↑ 3.62x |
| tier2_join_members_100 | postgres_18 | pg_fake | 🟢 ↑ 2.29x |
| tier2_join_members_1k | postgres_18 | pg_fake | 🟢 ↑ 1.78x |
| tier2_join_members_3k | postgres_18 | pg_fake | 🟢 ↑ 1.73x |
| tier2_derived_scalar_subq_100 | postgres_18 | pg_fake | 🔴 ↓ 4.05x |
| tier2_materialized_cte_100 | postgres_18 | pg_fake | 🔴 ↓ 1.40x |
| tier2_derived_source_join_100 | postgres_18 | pg_fake | 🔴 ↓ 1.88x |
| tier2_correlated_exists_100 | postgres_18 | pg_fake | 🟢 ↑ 1.21x |
| tier2_global_aggregate_100 | postgres_18 | pg_fake | 🟢 ↑ 1.17x |
| tier2_grouped_aggregate_100 | postgres_18 | pg_fake | 🔴 ↓ 3.53x |
| tier2_select_distinct_100 | postgres_18 | pg_fake | 🔴 ↓ 1.10x |
| tier2_union_all_100 | postgres_18 | pg_fake | 🔴 ↓ 4.56x |
| tier2_union_100 | postgres_18 | pg_fake | 🔴 ↓ 4.89x |

## Tier 3: rare operations and diagnostics

### Benchmarks

| Benchmark | Average | Change vs previous |
| --- | ---: | ---: |
| tier3_tx_local_guc/pg_fake | 60.69 us | -2.30% |
| tier3_tx_local_guc/postgres_18 | 136.91 us | +0.07% |
| tier3_skip_locked_queue_100/pg_fake | 180.57 us | -0.56% |
| tier3_skip_locked_queue_100/postgres_18 | 48.40 us | +0.22% |
| tier3_serializable_read/pg_fake | 18.67 us | +2.64% |
| tier3_serializable_read/postgres_18 | 85.84 us | +0.08% |
| tier3_serializable_write_skew/pg_fake | 129.39 us | +0.13% |
| tier3_serializable_write_skew/postgres_18 | 298.10 us | -1.26% |
| tier3_lateral_latest_100/pg_fake | 1.68 ms | -0.42% |
| tier3_lateral_latest_100/postgres_18 | 599.97 us | +420.87% |
| tier3_migration_lock_2tables/pg_fake | 24.69 us | -0.62% |
| tier3_migration_lock_2tables/postgres_18 | 83.38 us | -0.71% |
| tier3_trigger_insert_update/pg_fake | 66.07 us | +0.07% |
| tier3_trigger_insert_update/postgres_18 | 123.35 us | +0.83% |
| tier3_partial_unique_index_100/pg_fake | 64.87 us | -1.97% |
| tier3_partial_unique_index_100/postgres_18 | 534.15 us | -2.03% |
| tier3_temp_on_commit_drop/pg_fake | 16.54 us | +0.46% |
| tier3_temp_on_commit_drop/postgres_18 | 208.49 us | -16.46% |
| tier3_catalog_regclass_lookup/pg_fake | 59.52 us | +3.82% |
| tier3_catalog_regclass_lookup/postgres_18 | 36.22 us | +7.95% |
| tier3_array_agg_order_filter_100/pg_fake | 196.66 us | -2.15% |
| tier3_array_agg_order_filter_100/postgres_18 | 45.22 us | +2.27% |
| tier3_correlated_unnest_100/pg_fake | 183.29 us | -2.48% |
| tier3_correlated_unnest_100/postgres_18 | 151.16 us | +0.75% |
| tier3_hashed_advisory_lock/pg_fake | 46.02 us | +3.21% |
| tier3_hashed_advisory_lock/postgres_18 | 78.94 us | +2.27% |
| tier3_jsonb_join_group/pg_fake | 669.84 us | +1.47% |
| tier3_jsonb_join_group/postgres_18 | 378.86 us | +5.36% |
| tier3_window_rank_100/pg_fake | 306.47 us | +1.59% |
| tier3_window_rank_100/postgres_18 | 109.30 us | -0.42% |
| tier3_window_offset_100/pg_fake | 569.11 us | +0.40% |
| tier3_window_offset_100/postgres_18 | 125.19 us | -0.75% |
| tier3_window_moving_agg_100/pg_fake | 393.47 us | -0.37% |
| tier3_window_moving_agg_100/postgres_18 | 150.70 us | -0.39% |
| tier3_nested_filtered_view_100/pg_fake | 354.72 us | -1.19% |
| tier3_nested_filtered_view_100/postgres_18 | 36.46 us | +2.35% |
| tier3_tx_history_lookup/1 | 341.93 ns | +0.19% |
| tier3_tx_history_lookup/100 | 340.97 ns | -1.41% |
| tier3_tx_history_lookup/10,000 | 342.93 ns | -0.87% |
| tier3_tx_history_lookup/100,000 | 342.46 ns | -1.42% |
| tier3_mvcc_old_snapshot_read/1 | 559.39 ns | -1.12% |
| tier3_mvcc_old_snapshot_read/100 | 1.82 us | -0.03% |
| tier3_mvcc_old_snapshot_read/10,000 | 668.33 us | -0.15% |
| tier3_same_row_contention/wait_then_rollback | 1.88 ms | -3.69% |
| tier3_cte_update_100/pg_fake | 430.20 us | -1.53% |
| tier3_cte_update_100/postgres_18 | 102.69 us | -6.61% |
| tier3_recursive_series_100/pg_fake | 130.75 us | -0.94% |
| tier3_recursive_series_100/postgres_18 | 73.30 us | +0.24% |
| tier3_recursive_tree_127/pg_fake | 224.97 us | +0.03% |
| tier3_recursive_tree_127/postgres_18 | 121.69 us | +1.27% |

### Comparisons

| Benchmark | Baseline | Candidate | Relative |
| --- | --- | --- | ---: |
| tier3_tx_local_guc | postgres_18 | pg_fake | 🟢 ↑ 2.26x |
| tier3_skip_locked_queue_100 | postgres_18 | pg_fake | 🔴 ↓ 3.73x |
| tier3_serializable_read | postgres_18 | pg_fake | 🟢 ↑ 4.60x |
| tier3_serializable_write_skew | postgres_18 | pg_fake | 🟢 ↑ 2.30x |
| tier3_lateral_latest_100 | postgres_18 | pg_fake | 🔴 ↓ 2.80x |
| tier3_migration_lock_2tables | postgres_18 | pg_fake | 🟢 ↑ 3.38x |
| tier3_trigger_insert_update | postgres_18 | pg_fake | 🟢 ↑ 1.87x |
| tier3_partial_unique_index_100 | postgres_18 | pg_fake | 🟢 ↑ 8.23x |
| tier3_temp_on_commit_drop | postgres_18 | pg_fake | 🟢 ↑ 12.61x |
| tier3_catalog_regclass_lookup | postgres_18 | pg_fake | 🔴 ↓ 1.64x |
| tier3_array_agg_order_filter_100 | postgres_18 | pg_fake | 🔴 ↓ 4.35x |
| tier3_correlated_unnest_100 | postgres_18 | pg_fake | 🔴 ↓ 1.21x |
| tier3_hashed_advisory_lock | postgres_18 | pg_fake | 🟢 ↑ 1.72x |
| tier3_jsonb_join_group | postgres_18 | pg_fake | 🔴 ↓ 1.77x |
| tier3_window_rank_100 | postgres_18 | pg_fake | 🔴 ↓ 2.80x |
| tier3_window_offset_100 | postgres_18 | pg_fake | 🔴 ↓ 4.55x |
| tier3_window_moving_agg_100 | postgres_18 | pg_fake | 🔴 ↓ 2.61x |
| tier3_nested_filtered_view_100 | postgres_18 | pg_fake | 🔴 ↓ 9.73x |
| tier3_tx_history_lookup | 1 | 100 | 🟢 ↑ 1.00x |
| tier3_tx_history_lookup | 1 | 10,000 | 🔴 ↓ 1.00x |
| tier3_tx_history_lookup | 1 | 100,000 | 🔴 ↓ 1.00x |
| tier3_mvcc_old_snapshot_read | 1 | 100 | 🔴 ↓ 3.25x |
| tier3_mvcc_old_snapshot_read | 1 | 10,000 | 🔴 ↓ 1194.75x |
| tier3_cte_update_100 | postgres_18 | pg_fake | 🔴 ↓ 4.19x |
| tier3_recursive_series_100 | postgres_18 | pg_fake | 🔴 ↓ 1.78x |
| tier3_recursive_tree_127 | postgres_18 | pg_fake | 🔴 ↓ 1.85x |
