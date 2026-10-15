fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    migration_data_transform_benchmark(criterion, &runtime, &mut connections);
    benchmark_snapshots(criterion);
    benchmark_membership_joins(criterion, &runtime, &mut connections);
    set_operation_benchmark(criterion, &runtime, &mut connections);
    let db = Db::create();
    let mut first = PgFakeConnection::new(db.clone());
    let mut second = PgFakeConnection::new(db);
    let mut serializable = [(
        "pg_fake",
        BenchmarkConnection::PgFake(&mut first),
        BenchmarkConnection::PgFake(&mut second),
    )];
    benchmark_serializable_connections(criterion, &runtime, &mut serializable);
}
