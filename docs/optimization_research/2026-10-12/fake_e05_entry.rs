fn benchmarks(criterion: &mut Criterion) {
    benchmark_partial_index_access(criterion, &[10, 100, 1_000, 10_000]);
}
