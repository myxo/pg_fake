use std::{
    env,
    fmt::Write,
    path::{Path, PathBuf},
    process::Command,
};

use pg_fake_benchmarks::{Benchmark, BenchmarkValue, list_benchmarks};

fn main() {
    let commands = env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(commands, ["bench"], "usage: cargo x bench");
    run_benchmarks();
    print_report();
}

fn run_benchmarks() {
    let status = Command::new("cargo")
        .args(["bench", "-p", "pg_fake_sqlx", "--bench", "workloads"])
        .status()
        .expect("benchmark command must start");
    assert!(status.success(), "benchmark command must succeed");
}

fn print_report() {
    let root = find_criterion_root();
    let benchmarks = list_benchmarks();
    let mut measurements = Vec::new();
    let mut speedups = Vec::new();

    for benchmark in &benchmarks {
        for value in &benchmark.values {
            if let Some(average) = read_estimate(&root, benchmark, value) {
                measurements.push((
                    format!("{}/{}", benchmark.name, value.name),
                    format_time(average),
                ));
            }
        }
        for comparison in &benchmark.comparisons {
            let baseline = find_value(benchmark, comparison.baseline);
            let candidate = find_value(benchmark, comparison.candidate);
            let (Some(baseline), Some(candidate)) = (
                read_estimate(&root, benchmark, baseline),
                read_estimate(&root, benchmark, candidate),
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
    println!("\nBenchmarks\n");
    print_table(
        &["benchmark", "average"],
        &measurements
            .iter()
            .map(|(name, average)| vec![name.clone(), average.clone()])
            .collect::<Vec<_>>(),
    );
    if !speedups.is_empty() {
        println!("\nSpeedups\n");
        print_table(
            &["benchmark", "baseline", "candidate", "relative"],
            &speedups
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

fn find_criterion_root() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
        .join("criterion")
}

fn read_estimate(root: &Path, benchmark: &Benchmark, value: &BenchmarkValue) -> Option<f64> {
    let path = root
        .join(benchmark.name)
        .join(value.path.iter().collect::<PathBuf>())
        .join("new/estimates.json");
    let estimate = std::fs::read_to_string(path).ok()?;
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

fn format_relative(baseline: f64, candidate: f64) -> String {
    let ratio = candidate / baseline;
    if ratio > 1.0 {
        return format!("{ratio:.2}x slower");
    }
    if ratio < 1.0 {
        return format!("{:.2}x faster", 1.0 / ratio);
    }
    "same".to_owned()
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
