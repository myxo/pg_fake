fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let db = Db::create();
    let mut first = PgFakeConnection::new(db.clone());
    let mut second = PgFakeConnection::new(db);
    let mut connections = [(
        "pg_fake",
        BenchmarkConnection::PgFake(&mut first),
        BenchmarkConnection::PgFake(&mut second),
    )];
    benchmark_serializable_connections(criterion, &runtime, &mut connections);
}
