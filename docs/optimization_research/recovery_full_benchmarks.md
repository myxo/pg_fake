# Fresh full-session benchmark audit — 2026-09-30

This is a current-source cross-backend comparison, not a before/after speedup
claim against September 22 or the reverted September 29 source. Environment,
commands and fixture/history limits are recorded in [the recovery audit](recovery_audit.md).
All latency values below are Criterion mean estimates in microseconds, with 95%
confidence intervals. Ratio is pg_fake / PostgreSQL; the plan's 10×-faster target
requires a ratio of at most 0.1. Raw local results are retained under
`target/criterion/**/recovery_full_20260930/`, and the run log is
`/tmp/recovery-full-bench.log`.

| Tier | Paired workloads | pg_fake faster | Meets 10×-faster target | >10× slower |
| --- | ---: | ---: | ---: | ---: |
| 1 | 10 | 8 | 0 | 0 |
| 2 | 40 | 19 | 2 | 0 |
| 3 | 21 | 8 | 0 | 1 |

## Paired SQLx workloads

| Workload | pg_fake mean [95% CI] | PostgreSQL mean [95% CI] | Ratio |
| --- | ---: | ---: | ---: |
| tier1_insert_row | 49.195 [48.997, 49.462] | 93.081 [92.814, 93.375] | 0.529 |
| tier1_limit_offset_ordered_100_rows | 40.368 [40.279, 40.494] | 42.540 [42.243, 42.746] | 0.949 |
| tier1_many_match_inner_join | 177.697 [174.791, 180.513] | 61.762 [60.087, 63.664] | 2.877 |
| tier1_order_by_100_rows | 43.324 [43.277, 43.376] | 66.794 [66.616, 66.966] | 0.649 |
| tier1_select_100_rows | 34.085 [33.918, 34.269] | 59.110 [58.465, 59.591] | 0.577 |
| tier1_select_where_100_rows | 22.980 [22.811, 23.172] | 34.444 [34.367, 34.530] | 0.667 |
| tier1_select_where_indexed_100_rows | 15.629 [15.605, 15.662] | 30.156 [30.090, 30.239] | 0.518 |
| tier1_selective_inner_join | 103.243 [101.610, 104.946] | 37.563 [36.727, 38.427] | 2.749 |
| tier1_transaction_insert | 56.430 [54.996, 58.996] | 135.756 [131.942, 138.026] | 0.416 |
| tier1_update_row | 36.714 [36.433, 37.071] | 95.737 [91.809, 98.500] | 0.383 |
| tier2_alter_table_rewrite_100_rows | 319.609 [316.303, 324.791] | 360.153 [347.555, 372.822] | 0.887 |
| tier2_array_containment_100_rows | 120.948 [120.747, 121.216] | 41.888 [41.577, 42.334] | 2.887 |
| tier2_bigint_uuid_array_bind_store_fetch | 53.957 [53.862, 54.055] | 39.040 [36.082, 42.907] | 1.382 |
| tier2_correlated_exists_100_rows | 80.355 [78.820, 81.876] | 61.851 [60.876, 62.834] | 1.299 |
| tier2_create_table | 49.475 [48.959, 49.911] | 873.598 [848.252, 904.132] | 0.057 |
| tier2_delete_row | 23.866 [23.798, 23.941] | 97.527 [93.846, 99.794] | 0.245 |
| tier2_derived_and_scalar_subquery_100_rows | 420.529 [414.977, 426.203] | 78.312 [77.104, 79.564] | 5.370 |
| tier2_derived_source_join_100_rows | 171.220 [164.671, 181.009] | 66.719 [65.484, 68.034] | 2.566 |
| tier2_foreign_key_insert | 80.623 [78.654, 82.615] | 166.296 [163.037, 169.582] | 0.485 |
| tier2_global_aggregate_100_rows | 39.762 [39.130, 40.409] | 35.660 [35.106, 36.256] | 1.115 |
| tier2_grouped_aggregate_100_rows | 187.977 [185.194, 190.843] | 42.528 [41.939, 43.137] | 4.420 |
| tier2_insert_on_conflict_conflict_free | 63.118 [62.991, 63.248] | 68.284 [68.171, 68.396] | 0.924 |
| tier2_insert_on_conflict_do_nothing | 29.763 [29.698, 29.832] | 31.567 [31.361, 31.825] | 0.943 |
| tier2_insert_on_conflict_do_update | 41.010 [40.957, 41.062] | 35.914 [35.836, 36.011] | 1.142 |
| tier2_insert_row_returning | 55.623 [55.406, 55.883] | 98.520 [97.824, 98.953] | 0.565 |
| tier2_insert_row_with_defaults | 50.374 [50.204, 50.529] | 95.651 [94.976, 96.281] | 0.527 |
| tier2_json_insert_returning | 47.828 [47.780, 47.887] | 73.723 [70.486, 76.712] | 0.649 |
| tier2_jsonb_containment | 271.700 [271.194, 272.343] | 44.108 [43.613, 44.956] | 6.160 |
| tier2_jsonb_extraction | 118.574 [118.426, 118.746] | 67.389 [67.192, 67.605] | 1.760 |
| tier2_jsonb_insert_returning | 51.012 [50.729, 51.438] | 76.474 [74.713, 78.352] | 0.667 |
| tier2_materialized_cte_100_rows | 129.002 [126.569, 131.434] | 70.451 [69.324, 71.650] | 1.831 |
| tier2_nested_savepoint_release | 104.131 [103.868, 104.465] | 190.737 [190.217, 191.376] | 0.546 |
| tier2_nested_savepoint_rollback | 107.434 [107.313, 107.567] | 191.269 [190.508, 192.139] | 0.562 |
| tier2_offset_datetime_bind_store_fetch | 47.962 [47.858, 48.116] | 38.198 [36.920, 40.156] | 1.256 |
| tier2_ordered_string_agg_100_rows | 244.182 [243.930, 244.476] | 70.674 [70.446, 70.908] | 3.455 |
| tier2_runtime_patterns_100_rows | 244.282 [240.612, 249.107] | 118.357 [118.021, 118.704] | 2.064 |
| tier2_runtime_temporal_100_rows | 304.496 [303.888, 305.508] | 118.226 [118.003, 118.452] | 2.576 |
| tier2_select_distinct_100_rows | 49.324 [48.684, 49.966] | 38.956 [38.451, 39.464] | 1.266 |
| tier2_sequence_nextval | 28.836 [28.788, 28.885] | 29.032 [28.956, 29.117] | 0.993 |
| tier2_serial_identity_insert | 27.527 [27.506, 27.549] | 31.759 [30.262, 33.102] | 0.867 |
| tier2_session_settings_roundtrip | 65.536 [65.446, 65.635] | 141.569 [141.137, 141.996] | 0.463 |
| tier2_sqlx_migration_chain | 1089.186 [1085.873, 1093.121] | 6177.355 [5994.454, 6359.094] | 0.176 |
| tier2_transaction_repeatable_read_select_for_update | 63.863 [63.690, 64.025] | 86.976 [86.506, 87.628] | 0.734 |
| tier2_transactional_ddl_create_rollback | 47.139 [47.013, 47.289] | 1390.633 [1229.242, 1654.930] | 0.034 |
| tier2_union_100_rows | 522.237 [510.928, 533.845] | 78.282 [77.169, 79.424] | 6.671 |
| tier2_union_all_100_rows | 492.588 [481.616, 503.757] | 79.472 [78.074, 80.905] | 6.198 |
| tier2_update_from_row | 43.612 [43.395, 43.891] | 101.617 [95.584, 106.059] | 0.429 |
| tier2_uuid_any_100_rows | 60.588 [60.222, 61.024] | 53.824 [53.533, 54.192] | 1.126 |
| tier2_uuid_temporal_select | 35.527 [35.046, 35.933] | 31.437 [31.220, 31.625] | 1.130 |
| tier2_window_row_number_100_rows | 134.458 [134.283, 134.646] | 85.103 [84.907, 85.305] | 1.580 |
| tier3_catalog_regclass_lookup | 64.984 [64.899, 65.080] | 33.738 [33.574, 33.950] | 1.926 |
| tier3_correlated_unnest_100_rows | 197.634 [195.745, 200.012] | 152.112 [151.704, 152.597] | 1.299 |
| tier3_data_modifying_cte_update_100_rows | 470.594 [464.834, 476.068] | 100.683 [99.583, 101.795] | 4.674 |
| tier3_hashed_advisory_lock_acquisition | 52.539 [52.182, 52.902] | 80.054 [79.883, 80.253] | 0.656 |
| tier3_jsonb_join_group | 748.001 [745.736, 750.636] | 348.288 [339.805, 357.575] | 2.148 |
| tier3_lateral_latest_per_parent_100_rows | 1933.900 [1879.895, 1988.889] | 627.572 [612.133, 645.572] | 3.082 |
| tier3_migration_table_lock_two_relations | 28.936 [28.878, 29.015] | 84.871 [84.696, 85.018] | 0.341 |
| tier3_nested_filtered_view_100_rows | 393.170 [391.873, 394.804] | 35.362 [35.108, 35.590] | 11.119 |
| tier3_ordered_filtered_array_agg_100_rows | 204.586 [203.413, 206.076] | 44.246 [44.121, 44.385] | 4.624 |
| tier3_partial_unique_index_100_rows | 87.331 [87.128, 87.532] | 827.478 [712.501, 1021.086] | 0.106 |
| tier3_procedural_trigger_insert_update | 91.330 [91.139, 91.551] | 123.827 [122.789, 124.506] | 0.738 |
| tier3_recursive_cte_branching_traversal_127_rows | 265.820 [259.599, 272.122] | 117.276 [114.556, 120.191] | 2.267 |
| tier3_recursive_cte_numeric_series_100_rows | 151.269 [149.112, 153.504] | 65.944 [64.888, 67.019] | 2.294 |
| tier3_serializable_uncontended_read | 31.619 [30.989, 32.460] | 78.018 [76.618, 79.489] | 0.405 |
| tier3_serializable_write_skew | 170.575 [166.987, 174.000] | 278.940 [270.968, 288.057] | 0.612 |
| tier3_skip_locked_queue_100_rows | 196.797 [194.450, 199.133] | 43.344 [42.723, 43.971] | 4.540 |
| tier3_temporary_table_on_commit_drop | 22.993 [22.952, 23.034] | 202.737 [196.215, 212.554] | 0.113 |
| tier3_transaction_local_guc_roundtrip | 90.279 [90.155, 90.416] | 138.820 [138.490, 139.217] | 0.650 |
| tier3_window_moving_aggregate_100_rows | 399.801 [397.907, 402.865] | 151.560 [151.246, 151.892] | 2.638 |
| tier3_window_offset_100_rows | 571.467 [570.112, 573.171] | 126.355 [126.142, 126.564] | 4.523 |
| tier3_window_rank_100_rows | 304.344 [304.002, 304.662] | 110.376 [110.227, 110.532] | 2.757 |

## Unpaired diagnostics

These cases have no paired PostgreSQL measurement and are not included in the
ratio totals above.

| Diagnostic | Mean [95% CI] (µs) |
| --- | ---: |
| tier2_adapter_overhead_select_100_rows/core | 32.341 [32.165, 32.455] |
| tier2_adapter_overhead_select_100_rows/sqlx | 36.039 [35.845, 36.219] |
| tier2_concurrent_uncontended_reads/parallel | 21.155 [20.836, 21.467] |
| tier2_concurrent_uncontended_reads/sequential | 31.917 [31.125, 32.733] |
| tier2_core_parsed_vs_prepared_point_select/parse_and_analyze | 19.903 [19.887, 19.926] |
| tier2_core_parsed_vs_prepared_point_select/prepared_reuse | 0.614 [0.608, 0.620] |
| tier2_point_lookup_index_vs_scan/heap_scan/100 | 3.929 [3.888, 4.006] |
| tier2_point_lookup_index_vs_scan/heap_scan/10000 | 436.237 [435.923, 436.625] |
| tier2_point_lookup_index_vs_scan/unique_index/100 | 0.631 [0.628, 0.635] |
| tier2_point_lookup_index_vs_scan/unique_index/10000 | 0.784 [0.781, 0.787] |
| tier3_concurrent_same_row_contention/wait_then_rollback | 1935.314 [1906.130, 1964.087] |
| tier3_mvcc_old_snapshot_read/1 | 0.636 [0.635, 0.638] |
| tier3_mvcc_old_snapshot_read/100 | 1.925 [1.916, 1.937] |
| tier3_mvcc_old_snapshot_read/10000 | 659.084 [658.781, 659.457] |
| tier3_transaction_history_point_select/1 | 0.355 [0.355, 0.356] |
| tier3_transaction_history_point_select/100 | 0.360 [0.357, 0.364] |
| tier3_transaction_history_point_select/10000 | 0.359 [0.357, 0.361] |
| tier3_transaction_history_point_select/100000 | 0.356 [0.356, 0.356] |
| tier2_core_snapshot_100_rows/pg_fake | 17.447 [17.435, 17.462] |
