use super::{
    BASELINE, collect_report, find_results_root, format_change, format_markdown, format_relative,
    has_complete_baseline, select_benchmarks,
};
use pg_fake_benchmarks::{BenchmarkTier, list_benchmarks};

#[test]
fn formats_benchmark_change_as_percentage() {
    assert_eq!(format_change(100.0, 125.0), "+25.00%");
    assert_eq!(format_change(100.0, 75.0), "-25.00%");
    assert_eq!(format_change(100.0, 100.0), "0.00%");
}

#[test]
fn formats_relative_timing_with_colored_arrows() {
    assert_eq!(format_relative(100.0, 125.0), "🔴 ↓ 1.25x");
    assert_eq!(format_relative(100.0, 50.0), "🟢 ↑ 2.00x");
    assert_eq!(format_relative(100.0, 100.0), "⚪ → same");
}

#[test]
fn detects_complete_baselines() {
    let results = find_results_root().join("criterion");

    assert!(has_complete_baseline(
        &results.join("tier2_create_table/pg_fake/repo-baseline")
    ));
    assert!(!has_complete_baseline(&find_results_root()));
}

#[test]
fn reports_change_from_committed_measurements() {
    let root = find_results_root().join("criterion");
    let benchmarks = list_benchmarks();
    let report = collect_report(&root, BASELINE, &benchmarks);

    assert!(
        report
            .tiers
            .iter()
            .flat_map(|tier| &tier.measurements)
            .all(|(_, _, change)| change == "0.00%")
    );
    assert_eq!(
        report
            .tiers
            .iter()
            .map(|tier| tier.measurements.len())
            .sum::<usize>(),
        benchmarks
            .iter()
            .flat_map(|benchmark| { benchmark.values.iter().map(move |value| (benchmark, value)) })
            .filter(|(benchmark, value)| {
                has_complete_baseline(&super::find_baseline_path(&root, benchmark, value))
            })
            .count()
    );
    assert_eq!(
        report
            .tiers
            .iter()
            .map(|tier| tier.tier)
            .collect::<Vec<_>>(),
        BenchmarkTier::list()
    );
    let markdown = format_markdown(serde_json::Map::new(), &report);
    assert!(markdown.contains("| Benchmark | Average | Change vs previous |"));
    for tier in BenchmarkTier::list() {
        assert!(markdown.contains(&format!("## {}", tier.get_title())));
    }
}

#[test]
fn filters_reports_by_tier_without_including_other_stored_results() {
    let root = find_results_root().join("criterion");
    for tier in BenchmarkTier::list() {
        let filter = format!("{}_", tier.get_prefix());
        let benchmarks = select_benchmarks(Some(&filter));
        let report = collect_report(&root, BASELINE, &benchmarks);
        assert_eq!(report.tiers.len(), 1);
        assert_eq!(report.tiers[0].tier, tier);
        assert!(
            report.tiers[0]
                .measurements
                .iter()
                .all(|(name, _, _)| name.starts_with(&filter))
        );
        assert!(
            report.tiers[0]
                .speedups
                .iter()
                .all(|(name, _, _, _)| name.starts_with(&filter))
        );
    }
}

#[test]
fn excludes_comparisons_when_only_one_backend_is_selected() {
    let root = find_results_root().join("criterion");
    let benchmarks = select_benchmarks(Some("tier1_insert_row/pg_fake"));
    let report = collect_report(&root, BASELINE, &benchmarks);
    assert_eq!(report.tiers.len(), 1);
    assert_eq!(report.tiers[0].measurements.len(), 1);
    assert_eq!(
        report.tiers[0].measurements[0].0,
        "tier1_insert_row/pg_fake"
    );
    assert!(report.tiers[0].speedups.is_empty());
}

#[test]
fn filters_parameterized_benchmarks_using_their_criterion_names() {
    let benchmarks = select_benchmarks(Some("tier2_point_lookup_index_vs_scan/heap_scan/10000"));
    let report = collect_report(
        &find_results_root().join("criterion"),
        BASELINE,
        &benchmarks,
    );
    assert_eq!(report.tiers[0].measurements.len(), 1);
    assert_eq!(
        report.tiers[0].measurements[0].0,
        "tier2_point_lookup_index_vs_scan/heap_scan/10,000"
    );
    assert!(report.tiers[0].speedups.is_empty());
}

#[test]
#[should_panic(expected = "benchmark filter matched no benchmarks")]
fn rejects_unknown_filters() {
    select_benchmarks(Some("tier4_"));
}

#[test]
#[should_panic(expected = "filter must be a literal benchmark name fragment")]
fn rejects_regex_filters() {
    select_benchmarks(Some("tier[12]_"));
}

#[test]
fn preserves_recorded_baselines_under_tiered_names() {
    let root = find_results_root().join("criterion");
    for benchmark in list_benchmarks() {
        for value in &benchmark.values {
            let path = super::find_baseline_path(&root, &benchmark, value);
            if !path.exists() {
                continue;
            }
            assert!(has_complete_baseline(&path), "{}", path.display());
            let metadata: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(path.join("benchmark.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(metadata["group_id"], benchmark.format_name());
            assert_eq!(
                metadata["full_id"],
                format!("{}/{}", benchmark.format_name(), value.path.join("/"))
            );
        }
    }
}
