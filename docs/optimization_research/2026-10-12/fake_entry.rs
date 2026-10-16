fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let mut connections = [("pg_fake", BenchmarkConnection::PgFake(&mut fake))];
    create_table_benchmark(criterion, &runtime, &mut connections);
    transactional_ddl_benchmark(criterion, &runtime, &mut connections);
    benchmark_settings(criterion, &runtime, &mut connections);
    benchmark_transaction_local_gucs(criterion, &runtime, &mut connections);
    benchmark_savepoints(criterion, &runtime, &mut connections);
    migration_table_lock_benchmark(criterion, &runtime, &mut connections);
    benchmark_sqlx_migration_chain(criterion, &runtime, &mut connections);
    procedural_trigger_benchmark(criterion, &runtime, &mut connections);
    alter_table_benchmark(criterion, &runtime, &mut connections);
    partial_unique_index_benchmark(criterion, &runtime, &mut connections);
    temporary_table_benchmark(criterion, &runtime, &mut connections);
    sequence_benchmark(criterion, &runtime, &mut connections);
    catalog_regclass_benchmark(criterion, &runtime, &mut connections);
    serial_identity_benchmark(criterion, &runtime, &mut connections);
    uuid_temporal_benchmark(criterion, &runtime, &mut connections);
    offset_datetime_benchmark(criterion, &runtime, &mut connections);
    bigint_uuid_array_benchmark(criterion, &runtime, &mut connections);
    required_array_query_benchmarks(criterion, &runtime, &mut connections);
    general_array_operation_benchmarks(criterion, &runtime, &mut connections);
    hashed_advisory_lock_benchmark(criterion, &runtime, &mut connections);
    benchmark_json(criterion, &runtime, &mut connections);
    benchmark_jsonb(criterion, &runtime, &mut connections);
    migration_data_transform_benchmark(criterion, &runtime, &mut connections);
    insert_benchmark(criterion, &runtime, &mut connections);
    on_conflict_benchmark(criterion, &runtime, &mut connections);
    update_benchmark(criterion, &runtime, &mut connections);
    delete_benchmark(criterion, &runtime, &mut connections);
    transaction_benchmark(criterion, &runtime, &mut connections);
    repeatable_read_benchmark(criterion, &runtime, &mut connections);
    select_benchmark(criterion, &runtime, &mut connections);
    nested_filtered_view_benchmark(criterion, &runtime, &mut connections);
    order_by_benchmark(criterion, &runtime, &mut connections);
    core_vs_sqlx_benchmark(criterion, &runtime);
    benchmark_snapshots(criterion, None);
    parsed_vs_prepared_benchmark(criterion);
    transaction_history_benchmark(criterion, None);
    mvcc_version_chain_benchmark(criterion, None);
    indexed_vs_scan_benchmark(criterion, None);
    benchmark_selective_outer_joins(criterion, &[10, 100, 500, 1_000, 10_000]);
    benchmark_composite_access(criterion, &[100, 1_000, 10_000]);
    benchmark_composite_prefix(criterion, &[100, 1_000, 10_000]);
    benchmark_partial_index_access(criterion, &[10, 100, 1_000, 10_000]);
    benchmark_partial_index_maintenance(criterion);
    benchmark_bounded_on_conflict(criterion);
    benchmark_bounded_indexed_update(criterion);
    benchmark_guarded_mutations(criterion);
    benchmark_lookup_scaling(criterion, &runtime, &mut connections, None);
    benchmark_populated_table_writes(criterion, &runtime, &mut connections, None);
    concurrency_benchmark(criterion, &runtime);
    foreign_key_insert_benchmark(criterion, &runtime, &mut connections);
    inner_join_benchmark(criterion, &runtime, &mut connections);
    benchmark_selective_indexed_join(criterion, &runtime, &mut connections, None);
    benchmark_membership_joins(criterion, &runtime, &mut connections, None);
    benchmark_lateral(criterion, &runtime, &mut connections);
    benchmark_skip_locked(criterion, &runtime, &mut connections);
    derived_and_scalar_subquery_benchmark(criterion, &runtime, &mut connections);
    materialized_cte_benchmark(criterion, &runtime, &mut connections);
    derived_source_join_benchmark(criterion, &runtime, &mut connections);
    data_modifying_cte_benchmark(criterion, &runtime, &mut connections);
    recursive_cte_benchmark(criterion, &runtime, &mut connections);
    global_aggregate_benchmark(criterion, &runtime, &mut connections);
    grouped_aggregate_benchmark(criterion, &runtime, &mut connections);
    select_distinct_benchmark(criterion, &runtime, &mut connections);
    set_operation_benchmark(criterion, &runtime, &mut connections);
    benchmark_bound_writes(criterion, &runtime, &mut connections);
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
