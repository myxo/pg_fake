# E14: reusable window-expression templates

Baseline is the recovered E11/E7/E13 worktree, before this experiment. Source
snapshots are `/tmp/e14-windows-before.rs` and `/tmp/e14-grouping-before.rs`.
The candidate is retained after all 22 paired cases improved. Independent code
review and 284 core tests pass, as do both migration-transform differential
tests against PostgreSQL.

Native harness: `cargo build -p pg_fake --release --example window_probe`,
then run `target/release/examples/window_probe` three times sequentially.
macOS arm64, Cargo 1.98.1, default features, execution tracing disabled.
Each fresh 100/1,000-row table has ten buckets. The harness measures ordered
row_number, five lag/lead/value functions, a three-row moving SUM, and a
10-group aggregate control. Each query has 300 timed native autocommit
executions, both one-shot and explicitly prepared. Fixture creation,
preparation, and first execution are outside the timed loop; their diagnostic
observations are saved separately below. No compilation or profiling overlaps
measurement.

SQLx companion: `window_adapter_probe` uses the same window queries and fixture
sizes, warm persistent caching, 300 iterations, and decodes every output value.
Build both baseline executables with `cargo build -p pg_fake -p pg_fake_sqlx
--release --example window_probe --example window_adapter_probe`, then run both
sequentially for three rounds. This second paired series is the acceptance
baseline, because the native-only initial series did not include the adapter.

The candidate compiles owner-specific occurrences to internal slots once per
output expression, and substitutes typed window values into the smaller AST
per row. Expressions without window calls are borrowed. It also fixes the
pre-existing collector mismatch: deterministic window functions were shared
across owners while materialization required an owner match. Volatile
occurrences remain separate. Quoted SQL identifiers cannot become slot markers.

## Initial native baseline

Microseconds per operation. This series is preliminary; use the paired
native/adapter series for the final before/after decision.

| Case | Three runs | Mean |
| --- | --- | ---: |
| rows=100 case=row_number prepared=false | 212.605, 153.357, 154.589 | 173.517 |
| rows=100 case=row_number prepared=true | 145.341, 141.263, 141.589 | 142.731 |
| rows=100 case=five_windows prepared=false | 1020.869, 1004.751, 1011.949 | 1012.523 |
| rows=100 case=five_windows prepared=true | 1000.318, 982.402, 988.833 | 990.518 |
| rows=100 case=moving prepared=false | 256.449, 257.642, 259.256 | 257.782 |
| rows=100 case=moving prepared=true | 241.468, 245.152, 245.229 | 243.950 |
| rows=100 case=grouped prepared=false | 156.348, 157.006, 155.320 | 156.225 |
| rows=100 case=grouped prepared=true | 148.519, 150.464, 147.494 | 148.826 |
| rows=1000 case=row_number prepared=false | 1295.559, 1318.881, 1292.827 | 1302.422 |
| rows=1000 case=row_number prepared=true | 1277.100, 1262.777, 1278.857 | 1272.911 |
| rows=1000 case=five_windows prepared=false | 9300.681, 9278.868, 9398.539 | 9326.029 |
| rows=1000 case=five_windows prepared=true | 9335.996, 9278.899, 9372.148 | 9329.014 |
| rows=1000 case=moving prepared=false | 2228.991, 2200.571, 2198.510 | 2209.357 |
| rows=1000 case=moving prepared=true | 2183.611, 2168.164, 2177.401 | 2176.392 |
| rows=1000 case=grouped prepared=false | 891.253, 881.276, 849.287 | 873.939 |
| rows=1000 case=grouped prepared=true | 921.036, 910.250, 879.588 | 903.625 |

## Paired baseline

| Case | Three runs | Mean |
| --- | --- | ---: |
| rows=100 case=row_number prepared=false | 207.223, 152.400, 151.815 | 170.479 |
| rows=100 case=row_number prepared=true | 143.500, 142.223, 139.453 | 141.725 |
| rows=100 case=five_windows prepared=false | 1023.588, 1021.767, 1001.524 | 1015.626 |
| rows=100 case=five_windows prepared=true | 1004.901, 1022.568, 1013.113 | 1013.527 |
| rows=100 case=moving prepared=false | 255.006, 271.130, 257.747 | 261.294 |
| rows=100 case=moving prepared=true | 243.697, 246.985, 241.582 | 244.088 |
| rows=100 case=grouped prepared=false | 155.842, 154.991, 154.225 | 155.019 |
| rows=100 case=grouped prepared=true | 148.812, 147.606, 147.812 | 148.077 |
| rows=1000 case=row_number prepared=false | 1316.385, 1302.983, 1279.970 | 1299.779 |
| rows=1000 case=row_number prepared=true | 1300.266, 1275.535, 1256.688 | 1277.496 |
| rows=1000 case=five_windows prepared=false | 9532.741, 9428.418, 9284.014 | 9415.058 |
| rows=1000 case=five_windows prepared=true | 9520.095, 9368.369, 9266.968 | 9385.144 |
| rows=1000 case=moving prepared=false | 2221.554, 2224.904, 2189.345 | 2211.934 |
| rows=1000 case=moving prepared=true | 2186.338, 2183.805, 2192.687 | 2187.610 |
| rows=1000 case=grouped prepared=false | 876.789, 857.527, 857.797 | 864.038 |
| rows=1000 case=grouped prepared=true | 901.600, 897.932, 886.717 | 895.416 |
| SQLx rows=100 case=row_number persistent=true | 214.033, 169.618, 169.695 | 184.449 |
| SQLx rows=100 case=five_windows persistent=true | 1014.725, 1016.219, 1005.662 | 1012.202 |
| SQLx rows=100 case=moving persistent=true | 268.418, 270.538, 268.527 | 269.161 |
| SQLx rows=1000 case=row_number persistent=true | 1371.680, 1396.784, 1377.255 | 1381.906 |
| SQLx rows=1000 case=five_windows persistent=true | 9468.927, 9432.484, 9536.613 | 9479.341 |
| SQLx rows=1000 case=moving persistent=true | 2293.168, 2292.124, 2306.980 | 2297.424 |

## Retained candidate

Microseconds per operation; all 22 paired cases improve.

| Case | Three runs | Mean | Reduction |
| --- | --- | ---: | ---: |
| rows=100 case=row_number prepared=false | 163.794, 114.063, 111.734 | 129.864 | 23.8% |
| rows=100 case=row_number prepared=true | 105.760, 100.578, 97.681 | 101.340 | 28.5% |
| rows=100 case=five_windows prepared=false | 574.159, 571.542, 554.138 | 566.613 | 44.2% |
| rows=100 case=five_windows prepared=true | 537.942, 529.326, 513.013 | 526.760 | 48.0% |
| rows=100 case=moving prepared=false | 171.463, 175.855, 169.036 | 172.118 | 34.1% |
| rows=100 case=moving prepared=true | 150.975, 151.877, 148.742 | 150.531 | 38.3% |
| rows=100 case=grouped prepared=false | 151.338, 147.750, 147.029 | 148.706 | 4.1% |
| rows=100 case=grouped prepared=true | 141.673, 137.030, 136.482 | 138.395 | 6.5% |
| rows=1000 case=row_number prepared=false | 888.233, 893.638, 867.328 | 883.066 | 32.1% |
| rows=1000 case=row_number prepared=true | 847.026, 850.527, 830.786 | 842.780 | 34.0% |
| rows=1000 case=five_windows prepared=false | 4894.490, 4899.259, 4779.580 | 4857.776 | 48.4% |
| rows=1000 case=five_windows prepared=true | 4688.472, 4636.092, 4552.040 | 4625.535 | 50.7% |
| rows=1000 case=moving prepared=false | 1347.844, 1347.723, 1343.595 | 1346.387 | 39.1% |
| rows=1000 case=moving prepared=true | 1278.398, 1264.377, 1264.600 | 1269.125 | 42.0% |
| rows=1000 case=grouped prepared=false | 858.681, 832.517, 832.761 | 841.320 | 2.6% |
| rows=1000 case=grouped prepared=true | 867.800, 868.894, 852.194 | 862.963 | 3.6% |
| SQLx rows=100 case=row_number persistent=true | 165.378, 122.099, 123.906 | 137.128 | 25.7% |
| SQLx rows=100 case=five_windows persistent=true | 559.859, 562.571, 546.972 | 556.467 | 45.0% |
| SQLx rows=100 case=moving persistent=true | 176.262, 174.184, 174.152 | 174.866 | 35.0% |
| SQLx rows=1000 case=row_number persistent=true | 933.207, 939.798, 946.283 | 939.763 | 32.0% |
| SQLx rows=1000 case=five_windows persistent=true | 4783.409, 4841.155, 4770.692 | 4798.419 | 49.4% |
| SQLx rows=1000 case=moving persistent=true | 1388.055, 1369.660, 1384.012 | 1380.576 | 39.9% |

## Prepare and first-execution diagnostics

Single observations per run, in microseconds; these cold-path observations are
noisy and are not stable speedup estimates. All runs are retained.

| Case | Baseline prepare / first | Candidate prepare / first |
| --- | --- | --- |
| rows=100 case=row_number | 1795.833 / 1130.542; 129.042 / 205.834; 133.375 / 203.250 | 1876.500 / 1166.625; 132.333 / 161.417; 148.542 / 169.000 |
| rows=100 case=five_windows | 182.250 / 1189.417; 106.875 / 1014.333; 102.875 / 998.458 | 189.375 / 647.000; 106.041 / 569.583; 111.375 / 540.458 |
| rows=100 case=moving | 153.166 / 284.500; 92.417 / 272.083; 118.041 / 272.209 | 93.750 / 202.500; 63.917 / 165.291; 49.000 / 160.417 |
| rows=100 case=grouped | 48.750 / 274.917; 53.542 / 186.625; 46.583 / 195.792 | 41.834 / 264.208; 39.542 / 174.417; 33.917 / 170.375 |
| rows=1000 case=row_number | 58.459 / 1357.500; 45.084 / 1280.625; 50.417 / 1284.916 | 130.708 / 935.542; 39.791 / 883.917; 44.833 / 858.333 |
| rows=1000 case=five_windows | 106.375 / 9489.625; 89.291 / 9478.333; 90.750 / 9323.542 | 117.375 / 4641.667; 85.292 / 4674.834; 109.958 / 4595.917 |
| rows=1000 case=moving | 110.833 / 2263.958; 110.875 / 2194.792; 106.291 / 2198.417 | 87.125 / 1310.875; 104.500 / 1293.291; 86.167 / 1285.750 |
| rows=1000 case=grouped | 49.708 / 919.458; 55.417 / 909.791; 52.875 / 903.916 | 64.000 / 903.584; 46.458 / 866.125; 48.375 / 864.667 |

## Validation and decision

Retain this measured candidate. 284 core library tests and both PostgreSQL
migration-transform differential tests pass. The new differential test covers
multiple output owners, repeated deterministic calls, grouped windows, nested
queries, DISTINCT ordering, quoted names, and duplicate sequence-backed volatile
calls. Formatting passes. Independent code and harness reviews found no remaining
issue. No partition/specification cache or moving-frame algorithm rewrite was
added. This accepted experiment is committed as `64c30e9`.
