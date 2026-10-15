fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    benchmark_jsonb(criterion, &runtime, &mut connections);
    parsed_vs_prepared_benchmark(criterion);
    mvcc_version_chain_benchmark(criterion);
    indexed_vs_scan_benchmark(criterion);
    benchmark_lookup_scaling(criterion, &runtime, &mut connections);
    benchmark_membership_joins(criterion, &runtime, &mut connections);
    benchmark_bound_writes(criterion, &runtime, &mut connections);
}
