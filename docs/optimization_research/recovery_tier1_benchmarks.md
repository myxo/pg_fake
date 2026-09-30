# Filtered tier-1 audit — 2026-09-30

This separate run uses baseline `recovery_tier1_20260930`. It is a current-source
cross-backend comparison, not an optimization before/after comparison. Filtering
changes accumulated fixture/history state; do not merge these estimates with
the [full sweep](recovery_full_benchmarks.md). Means and 95% confidence intervals
are in microseconds. Ratio is pg_fake / PostgreSQL.

| Workload | pg_fake mean [95% CI] | PostgreSQL mean [95% CI] | Ratio |
| --- | ---: | ---: | ---: |
| tier1_insert_row | 48.179 [47.865, 48.588] | 95.331 [93.908, 97.018] | 0.505 |
| tier1_limit_offset_ordered_100_rows | 40.077 [40.032, 40.121] | 43.049 [42.992, 43.107] | 0.931 |
| tier1_many_match_inner_join | 166.344 [165.584, 167.354] | 66.979 [66.780, 67.180] | 2.484 |
| tier1_order_by_100_rows | 44.223 [44.156, 44.294] | 66.553 [66.441, 66.680] | 0.664 |
| tier1_select_100_rows | 33.673 [33.603, 33.748] | 59.494 [59.300, 59.697] | 0.566 |
| tier1_select_where_100_rows | 22.158 [22.119, 22.195] | 35.123 [35.028, 35.222] | 0.631 |
| tier1_select_where_indexed_100_rows | 15.608 [15.595, 15.622] | 30.103 [29.742, 30.331] | 0.518 |
| tier1_selective_inner_join | 98.090 [97.924, 98.248] | 41.443 [41.302, 41.559] | 2.367 |
| tier1_transaction_insert | 55.351 [54.911, 56.091] | 137.499 [137.144, 137.820] | 0.403 |
| tier1_update_row | 36.755 [36.542, 37.022] | 97.138 [93.724, 99.181] | 0.378 |

8/10 paired workloads are faster; 0/10 meet the PostgreSQL/10 target.

Reproduction:

```sh
cargo bench -p pg_fake_benchmarks --bench workloads -- tier1_ \
  --warm-up-time 0.3 --measurement-time 1 --sample-size 20 \
  --save-baseline recovery_tier1_20260930
```

Group-specific timing/sample overrides remain active. Raw estimates are under
`target/criterion/**/recovery_tier1_20260930/`; log:
`/tmp/recovery-tier1-bench.log`. No other build/test/profile overlapped the run.
