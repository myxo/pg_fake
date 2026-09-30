# E13: bound-DML companion measurements

This experiment measures existing parameterized execution; it adds a diagnostic
harness, not a new write executor. Inline SQL remains measured separately.
Build with `cargo build -p pg_fake_sqlx --release --example dml_binding_probe`.
Run `target/release/examples/dml_binding_probe` three times sequentially, with no
other compilation, tests, profiling, or benchmarking. These runs followed the
intermediate adaptive grouping measurements; grouping is not used by this probe.

macOS arm64, Cargo 1.98.1, default features, no execution tracing. Each API and
inline/bound variant owns a fresh database. INSERT grows from one warm row to
1,001 rows; UPDATE changes a single existing row. Values change on every call.
Each native/autocommit and SQLx/autocommit case has 1,000 timed operations.
SQLx uses default persistent caching; changing inline SQL causes misses, while
stable bound SQL reuses its cached statement. Native preparation and the first
execution are outside timing. Each pair executes the same logical writes and
retains the same row/history growth. This is a comparison of two existing API
usage patterns, not a before/after source-optimization claim.

```sql
INSERT INTO t VALUES ($1, $1);
UPDATE t SET value = $1 WHERE id = 0;
```

Microseconds per operation.

| Case | Three runs | Mean |
| --- | --- | ---: |
| native update=false bound=false | 25.883, 13.962, 13.972 | 17.939 |
| SQLx update=false bound=false persistent=true | 38.574, 40.304, 40.059 | 39.646 |
| native update=false bound=true | 12.430, 12.006, 11.950 | 12.129 |
| SQLx update=false bound=true persistent=true | 28.532, 28.676, 28.619 | 28.609 |
| native update=true bound=false | 20.887, 21.015, 21.105 | 21.002 |
| SQLx update=true bound=false persistent=true | 46.934, 47.451, 47.303 | 47.229 |
| native update=true bound=true | 17.950, 17.960, 18.036 | 17.982 |
| SQLx update=true bound=true persistent=true | 37.435, 37.507, 37.473 | 37.472 |

| API | Operation | Inline mean | Bound mean | Reduction |
| --- | --- | ---: | ---: | ---: |
| native | INSERT | 17.939 | 12.129 | 32.4% |
| native | UPDATE | 21.002 | 17.982 | 14.4% |
| SQLx | INSERT | 39.646 | 28.609 | 27.8% |
| SQLx | UPDATE | 47.229 | 37.472 | 20.7% |

Prepare / first-execution observations (µs), diagnostic only. The canonical bound
statement is prepared outside timing even in the native inline case.

| Case | Three prepare / first observations |
| --- | --- |
| native update=false bound=false | 2359.583 / 755.875; 84.750 / 47.417; 73.750 / 42.083 |
| native update=false bound=true | 24.750 / 53.208; 19.000 / 19.875; 14.583 / 21.584 |
| native update=true bound=false | 339.709 / 339.709; 54.583 / 75.125; 48.125 / 70.667 |
| native update=true bound=true | 14.750 / 34.959; 14.250 / 24.584; 14.292 / 24.291 |

Native inline INSERT has a cold-run outlier (25.883 µs versus approximately
13.97 µs in the other two runs), so its 32.4% mean reduction is noisy and must
not be presented as a stable speedup. The SQLx observations are consistent.

The probe compiles and all three runs succeed. Independent harness review found
no blocking issue; the native INSERT variability is disclosed above.
Committed as `c072977`.
