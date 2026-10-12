fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    insert_benchmark(criterion, &runtime, &mut connections);
    update_benchmark(criterion, &runtime, &mut connections);
    transaction_benchmark(criterion, &runtime, &mut connections);
    select_benchmark(criterion, &runtime, &mut connections);
    order_by_benchmark(criterion, &runtime, &mut connections);
    inner_join_benchmark(criterion, &runtime, &mut connections);
    benchmark_bound_writes(criterion, &runtime, &mut connections);
    benchmark_populated_table_writes(criterion, &runtime, &mut connections);
    foreign_key_insert_benchmark(criterion, &runtime, &mut connections);
    benchmark_jsonb(criterion, &runtime, &mut connections);
    migration_data_transform_benchmark(criterion, &runtime, &mut connections);
    grouped_aggregate_benchmark(criterion, &runtime, &mut connections);
    set_operation_benchmark(criterion, &runtime, &mut connections);
    benchmark_membership_joins(criterion, &runtime, &mut connections);
    core_vs_sqlx_benchmark(criterion, &runtime);
}
