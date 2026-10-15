fn benchmarks(criterion: &mut Criterion) {
    benchmark_composite_prefix(criterion, &[100, 1_000, 10_000]);
}
