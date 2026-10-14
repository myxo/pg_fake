fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    update_benchmark(criterion, &runtime, &mut connections);
    benchmark_lookup_scaling(criterion, &runtime, &mut connections);
    benchmark_populated_table_writes(criterion, &runtime, &mut connections);
}
