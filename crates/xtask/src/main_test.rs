
use super::{
    BASELINE, collect_report, find_results_root, format_change, format_markdown, format_relative,
    has_complete_baseline,
};

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
        &results.join("create_table/pg_fake/repo-baseline")
    ));
    assert!(!has_complete_baseline(&find_results_root()));
}

#[test]
fn reports_change_from_committed_measurements() {
    let root = find_results_root().join("criterion");
    let report = collect_report(&root, BASELINE);

    assert!(
        report
            .measurements
            .iter()
            .all(|(_, _, change)| change == "0.00%")
    );
    assert!(
        format_markdown(serde_json::Map::new(), &report)
            .contains("| Benchmark | Average | Change vs previous |")
    );
}
