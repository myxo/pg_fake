# E7: bind prepared temporal expressions

Baseline: `c5adffb` plus the separately measured E11 default-sharing change.
The Git approval reviewer currently blocks commits, so these experiments remain
uncommitted. The baseline prepared executor has no scalar function nodes.

Reproduce with `cargo build -p pg_fake --release --example temporal_probe`, then
run `target/release/examples/temporal_probe` three times. Native API,
autocommit, default features, macOS arm64, Cargo 1.98.1. Each fixture contains
10, 100, or 1,000 integer rows. Setup is excluded; each repeated-execution case
has 1,000 iterations. The probe prints preparation and first execution
separately, then one-shot and prepared reuse. Those single first-operation
samples include process-cold effects and are diagnostic, not stable means.
No compilation or profiling overlaps the timing runs.

Baseline microseconds per operation:

| Rows | Ordered | API | Three runs |
| ---: | --- | --- | --- |
| 10 | no | one-shot | 121.957, 98.108, 98.290 |
| 10 | no | prepared | 83.115, 83.353, 83.041 |
| 10 | yes | one-shot | 102.025, 102.409, 99.987 |
| 10 | yes | prepared | 85.577, 84.168, 83.423 |
| 100 | no | one-shot | 626.759, 614.408, 610.147 |
| 100 | no | prepared | 605.122, 601.208, 596.339 |
| 100 | yes | one-shot | 620.891, 614.907, 613.677 |
| 100 | yes | prepared | 600.669, 599.004, 597.483 |
| 1,000 | no | one-shot | 5818.858, 5804.263, 5728.678 |
| 1,000 | no | prepared | 5806.221, 5815.680, 5722.338 |
| 1,000 | yes | one-shot | 5830.644, 5751.859, 5664.194 |
| 1,000 | yes | prepared | 5788.984, 5752.253, 5691.139 |

Implementation binds numeric casts, numeric arithmetic coercion and the existing
`floor`, `to_timestamp`, `date_trunc`, and `to_char` functions. Execution calls
the same scalar evaluators as the generic path; timezone is read at execution.
Typmod casts and unsupported argument shapes retain the fallback. Ordered
queries remain generic until E13 and serve as a regression sentinel here.

Independent review found and fixed unsupported FLOOR modifier admission and
constant-right Boolean evaluation ordering. The second review found no remaining
issue. Validation: 283 core library tests, five SQLx runtime differential tests,
seven migration-transform tests, and formatting pass. Regressions compare both
one-shot and prepared output against the generic path, including column metadata,
timezone changes, parameters, NULLs, and lazy errors. Clippy with `-D warnings`
reports existing `collapsible_match` in executor/mod.rs, `large_enum_variant`
in PreparedSource (unchanged large StreamedJoin fields), and `question_mark`
in query/select.rs; no cleanup is bundled into this experiment.

Final-source microseconds per operation (three independent process runs):

| Rows | Ordered | API | Three runs |
| ---: | --- | --- | --- |
| 10 | no | one-shot | 67.728, 45.325, 45.294 |
| 10 | no | prepared | 28.815, 28.049, 28.014 |
| 10 | yes | one-shot | 101.707, 101.051, 100.955 |
| 10 | yes | prepared | 84.455, 84.147, 83.808 |
| 100 | no | one-shot | 292.671, 292.590, 289.872 |
| 100 | no | prepared | 271.536, 271.863, 271.466 |
| 100 | yes | one-shot | 620.151, 619.788, 623.317 |
| 100 | yes | prepared | 604.371, 602.646, 606.502 |
| 1,000 | no | one-shot | 2743.506, 2740.407, 2736.849 |
| 1,000 | no | prepared | 2716.214, 2718.223, 2716.255 |
| 1,000 | yes | one-shot | 5827.350, 5769.214, 5795.333 |
| 1,000 | yes | prepared | 5818.290, 5810.278, 5791.547 |

Retain the implementation: warm prepared latency improves 83.170 → 28.293 µs
at 10 rows (66.0%), 600.890 → 271.622 µs at 100 rows (54.8%), and
5,781.413 → 2,716.897 µs at 1,000 rows (53.0%). One-shot 100-row execution
improves 617.105 → 291.711 µs (52.7%). Ordered queries remain within about
1% of their baseline and establish no gain. No SQLx speedup is claimed here;
the registered ordered temporal workload still needs E13's ordering extension.

For the 100-row unordered case, preparation samples were
59.084/60.667/46.292 µs before and 52.958/55.125/49.500 µs after;
first execution was 602.292/610.459/601.875 µs before and
270.500/270.708/275.583 µs after. These remain diagnostic samples,
not the repeated-execution acceptance measurements.
