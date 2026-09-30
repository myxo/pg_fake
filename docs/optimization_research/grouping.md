# High-cardinality grouping follow-up

This is a fresh experiment after the recovered E11/E7/E13/E14 changes.
The final normalization/threshold candidate is retained for its measured
high-cardinality gain, with the small-group cost disclosed below. Baseline source snapshots are
`/tmp/grouping-{mod,equality,joins,advisory}-before.rs`.

Build with `cargo build -p pg_fake_sqlx --release --example grouping_probe`.
Run `target/release/examples/grouping_probe` three times sequentially, without
compilation, testing or profiling in parallel. macOS arm64, Cargo 1.98.1,
default features, execution tracing disabled. Every native/SQLx case owns a
fresh table with 100, 1,000 or 10,000 rows and either ten or one group per row.
There is no primary key; fixture insertion is outside timing. Native one-shot
and prepared executions are reported separately, with prepare/first diagnostics.
SQLx uses a warmed persistent statement cache and decodes both output columns.
Cases use 300 iterations, reduced to 30 at 10,000 rows.

Query: `SELECT bucket, count(*) FROM t GROUP BY bucket ORDER BY bucket`.

A separate `--profile` invocation runs only the 1,000-row/1,000-group native
case for 10,000 iterations. macOS `sample` observed it for two seconds at a
requested 1 ms interval, after a one-second startup delay and the printed
prepare/first diagnostics. Setup, compilation and benchmarks did not overlap
the profile. The main thread had 1,435 samples; `collect_grouped_select_rows`
had 748 inclusive samples (52.1%). The group-matching closure had substantial
self samples and its `compare_values` child had 206 samples. Inclusive counts
overlap and must not be summed. Raw local evidence is
`/tmp/grouping-profile.txt` and `/tmp/grouping-profile-run.log`.

The candidate retains the existing vector of groups and visitation order, adding
an execution-local hash lookup of existing equality keys. NULLs are represented
explicitly; types without a supported hash key retain comparison scanning.
The final candidate normalizes bpchar key values once and retains the original
equality scan. Both hashing and fallback ignore trailing bpchar spaces even
when another key disables hashing. Join and equality modules remain unchanged.
The lookup is rebuilt from captured partial groups when execution resumes.

## Baseline

Microseconds per operation.

| Case | Three runs | Mean |
| --- | --- | ---: |
| native rows=100 groups=10 prepared=false | 118.013, 107.899, 106.986 | 110.966 |
| native rows=100 groups=10 prepared=true | 99.059, 99.355, 100.864 | 99.759 |
| SQLx rows=100 groups=10 persistent=true | 119.649, 119.711, 120.069 | 119.810 |
| native rows=100 groups=100 prepared=false | 300.758, 299.474, 296.246 | 298.826 |
| native rows=100 groups=100 prepared=true | 289.384, 293.419, 297.844 | 293.549 |
| SQLx rows=100 groups=100 persistent=true | 314.416, 312.184, 310.510 | 312.370 |
| native rows=1000 groups=10 prepared=false | 621.791, 620.501, 622.935 | 621.742 |
| native rows=1000 groups=10 prepared=true | 624.177, 629.041, 630.952 | 628.057 |
| SQLx rows=1000 groups=10 persistent=true | 655.419, 642.883, 638.229 | 645.510 |
| native rows=1000 groups=1000 prepared=false | 4197.659, 4185.309, 4159.178 | 4180.715 |
| native rows=1000 groups=1000 prepared=true | 4206.000, 4208.583, 4210.138 | 4208.240 |
| SQLx rows=1000 groups=1000 persistent=true | 4285.103, 4266.277, 4275.250 | 4275.543 |
| native rows=10000 groups=10 prepared=false | 5777.250, 5750.558, 5742.012 | 5756.607 |
| native rows=10000 groups=10 prepared=true | 5890.949, 5900.074, 5882.067 | 5891.030 |
| SQLx rows=10000 groups=10 persistent=true | 6081.051, 5873.346, 5849.054 | 5934.484 |
| native rows=10000 groups=10000 prepared=false | 191958.406, 190981.807, 190491.075 | 191143.763 |
| native rows=10000 groups=10000 prepared=true | 192267.847, 191872.362, 191448.626 | 191862.945 |
| SQLx rows=10000 groups=10000 persistent=true | 192566.165, 193700.556, 191721.831 | 192662.851 |

Prepare / first diagnostics are single observations, not stable latency estimates.

| Case | Three prepare / first observations (µs) |
| --- | --- |
| rows=100 groups=10 | 152.166 / 231.750; 124.458 / 177.625; 126.667 / 175.292 |
| rows=100 groups=100 | 38.833 / 340.666; 33.250 / 325.166; 41.250 / 316.917 |
| rows=1000 groups=10 | 38.333 / 631.750; 50.667 / 651.417; 94.334 / 668.625 |
| rows=1000 groups=1000 | 69.792 / 4336.125; 38.000 / 4346.250; 42.750 / 4415.750 |
| rows=10000 groups=10 | 119.125 / 6013.250; 122.375 / 6002.750; 119.708 / 6040.042 |
| rows=10000 groups=10000 | 119.417 / 192791.959; 112.375 / 192127.625; 114.000 / 192022.500 |

## Rejected always-hash candidate

Despite the high-cardinality gain, ten-group controls regress. This candidate
was rejected and refined to retain scanning below 128 groups. All values are µs.

| Case | Three runs | Mean | Reduction |
| --- | --- | ---: | ---: |
| native rows=100 groups=10 prepared=false | 135.363, 112.543, 116.880 | 121.595 | -9.6% |
| native rows=100 groups=10 prepared=true | 121.239, 104.019, 107.026 | 110.761 | -11.0% |
| SQLx rows=100 groups=10 persistent=true | 129.540, 125.498, 126.182 | 127.073 | -6.1% |
| native rows=100 groups=100 prepared=false | 311.142, 303.564, 301.991 | 305.566 | -2.3% |
| native rows=100 groups=100 prepared=true | 304.857, 291.512, 291.178 | 295.849 | -0.8% |
| SQLx rows=100 groups=100 persistent=true | 323.792, 308.263, 314.245 | 315.433 | -1.0% |
| native rows=1000 groups=10 prepared=false | 688.398, 670.200, 673.937 | 677.512 | -9.0% |
| native rows=1000 groups=10 prepared=true | 695.343, 682.819, 692.543 | 690.235 | -9.9% |
| SQLx rows=1000 groups=10 persistent=true | 711.690, 689.303, 694.898 | 698.630 | -8.2% |
| native rows=1000 groups=1000 prepared=false | 2893.644, 2786.226, 2791.366 | 2823.745 | 32.5% |
| native rows=1000 groups=1000 prepared=true | 2856.786, 2758.339, 2715.353 | 2776.826 | 34.0% |
| SQLx rows=1000 groups=1000 persistent=true | 2913.471, 2835.841, 2815.137 | 2854.816 | 33.2% |
| native rows=10000 groups=10 prepared=false | 6266.018, 6124.099, 6273.558 | 6221.225 | -8.1% |
| native rows=10000 groups=10 prepared=true | 6599.824, 6306.243, 6363.196 | 6423.088 | -9.0% |
| SQLx rows=10000 groups=10 persistent=true | 6380.771, 6923.374, 6298.010 | 6534.052 | -10.1% |
| native rows=10000 groups=10000 prepared=false | 35557.508, 35546.896, 35217.603 | 35440.669 | 81.5% |
| native rows=10000 groups=10000 prepared=true | 34858.733, 36325.543, 35131.996 | 35438.757 | 81.5% |
| SQLx rows=10000 groups=10000 persistent=true | 35314.026, 36508.086, 35694.131 | 35838.748 | 81.4% |

## Intermediate adaptive candidate with typed scans

Scanning below 128 groups reduced the regression but still added overhead to
small-group native controls. This version is superseded by a refinement that
normalizes bpchar keys once per row and reuses the original equality scan.
All values are microseconds.

| Case | Three runs | Mean | Reduction |
| --- | --- | ---: | ---: |
| native rows=100 groups=10 prepared=false | 157.255, 109.116, 108.434 | 124.935 | -12.6% |
| native rows=100 groups=10 prepared=true | 107.035, 104.312, 101.338 | 104.228 | -4.5% |
| SQLx rows=100 groups=10 persistent=true | 118.993, 122.137, 119.912 | 120.347 | -0.4% |
| native rows=100 groups=100 prepared=false | 306.582, 306.170, 303.800 | 305.517 | -2.2% |
| native rows=100 groups=100 prepared=true | 299.264, 297.650, 296.178 | 297.697 | -1.4% |
| SQLx rows=100 groups=100 persistent=true | 318.152, 315.054, 317.752 | 316.986 | -1.5% |
| native rows=1000 groups=10 prepared=false | 647.332, 621.574, 635.446 | 634.784 | -2.1% |
| native rows=1000 groups=10 prepared=true | 680.354, 638.120, 643.024 | 653.833 | -4.1% |
| SQLx rows=1000 groups=10 persistent=true | 668.727, 647.836, 648.162 | 654.908 | -1.5% |
| native rows=1000 groups=1000 prepared=false | 2767.977, 2785.428, 2790.044 | 2781.150 | 33.5% |
| native rows=1000 groups=1000 prepared=true | 2765.138, 2792.423, 2789.293 | 2782.285 | 33.9% |
| SQLx rows=1000 groups=1000 persistent=true | 2852.669, 2838.027, 3078.283 | 2922.993 | 31.6% |
| native rows=10000 groups=10 prepared=false | 5864.342, 5780.738, 6038.396 | 5894.492 | -2.4% |
| native rows=10000 groups=10 prepared=true | 6174.071, 6100.796, 6359.704 | 6211.524 | -5.4% |
| SQLx rows=10000 groups=10 persistent=true | 5960.440, 5918.688, 5909.675 | 5929.601 | 0.1% |
| native rows=10000 groups=10000 prepared=false | 35039.336, 33685.972, 34145.492 | 34290.267 | 82.1% |
| native rows=10000 groups=10000 prepared=true | 35295.314, 35299.907, 34885.806 | 35160.342 | 81.7% |
| SQLx rows=10000 groups=10000 persistent=true | 35138.688, 35787.104, 35880.392 | 35602.061 | 81.5% |

## Final candidate: normalized keys and original scan

Microseconds per operation. Negative reductions are reported rather than hidden.

| Case | Three runs | Mean | Reduction |
| --- | --- | ---: | ---: |
| native rows=100 groups=10 prepared=false | 162.016, 111.073, 107.144 | 126.744 | -14.2% |
| native rows=100 groups=10 prepared=true | 107.092, 104.438, 99.018 | 103.516 | -3.8% |
| SQLx rows=100 groups=10 persistent=true | 119.468, 124.772, 119.769 | 121.336 | -1.3% |
| native rows=100 groups=100 prepared=false | 300.010, 310.718, 299.970 | 303.566 | -1.6% |
| native rows=100 groups=100 prepared=true | 293.387, 296.584, 295.970 | 295.314 | -0.6% |
| SQLx rows=100 groups=100 persistent=true | 314.071, 313.173, 312.588 | 313.277 | -0.3% |
| native rows=1000 groups=10 prepared=false | 637.293, 627.060, 629.748 | 631.367 | -1.5% |
| native rows=1000 groups=10 prepared=true | 631.775, 643.790, 637.418 | 637.661 | -1.5% |
| SQLx rows=1000 groups=10 persistent=true | 651.864, 655.086, 652.292 | 653.081 | -1.2% |
| native rows=1000 groups=1000 prepared=false | 2776.432, 2736.617, 2758.968 | 2757.339 | 34.0% |
| native rows=1000 groups=1000 prepared=true | 2764.828, 2772.875, 2771.295 | 2769.666 | 34.2% |
| SQLx rows=1000 groups=1000 persistent=true | 2836.684, 2895.655, 2841.604 | 2857.981 | 33.2% |
| native rows=10000 groups=10 prepared=false | 5964.774, 5817.776, 5826.292 | 5869.614 | -2.0% |
| native rows=10000 groups=10 prepared=true | 6266.137, 5952.983, 5940.711 | 6053.277 | -2.8% |
| SQLx rows=10000 groups=10 persistent=true | 6019.071, 5938.857, 5935.044 | 5964.324 | -0.5% |
| native rows=10000 groups=10000 prepared=false | 34851.661, 34260.714, 34855.185 | 34655.853 | 81.9% |
| native rows=10000 groups=10000 prepared=true | 35123.243, 35510.486, 34951.808 | 35195.179 | 81.7% |
| SQLx rows=10000 groups=10000 persistent=true | 36340.222, 35195.890, 35434.126 | 35656.746 | 81.5% |

| Case | Three prepare / first observations (µs) |
| --- | --- |
| rows=100 groups=10 | 1765.833 / 1397.167; 131.541 / 191.875; 125.333 / 179.541 |
| rows=100 groups=100 | 32.333 / 367.125; 49.833 / 344.959; 30.167 / 323.375 |
| rows=1000 groups=10 | 38.416 / 674.959; 44.000 / 648.291; 39.750 / 664.958 |
| rows=1000 groups=1000 | 58.209 / 2972.959; 35.625 / 2882.417; 31.708 / 2914.916 |
| rows=10000 groups=10 | 112.000 / 6179.417; 113.542 / 6058.250; 116.250 / 6069.917 |
| rows=10000 groups=10000 | 114.708 / 35880.416; 117.250 / 35538.708; 122.834 / 36164.875 |

The intended high-cardinality cases improve 33–34% at 1,000 groups and
81–82% at 10,000 groups. Small-group cases do not improve: most means are
0.3–3.8% higher. The 100-row one-shot ten-group mean is dominated by its first
162.016 µs observation (later runs 111.073 and 107.144 µs), so its reported
14.2% regression is noisy. Separate three-run `--small` repeats below investigate
this startup variability; they skip only the 10,000-row fixtures.
No across-the-board speedup is claimed.

Validation: 285 core tests pass, including resumption after hashing begins and
exactly-once sequence effects. The grouping-key PostgreSQL differential test and
both migration-transform tests pass. Coverage includes NULLs, bpchar trailing
spaces and representative output values, JSONB numeric/object equality, numeric
and float fallback (including float NaN), arrays/dates, mixed CASE result types,
late transition from hashable NULL keys to numeric fallback, and joins.
Numeric NaN is excluded because the existing INSERT path rejects it before
grouping; this experiment does not change numeric input support.
Independent source review found no remaining issue. The final implementation
normalizes bpchar grouping keys once, reuses the original equality scan below
128 groups, and builds a lookup once above that threshold. Unsupported keys
permanently disable hashing for the current scan. Equality and join modules are
unchanged from baseline.

## Small-fixture repeats and retention decision

The original baseline followed a separate profiling invocation; its executable
had already run. The first final-candidate launch after each rebuild repeatedly
showed inflated first-case timings. We therefore retained a second three-run
small-fixture series without another rebuild. This reduces that asymmetry but
does not prove all residual variation is noise. Both repeat series are shown.

### First small-fixture series after the example rebuild

| Case | Three runs (µs) | Mean | Reduction vs original baseline |
| --- | --- | ---: | ---: |
| native rows=100 groups=10 prepared=false | 159.029, 108.202, 111.935 | 126.389 | -13.9% |
| native rows=100 groups=10 prepared=true | 106.068, 99.222, 106.856 | 104.049 | -4.3% |
| SQLx rows=100 groups=10 persistent=true | 119.698, 119.898, 126.902 | 122.166 | -2.0% |
| native rows=100 groups=100 prepared=false | 302.469, 298.339, 311.769 | 304.192 | -1.8% |
| native rows=100 groups=100 prepared=true | 291.643, 289.577, 298.801 | 293.340 | 0.1% |
| SQLx rows=100 groups=100 persistent=true | 313.720, 312.491, 324.510 | 316.907 | -1.5% |
| native rows=1000 groups=10 prepared=false | 644.802, 636.628, 658.399 | 646.610 | -4.0% |
| native rows=1000 groups=10 prepared=true | 666.828, 630.586, 644.001 | 647.138 | -3.0% |
| SQLx rows=1000 groups=10 persistent=true | 672.816, 651.665, 677.153 | 667.211 | -3.4% |
| native rows=1000 groups=1000 prepared=false | 2797.048, 2806.423, 2849.297 | 2817.589 | 32.6% |
| native rows=1000 groups=1000 prepared=true | 2818.479, 2852.051, 2867.485 | 2846.005 | 32.4% |
| SQLx rows=1000 groups=1000 persistent=true | 2832.160, 2933.443, 2913.703 | 2893.102 | 32.3% |

### Three additional rounds without rebuilding

| Case | Three runs (µs) | Mean | Reduction vs original baseline |
| --- | --- | ---: | ---: |
| native rows=100 groups=10 prepared=false | 125.550, 108.474, 109.428 | 114.484 | -3.2% |
| native rows=100 groups=10 prepared=true | 103.034, 99.755, 99.891 | 100.893 | -1.1% |
| SQLx rows=100 groups=10 persistent=true | 124.588, 119.679, 120.595 | 121.621 | -1.5% |
| native rows=100 groups=100 prepared=false | 309.752, 303.059, 302.173 | 304.995 | -2.1% |
| native rows=100 groups=100 prepared=true | 298.947, 289.406, 297.083 | 295.145 | -0.5% |
| SQLx rows=100 groups=100 persistent=true | 326.666, 312.902, 313.094 | 317.554 | -1.7% |
| native rows=1000 groups=10 prepared=false | 648.374, 630.075, 636.673 | 638.374 | -2.7% |
| native rows=1000 groups=10 prepared=true | 646.143, 628.145, 631.567 | 635.285 | -1.2% |
| SQLx rows=1000 groups=10 persistent=true | 671.598, 651.754, 651.396 | 658.249 | -2.0% |
| native rows=1000 groups=1000 prepared=false | 2854.598, 2788.640, 2735.717 | 2792.985 | 33.2% |
| native rows=1000 groups=1000 prepared=true | 2870.503, 2797.012, 2739.396 | 2802.304 | 33.4% |
| SQLx rows=1000 groups=1000 persistent=true | 2820.574, 2948.288, 2837.133 | 2868.665 | 32.9% |

Retain the final normalization/threshold candidate for its intended
high-cardinality workload: 33–34% lower latency at 1,000 groups and 81–82% at
10,000 groups, removing the observed quadratic lookup scaling. This satisfies
the plan's representative-gain/scaling criterion. The tradeoff is explicit:
small-group means are approximately 0.5–3.2% higher in the final repeat, with
no small-group improvement claimed. The initial always-hash implementation's
6–11% small-group slowdown was rejected. No additional optimization was retained
solely on noisy first-launch observations. Formatting, core/differential tests,
and source/documentation review pass. The final review independently checked all
96 case records across the six timing series. Committed as `5247ad1`.
