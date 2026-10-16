fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    select_benchmark(criterion, &runtime, &mut connections);
    benchmark_bounded_on_conflict(criterion);
    benchmark_bounded_indexed_update(criterion);
}
