#[derive(Clone)]
pub struct Benchmark {
    pub name: &'static str,
    pub values: Vec<BenchmarkValue>,
    pub comparisons: Vec<BenchmarkComparison>,
}

#[derive(Clone)]
pub struct BenchmarkValue {
    pub name: &'static str,
    pub path: &'static [&'static str],
}

#[derive(Clone)]
pub struct BenchmarkComparison {
    pub baseline: &'static str,
    pub candidate: &'static str,
}

pub fn find_benchmark(name: &str) -> Benchmark {
    list_benchmarks()
        .into_iter()
        .find(|benchmark| benchmark.name == name)
        .expect("benchmark must be registered")
}

pub fn list_benchmarks() -> Vec<Benchmark> {
    vec![
        build_benchmark("create_table", postgres_values(), postgres_comparisons()),
        build_benchmark("insert_row", postgres_values(), postgres_comparisons()),
        build_benchmark(
            "insert_row_returning",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "insert_row_with_defaults",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark("update_row", postgres_values(), postgres_comparisons()),
        build_benchmark("update_from_row", postgres_values(), postgres_comparisons()),
        build_benchmark("delete_row", postgres_values(), postgres_comparisons()),
        build_benchmark(
            "sequence_nextval",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "transaction_insert",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "transaction_repeatable_read_select_for_update",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark("select_100_rows", postgres_values(), postgres_comparisons()),
        build_benchmark(
            "limit_offset_ordered_100_rows",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "order_by_100_rows",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "adapter_overhead_select_100_rows",
            vec![value("core", &["core"]), value("sqlx", &["sqlx"])],
            vec![comparison("core", "sqlx")],
        ),
        build_benchmark(
            "core_parsed_vs_prepared_point_select",
            vec![
                value("parse_and_analyze", &["parse_and_analyze"]),
                value("prepared_reuse", &["prepared_reuse"]),
            ],
            vec![comparison("parse_and_analyze", "prepared_reuse")],
        ),
        build_benchmark(
            "transaction_history_point_select",
            vec![
                value("1", &["1"]),
                value("100", &["100"]),
                value("10,000", &["10000"]),
                value("100,000", &["100000"]),
            ],
            vec![
                comparison("1", "100"),
                comparison("1", "10,000"),
                comparison("1", "100,000"),
            ],
        ),
        build_benchmark(
            "mvcc_old_snapshot_read",
            vec![
                value("1", &["1"]),
                value("100", &["100"]),
                value("10,000", &["10000"]),
            ],
            vec![comparison("1", "100"), comparison("1", "10,000")],
        ),
        build_benchmark(
            "point_lookup_index_vs_scan",
            vec![
                value("heap_scan/100", &["heap_scan", "100"]),
                value("unique_index/100", &["unique_index", "100"]),
                value("heap_scan/10,000", &["heap_scan", "10000"]),
                value("unique_index/10,000", &["unique_index", "10000"]),
            ],
            vec![
                comparison("heap_scan/100", "unique_index/100"),
                comparison("heap_scan/10,000", "unique_index/10,000"),
            ],
        ),
        build_benchmark(
            "concurrent_uncontended_reads",
            vec![
                value("sequential", &["sequential"]),
                value("parallel", &["parallel"]),
            ],
            vec![comparison("sequential", "parallel")],
        ),
        build_benchmark(
            "concurrent_same_row_contention",
            vec![value("wait_then_rollback", &["wait_then_rollback"])],
            vec![],
        ),
        build_benchmark(
            "foreign_key_insert",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "selective_inner_join",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "many_match_inner_join",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "derived_and_scalar_subquery_100_rows",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "correlated_exists_100_rows",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "global_aggregate_100_rows",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "grouped_aggregate_100_rows",
            postgres_values(),
            postgres_comparisons(),
        ),
        build_benchmark(
            "select_distinct_100_rows",
            postgres_values(),
            postgres_comparisons(),
        ),
    ]
}

fn build_benchmark(
    name: &'static str,
    values: Vec<BenchmarkValue>,
    comparisons: Vec<BenchmarkComparison>,
) -> Benchmark {
    Benchmark {
        name,
        values,
        comparisons,
    }
}

fn postgres_values() -> Vec<BenchmarkValue> {
    vec![
        value("pg_fake", &["pg_fake"]),
        value("postgres_18", &["postgres_18"]),
    ]
}

fn postgres_comparisons() -> Vec<BenchmarkComparison> {
    vec![comparison("postgres_18", "pg_fake")]
}

fn value(name: &'static str, path: &'static [&'static str]) -> BenchmarkValue {
    BenchmarkValue { name, path }
}

fn comparison(baseline: &'static str, candidate: &'static str) -> BenchmarkComparison {
    BenchmarkComparison {
        baseline,
        candidate,
    }
}
