use std::{
    collections::BTreeSet,
    env,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use pg_fake_benchmarks::{Benchmark, BenchmarkValue, list_benchmarks};

const BASELINE: &str = "repo-baseline";
const BASELINE_FILES: [&str; 4] = [
    "benchmark.json",
    "estimates.json",
    "sample.json",
    "tukey.json",
];

struct Report {
    measurements: Vec<(String, String, String)>,
    speedups: Vec<(String, String, String, String)>,
}

fn main() {
    let commands = env::args().skip(1).collect::<Vec<_>>();
    let record = match commands.as_slice() {
        [command] if command == "bench" => false,
        [command, action] if command == "bench" && action == "record" => true,
        _ => panic!("usage: cargo x bench [record]"),
    };

    let environment = collect_environment();
    print_environment(&environment);
    restore_baseline(record);
    run_benchmarks(record);

    let report = collect_report(&find_criterion_root(), "new");
    print_report(&report);
    if record {
        save_results(&environment, &report);
        println!(
            "\nSaved benchmark results to {}",
            find_results_root().display()
        );
    }
}

fn collect_environment() -> Vec<(String, String)> {
    let mut environment = vec![
        ("recorded_at".to_owned(), find_timestamp()),
        ("os".to_owned(), env::consts::OS.to_owned()),
        ("architecture".to_owned(), env::consts::ARCH.to_owned()),
        (
            "os_version".to_owned(),
            run_command("uname", &["-sr"]).unwrap_or_else(|| "unknown".to_owned()),
        ),
        (
            "cpu".to_owned(),
            find_cpu_model().unwrap_or_else(|| "unknown".to_owned()),
        ),
        (
            "logical_cpus".to_owned(),
            std::thread::available_parallelism()
                .map(|count| count.get().to_string())
                .unwrap_or_else(|_| "unknown".to_owned()),
        ),
    ];
    if let Some(physical_cores) = find_physical_cores() {
        environment.push(("physical_cores".to_owned(), physical_cores));
    }
    if let Some(performance_levels) = find_performance_levels() {
        environment.push(("performance_levels".to_owned(), performance_levels));
    }
    environment.extend([
        (
            "rust".to_owned(),
            run_command("rustc", &["--version"]).unwrap_or_else(|| "unknown".to_owned()),
        ),
        ("postgres_target".to_owned(), "18".to_owned()),
        ("criterion".to_owned(), "0.5".to_owned()),
    ]);
    environment
}

fn find_timestamp() -> String {
    run_command("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_else(|| "unknown".to_owned())
}

fn find_cpu_model() -> Option<String> {
    if env::consts::OS == "macos" {
        return run_command("sysctl", &["-n", "machdep.cpu.brand_string"])
            .or_else(|| run_command("sysctl", &["-n", "hw.model"]));
    }
    if env::consts::OS == "linux" {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
        return cpuinfo.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            matches!(name.trim(), "model name" | "Hardware" | "Processor")
                .then(|| value.trim().to_owned())
        });
    }
    None
}

fn find_physical_cores() -> Option<String> {
    if env::consts::OS == "macos" {
        return run_command("sysctl", &["-n", "hw.physicalcpu"]);
    }
    if env::consts::OS == "linux" {
        let cores = run_command("lscpu", &["-p=SOCKET,CORE,ONLINE"])?
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let mut values = line.split(',');
                let socket = values.next()?;
                let core = values.next()?;
                let online = values.next()?;
                (online == "Y").then(|| (socket.to_owned(), core.to_owned()))
            })
            .collect::<BTreeSet<_>>();
        return Some(cores.len().to_string());
    }
    None
}

fn find_performance_levels() -> Option<String> {
    if env::consts::OS != "macos" {
        return None;
    }
    let count = run_command("sysctl", &["-n", "hw.nperflevels"])?
        .parse::<usize>()
        .ok()?;
    let levels = (0..count)
        .filter_map(|level| {
            let physical = run_command(
                "sysctl",
                &["-n", &format!("hw.perflevel{level}.physicalcpu")],
            )?;
            let logical = run_command(
                "sysctl",
                &["-n", &format!("hw.perflevel{level}.logicalcpu")],
            )?;
            Some(format!(
                "level {level}: {physical} physical / {logical} logical"
            ))
        })
        .collect::<Vec<_>>();
    (!levels.is_empty()).then(|| levels.join("; "))
}

fn run_command(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|output| !output.is_empty())
}

fn restore_baseline(record: bool) {
    let criterion_root = find_criterion_root();
    let results_root = find_results_root().join("criterion");
    let benchmarks = list_benchmarks();

    for benchmark in &benchmarks {
        for value in &benchmark.values {
            let target = find_baseline_path(&criterion_root, benchmark, value);
            if target.exists() {
                fs::remove_dir_all(&target).expect("old target baseline must be removable");
            }
        }
    }

    if !results_root.exists() {
        assert!(
            record,
            "no committed benchmark baseline exists; run `cargo x bench record` first"
        );
        return;
    }

    for benchmark in &benchmarks {
        for value in &benchmark.values {
            let source = find_baseline_path(&results_root, benchmark, value);
            let target = find_baseline_path(&criterion_root, benchmark, value);
            if record && !has_complete_baseline(&source) {
                continue;
            }
            copy_baseline(&source, &target);
        }
    }
}

fn run_benchmarks(record: bool) {
    let argument = if record {
        "--save-baseline"
    } else {
        "--baseline"
    };
    let status = Command::new("cargo")
        .args([
            "bench",
            "-p",
            "pg_fake_benchmarks",
            "--bench",
            "workloads",
            "--",
            argument,
            BASELINE,
            "--noplot",
        ])
        .status()
        .expect("benchmark command must start");
    assert!(status.success(), "benchmark command must succeed");
}

fn collect_report(root: &Path, result: &str) -> Report {
    let benchmarks = list_benchmarks();
    let previous_root = find_results_root().join("criterion");
    let (postgres_benchmarks, other_benchmarks): (Vec<_>, Vec<_>) =
        benchmarks.iter().partition(|benchmark| {
            benchmark
                .values
                .iter()
                .any(|value| value.name == "postgres_18")
        });
    let mut measurements = Vec::new();
    let mut speedups = Vec::new();

    for benchmark in postgres_benchmarks.into_iter().chain(other_benchmarks) {
        for value in &benchmark.values {
            if let Some(average) = read_estimate(root, benchmark, value, result) {
                let change = read_estimate(&previous_root, benchmark, value, BASELINE).map_or_else(
                    || "N/A".to_owned(),
                    |previous| format_change(previous, average),
                );
                measurements.push((
                    format!("{}/{}", benchmark.name, value.name),
                    format_time(average),
                    change,
                ));
            }
        }
        for comparison in &benchmark.comparisons {
            let baseline = find_value(benchmark, comparison.baseline);
            let candidate = find_value(benchmark, comparison.candidate);
            let (Some(baseline), Some(candidate)) = (
                read_estimate(root, benchmark, baseline, result),
                read_estimate(root, benchmark, candidate, result),
            ) else {
                continue;
            };
            speedups.push((
                benchmark.name.to_owned(),
                comparison.baseline.to_owned(),
                comparison.candidate.to_owned(),
                format_relative(baseline, candidate),
            ));
        }
    }

    assert!(
        !measurements.is_empty(),
        "no Criterion estimates were found"
    );
    Report {
        measurements,
        speedups,
    }
}

fn print_environment(environment: &[(String, String)]) {
    println!("Environment\n");
    print_table(
        &["property", "value"],
        &environment
            .iter()
            .map(|(property, value)| vec![property.clone(), value.clone()])
            .collect::<Vec<_>>(),
    );
    println!();
}

fn print_report(report: &Report) {
    println!("\nBenchmarks\n");
    print_table(
        &["benchmark", "average", "change vs previous"],
        &report
            .measurements
            .iter()
            .map(|(name, average, change)| vec![name.clone(), average.clone(), change.clone()])
            .collect::<Vec<_>>(),
    );
    if !report.speedups.is_empty() {
        println!("\nSpeedups\n");
        print_table(
            &["benchmark", "baseline", "candidate", "relative"],
            &report
                .speedups
                .iter()
                .map(|(name, baseline, candidate, relative)| {
                    vec![
                        name.clone(),
                        baseline.clone(),
                        candidate.clone(),
                        relative.clone(),
                    ]
                })
                .collect::<Vec<_>>(),
        );
    }
}

fn save_results(environment: &[(String, String)], report: &Report) {
    let results_root = find_results_root();
    let stored_criterion_root = results_root.join("criterion");
    if stored_criterion_root.exists() {
        fs::remove_dir_all(&stored_criterion_root)
            .expect("old committed baseline must be removable");
    }

    let criterion_root = find_criterion_root();
    for benchmark in list_benchmarks() {
        for value in &benchmark.values {
            let source = find_baseline_path(&criterion_root, &benchmark, value);
            let target = find_baseline_path(&stored_criterion_root, &benchmark, value);
            copy_baseline(&source, &target);
        }
    }

    fs::create_dir_all(&results_root).expect("results directory must be creatable");
    let environment = environment
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
        .collect::<serde_json::Map<_, _>>();
    fs::write(
        results_root.join("environment.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&environment).expect("environment must serialize as JSON")
        ),
    )
    .expect("environment report must be writable");
    fs::write(
        results_root.join("report.md"),
        format_markdown(environment, report),
    )
    .expect("benchmark report must be writable");
}

fn format_markdown(
    environment: serde_json::Map<String, serde_json::Value>,
    report: &Report,
) -> String {
    let mut markdown = String::from("# Benchmark results\n\n## Environment\n\n");
    markdown.push_str("| Property | Value |\n| --- | --- |\n");
    for (property, value) in environment {
        writeln!(
            markdown,
            "| {} | {} |",
            property.replace('|', "\\|"),
            value.as_str().unwrap().replace('|', "\\|")
        )
        .unwrap();
    }
    markdown.push_str(
        "\n## Benchmarks\n\n| Benchmark | Average | Change vs previous |\n| --- | ---: | ---: |\n",
    );
    for (name, average, change) in &report.measurements {
        writeln!(markdown, "| {name} | {average} | {change} |").unwrap();
    }
    if !report.speedups.is_empty() {
        markdown.push_str(
            "\n## Comparisons\n\n| Benchmark | Baseline | Candidate | Relative |\n| --- | --- | --- | ---: |\n",
        );
        for (name, baseline, candidate, relative) in &report.speedups {
            writeln!(
                markdown,
                "| {name} | {baseline} | {candidate} | {relative} |"
            )
            .unwrap();
        }
    }
    markdown
}

fn copy_baseline(source: &Path, target: &Path) {
    assert!(
        has_complete_baseline(source),
        "baseline is incomplete: {}",
        source.display()
    );
    fs::create_dir_all(target).expect("baseline directory must be creatable");
    for file in BASELINE_FILES {
        let source = source.join(file);
        fs::copy(&source, target.join(file)).expect("baseline file must be copied");
    }
}

fn has_complete_baseline(path: &Path) -> bool {
    BASELINE_FILES.iter().all(|file| path.join(file).is_file())
}

fn find_results_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be inside the workspace crates directory")
        .join("pg_fake_benchmarks/results")
}

fn find_criterion_root() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .expect("xtask must be inside the workspace")
                .join("target")
        })
        .join("criterion")
}

fn find_baseline_path(root: &Path, benchmark: &Benchmark, value: &BenchmarkValue) -> PathBuf {
    root.join(benchmark.name)
        .join(value.path.iter().collect::<PathBuf>())
        .join(BASELINE)
}

fn read_estimate(
    root: &Path,
    benchmark: &Benchmark,
    value: &BenchmarkValue,
    result: &str,
) -> Option<f64> {
    let path = root
        .join(benchmark.name)
        .join(value.path.iter().collect::<PathBuf>())
        .join(result)
        .join("estimates.json");
    let estimate = fs::read_to_string(path).ok()?;
    Some(
        serde_json::from_str::<serde_json::Value>(&estimate)
            .expect("Criterion estimate must contain valid JSON")["mean"]["point_estimate"]
            .as_f64()
            .expect("Criterion estimate must contain a mean point estimate"),
    )
}

fn find_value<'a>(benchmark: &'a Benchmark, name: &str) -> &'a BenchmarkValue {
    benchmark
        .values
        .iter()
        .find(|value| value.name == name)
        .expect("comparison value must be registered")
}

fn format_time(nanoseconds: f64) -> String {
    if nanoseconds < 1_000.0 {
        return format!("{nanoseconds:.2} ns");
    }
    if nanoseconds < 1_000_000.0 {
        return format!("{:.2} us", nanoseconds / 1_000.0);
    }
    if nanoseconds < 1_000_000_000.0 {
        return format!("{:.2} ms", nanoseconds / 1_000_000.0);
    }
    format!("{:.2} s", nanoseconds / 1_000_000_000.0)
}

fn format_change(previous: f64, current: f64) -> String {
    assert!(
        previous > 0.0,
        "previous benchmark estimate must be positive"
    );
    let change = (current - previous) / previous * 100.0;
    if change.abs() < 0.005 {
        return "0.00%".to_owned();
    }
    format!("{change:+.2}%")
}

fn format_relative(baseline: f64, candidate: f64) -> String {
    let ratio = candidate / baseline;
    if ratio > 1.0 {
        return format!("🔴 ↓ {ratio:.2}x");
    }
    if ratio < 1.0 {
        return format!("🟢 ↑ {:.2}x", 1.0 / ratio);
    }
    "⚪ → same".to_owned()
}

fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }
    println!("{}", format_row(headers, &widths));
    println!(
        "{}",
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  ")
    );
    for row in rows {
        println!("{}", format_row(row, &widths));
    }
}

fn format_row(values: &[impl AsRef<str>], widths: &[usize]) -> String {
    let mut row = String::new();
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            row.push_str("  ");
        }
        if index == 0 {
            write!(row, "{:<width$}", value.as_ref(), width = widths[index]).unwrap();
        } else {
            write!(row, "{:>width$}", value.as_ref(), width = widths[index]).unwrap();
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::{
        BASELINE, collect_report, find_results_root, format_change, format_markdown,
        format_relative, has_complete_baseline,
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
}
