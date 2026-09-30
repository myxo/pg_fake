# Projection-name follow-up: tier 1 and tier 3

Current-source SQLx means and 95% confidence intervals in microseconds, from
Criterion baseline `projection_tiers13`. This combined filtered sweep has
different fixture/history state from the prior full and tier-1-only sweeps.
It is a cross-backend assessment, not a source-change A/B estimate. The exact
paired experiment is in [projection names](projection_names.md).

| Workload | pg_fake mean [95% CI] | PostgreSQL mean [95% CI] | Ratio |
| --- | ---: | ---: | ---: |
| tier1_insert_row | 48.918 [48.714, 49.226] | 93.399 [92.963, 93.714] | 0.524 |
| tier1_limit_offset_ordered_100_rows | 40.228 [40.194, 40.261] | 43.106 [42.987, 43.251] | 0.933 |
| tier1_many_match_inner_join | 164.812 [164.564, 165.184] | 66.846 [66.767, 66.924] | 2.466 |
| tier1_order_by_100_rows | 43.829 [43.732, 43.918] | 67.164 [66.759, 67.470] | 0.653 |
| tier1_select_100_rows | 33.543 [33.193, 33.792] | 60.333 [59.911, 60.863] | 0.556 |
| tier1_select_where_100_rows | 22.174 [21.953, 22.437] | 34.862 [34.074, 35.305] | 0.636 |
| tier1_select_where_indexed_100_rows | 15.428 [15.346, 15.505] | 35.249 [34.985, 35.612] | 0.438 |
| tier1_selective_inner_join | 97.810 [97.706, 97.962] | 41.064 [40.862, 41.254] | 2.382 |
| tier1_transaction_insert | 55.121 [54.665, 55.930] | 138.894 [138.287, 139.645] | 0.397 |
| tier1_update_row | 36.276 [35.776, 36.942] | 95.230 [89.552, 99.082] | 0.381 |
| tier3_catalog_regclass_lookup | 55.478 [55.251, 55.633] | 33.504 [33.357, 33.651] | 1.656 |
| tier3_correlated_unnest_100_rows | 193.527 [192.349, 194.946] | 152.795 [152.435, 153.171] | 1.267 |
| tier3_data_modifying_cte_update_100_rows | 447.955 [446.911, 449.121] | 105.146 [104.774, 105.540] | 4.260 |
| tier3_hashed_advisory_lock_acquisition | 50.883 [50.736, 51.040] | 79.298 [76.582, 81.336] | 0.642 |
| tier3_jsonb_join_group | 757.031 [753.451, 760.406] | 350.193 [341.555, 359.553] | 2.162 |
| tier3_lateral_latest_per_parent_100_rows | 1684.939 [1673.710, 1705.539] | 569.349 [553.788, 583.837] | 2.959 |
| tier3_migration_table_lock_two_relations | 29.868 [29.772, 29.969] | 85.424 [85.265, 85.580] | 0.350 |
| tier3_nested_filtered_view_100_rows | 371.043 [370.288, 371.981] | 35.820 [35.162, 36.247] | 10.359 |
| tier3_ordered_filtered_array_agg_100_rows | 202.150 [201.983, 202.321] | 44.962 [44.825, 45.119] | 4.496 |
| tier3_partial_unique_index_100_rows | 85.367 [85.180, 85.546] | 557.234 [532.059, 590.235] | 0.153 |
| tier3_procedural_trigger_insert_update | 90.821 [90.738, 90.902] | 123.834 [123.639, 124.037] | 0.733 |
| tier3_recursive_cte_branching_traversal_127_rows | 251.688 [249.938, 253.643] | 123.545 [123.312, 123.784] | 2.037 |
| tier3_recursive_cte_numeric_series_100_rows | 143.700 [143.438, 143.977] | 74.685 [74.518, 74.872] | 1.924 |
| tier3_serializable_uncontended_read | 37.337 [37.282, 37.401] | 87.118 [86.940, 87.348] | 0.429 |
| tier3_serializable_write_skew | 181.037 [180.555, 181.513] | 303.110 [302.544, 303.706] | 0.597 |
| tier3_skip_locked_queue_100_rows | 186.260 [185.622, 187.004] | 48.696 [48.577, 48.840] | 3.825 |
| tier3_temporary_table_on_commit_drop | 21.974 [21.942, 22.008] | 197.919 [195.312, 200.958] | 0.111 |
| tier3_transaction_local_guc_roundtrip | 88.307 [87.971, 88.654] | 138.648 [137.980, 139.130] | 0.637 |
| tier3_window_moving_aggregate_100_rows | 406.245 [405.199, 407.396] | 152.943 [152.044, 154.040] | 2.656 |
| tier3_window_offset_100_rows | 587.372 [581.021, 596.175] | 126.710 [126.439, 127.004] | 4.636 |
| tier3_window_rank_100_rows | 302.531 [302.115, 302.968] | 109.749 [109.612, 109.894] | 2.757 |

| Tier | Pairs | pg_fake faster | Meets PostgreSQL/10 |
| --- | ---: | ---: | ---: |
| 1 | 10 | 8 | 0 |
| 3 | 21 | 8 | 0 |

Group-specific measurement overrides remain active. No build, test, other
benchmark or sampling overlapped the run. Log: `/tmp/projection-tiers13.log`.
Raw estimates: `target/criterion/**/projection_tiers13/`.

## Unpaired diagnostics

These eight measurements are not PostgreSQL comparisons. Together with the
62 paired-backend estimates above, all 70 records are accounted for.

| Workload / function | Mean [95% CI], µs |
| --- | ---: |
| tier3_concurrent_same_row_contention/wait_then_rollback | 1885.530 [1826.039, 1942.429] |
| tier3_mvcc_old_snapshot_read/1 | 0.645 [0.643, 0.647] |
| tier3_mvcc_old_snapshot_read/100 | 2.087 [1.898, 2.343] |
| tier3_mvcc_old_snapshot_read/10000 | 673.184 [666.489, 685.045] |
| tier3_transaction_history_point_select/1 | 0.364 [0.361, 0.367] |
| tier3_transaction_history_point_select/100 | 0.361 [0.359, 0.363] |
| tier3_transaction_history_point_select/10000 | 0.359 [0.359, 0.360] |
| tier3_transaction_history_point_select/100000 | 0.363 [0.360, 0.367] |
