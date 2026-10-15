fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    insert_benchmark(criterion, &runtime, &mut connections);
    select_benchmark(criterion, &runtime, &mut connections);
    benchmark_snapshots(criterion);
    benchmark_lookup_scaling(criterion, &runtime, &mut connections);
    benchmark_populated_table_writes(criterion, &runtime, &mut connections);
    concurrency_benchmark(criterion, &runtime);
    benchmark_membership_joins(criterion, &runtime, &mut connections);
}
