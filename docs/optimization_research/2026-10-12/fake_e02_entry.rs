fn benchmarks(criterion: &mut Criterion) {
    benchmark_selective_outer_joins(criterion, &[10, 100, 500, 1_000, 10_000]);
}
