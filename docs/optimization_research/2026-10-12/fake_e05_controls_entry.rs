fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    select_benchmark(criterion, &runtime, &mut connections);
    core_vs_sqlx_benchmark(criterion, &runtime);
    benchmark_lookup_scaling(criterion, &runtime, &mut connections, None);
    inner_join_benchmark(criterion, &runtime, &mut connections);
    benchmark_selective_indexed_join(criterion, &runtime, &mut connections, None);
    concurrency_benchmark(criterion, &runtime);
}
