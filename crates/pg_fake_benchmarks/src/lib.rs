#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkTier {
    Essential,
    Important,
    Rare,
}

impl BenchmarkTier {
    pub fn list() -> [Self; 3] {
        [Self::Essential, Self::Important, Self::Rare]
    }

    pub fn get_prefix(self) -> &'static str {
        match self {
            Self::Essential => "tier1",
            Self::Important => "tier2",
            Self::Rare => "tier3",
        }
    }

    pub fn get_title(self) -> &'static str {
        match self {
            Self::Essential => "Tier 1: essential operations",
            Self::Important => "Tier 2: important operations",
            Self::Rare => "Tier 3: rare operations and diagnostics",
        }
    }
}

#[derive(Clone)]
pub struct Benchmark {
    pub tier: BenchmarkTier,
    pub name: &'static str,
    pub values: Vec<BenchmarkValue>,
    pub comparisons: Vec<BenchmarkComparison>,
}

impl Benchmark {
    pub fn format_name(&self) -> String {
        format!("{}_{}", self.tier.get_prefix(), self.name)
    }
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
        build_benchmark(
            BenchmarkTier::Important,
            "core_snapshot_100_rows",
            vec![build_value("pg_fake", &["pg_fake"])],
            vec![],
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "session_settings_roundtrip",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "transaction_local_guc_roundtrip",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "nested_savepoint_release",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "nested_savepoint_rollback",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "skip_locked_queue_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "serializable_uncontended_read",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "serializable_write_skew",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "lateral_latest_per_parent_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "runtime_temporal_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "runtime_patterns_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "create_table",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "transactional_ddl_create_rollback",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "migration_table_lock_two_relations",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "sqlx_migration_chain",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "procedural_trigger_insert_update",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "alter_table_rewrite_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "partial_unique_index_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "temporary_table_on_commit_drop",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "insert_row",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "insert_row_returning",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "insert_row_with_defaults",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "insert_on_conflict_do_nothing",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "insert_on_conflict_conflict_free",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "insert_on_conflict_do_update",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "update_row",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "update_from_row",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "delete_row",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "sequence_nextval",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "catalog_regclass_lookup",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "serial_identity_insert",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "uuid_temporal_select",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "offset_datetime_bind_store_fetch",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "bigint_uuid_array_bind_store_fetch",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "uuid_any_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "ordered_filtered_array_agg_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "array_containment_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "correlated_unnest_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "hashed_advisory_lock_acquisition",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "json_insert_returning",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "jsonb_insert_returning",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "jsonb_extraction",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "jsonb_containment",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "jsonb_join_group",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "window_row_number_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "window_rank_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "window_offset_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "window_moving_aggregate_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "ordered_string_agg_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "transaction_insert",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "transaction_repeatable_read_select_for_update",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "select_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "select_where_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "select_where_indexed_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "limit_offset_ordered_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "nested_filtered_view_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "order_by_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "adapter_overhead_select_100_rows",
            vec![
                build_value("core", &["core"]),
                build_value("sqlx", &["sqlx"]),
            ],
            vec![build_comparison("core", "sqlx")],
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "core_parsed_vs_prepared_point_select",
            vec![
                build_value("parse_and_analyze", &["parse_and_analyze"]),
                build_value("prepared_reuse", &["prepared_reuse"]),
            ],
            vec![build_comparison("parse_and_analyze", "prepared_reuse")],
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "transaction_history_point_select",
            vec![
                build_value("1", &["1"]),
                build_value("100", &["100"]),
                build_value("10,000", &["10000"]),
                build_value("100,000", &["100000"]),
            ],
            vec![
                build_comparison("1", "100"),
                build_comparison("1", "10,000"),
                build_comparison("1", "100,000"),
            ],
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "mvcc_old_snapshot_read",
            vec![
                build_value("1", &["1"]),
                build_value("100", &["100"]),
                build_value("10,000", &["10000"]),
            ],
            vec![
                build_comparison("1", "100"),
                build_comparison("1", "10,000"),
            ],
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "point_lookup_index_vs_scan",
            vec![
                build_value("heap_scan/100", &["heap_scan", "100"]),
                build_value("unique_index/100", &["unique_index", "100"]),
                build_value("heap_scan/10,000", &["heap_scan", "10000"]),
                build_value("unique_index/10,000", &["unique_index", "10000"]),
            ],
            vec![
                build_comparison("heap_scan/100", "unique_index/100"),
                build_comparison("heap_scan/10,000", "unique_index/10,000"),
            ],
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "concurrent_uncontended_reads",
            vec![
                build_value("sequential", &["sequential"]),
                build_value("parallel", &["parallel"]),
            ],
            vec![build_comparison("sequential", "parallel")],
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "concurrent_same_row_contention",
            vec![build_value("wait_then_rollback", &["wait_then_rollback"])],
            vec![],
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "foreign_key_insert",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "selective_inner_join",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Essential,
            "many_match_inner_join",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "derived_and_scalar_subquery_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "materialized_cte_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "data_modifying_cte_update_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "recursive_cte_numeric_series_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Rare,
            "recursive_cte_branching_traversal_127_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "correlated_exists_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "global_aggregate_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "grouped_aggregate_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "select_distinct_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "union_all_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
        build_benchmark(
            BenchmarkTier::Important,
            "union_100_rows",
            build_postgres_values(),
            build_postgres_comparisons(),
        ),
    ]
}

fn build_benchmark(
    tier: BenchmarkTier,
    name: &'static str,
    values: Vec<BenchmarkValue>,
    comparisons: Vec<BenchmarkComparison>,
) -> Benchmark {
    Benchmark {
        tier,
        name,
        values,
        comparisons,
    }
}

fn build_postgres_values() -> Vec<BenchmarkValue> {
    vec![
        build_value("pg_fake", &["pg_fake"]),
        build_value("postgres_18", &["postgres_18"]),
    ]
}

fn build_postgres_comparisons() -> Vec<BenchmarkComparison> {
    vec![build_comparison("postgres_18", "pg_fake")]
}

fn build_value(name: &'static str, path: &'static [&'static str]) -> BenchmarkValue {
    BenchmarkValue { name, path }
}

fn build_comparison(baseline: &'static str, candidate: &'static str) -> BenchmarkComparison {
    BenchmarkComparison {
        baseline,
        candidate,
    }
}
