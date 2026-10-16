fn benchmarks(criterion: &mut Criterion) {
    benchmark_partial_index_maintenance(criterion);
    benchmark_bounded_on_conflict(criterion);
}
