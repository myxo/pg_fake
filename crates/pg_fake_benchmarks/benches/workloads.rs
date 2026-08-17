use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use pg_fake::{
    api::{IsolationLevel, Session},
    value::Value,
};
use pg_fake_benchmarks as benchmarks;
use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{AssertSqlSafe, Connection};
use sqlx_postgres::PgConnection;
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::runtime::Runtime;

enum BenchmarkConnection<'a> {
    PgFake(&'a mut PgFakeConnection),
    Postgres(&'a mut PgConnection),
}

type NamedBenchmarkConnection<'a> = (&'static str, BenchmarkConnection<'a>);

impl BenchmarkConnection<'_> {
    fn execute(&mut self, runtime: &Runtime, sql: &str) {
        match self {
            Self::PgFake(connection) => {
                let result = runtime
                    .block_on(sqlx::query(AssertSqlSafe(sql)).execute(&mut **connection))
                    .unwrap();
                black_box(result);
            }
            Self::Postgres(connection) => {
                let result = runtime
                    .block_on(sqlx::query(AssertSqlSafe(sql)).execute(&mut **connection))
                    .unwrap();
                black_box(result);
            }
        }
    }

    fn fetch(&mut self, runtime: &Runtime, sql: &str) {
        match self {
            Self::PgFake(connection) => {
                let rows = runtime
                    .block_on(sqlx::query(AssertSqlSafe(sql)).fetch_all(&mut **connection))
                    .unwrap();
                black_box(rows);
            }
            Self::Postgres(connection) => {
                let rows = runtime
                    .block_on(sqlx::query(AssertSqlSafe(sql)).fetch_all(&mut **connection))
                    .unwrap();
                black_box(rows);
            }
        }
    }

    fn execute_in_transaction(&mut self, runtime: &Runtime, sql: &str) {
        match self {
            Self::PgFake(connection) => {
                let result = runtime.block_on(async {
                    let mut transaction = connection.begin().await.unwrap();
                    let result = sqlx::query(AssertSqlSafe(sql))
                        .execute(&mut *transaction)
                        .await
                        .unwrap();
                    transaction.commit().await.unwrap();
                    result
                });
                black_box(result);
            }
            Self::Postgres(connection) => {
                let result = runtime.block_on(async {
                    let mut transaction = connection.begin().await.unwrap();
                    let result = sqlx::query(AssertSqlSafe(sql))
                        .execute(&mut *transaction)
                        .await
                        .unwrap();
                    transaction.commit().await.unwrap();
                    result
                });
                black_box(result);
            }
        }
    }

    fn fetch_in_transaction(&mut self, runtime: &Runtime, begin: &str, sql: &str) {
        match self {
            Self::PgFake(connection) => runtime.block_on(async {
                let mut transaction = connection.begin_with(AssertSqlSafe(begin)).await.unwrap();
                let rows = sqlx::query(AssertSqlSafe(sql))
                    .fetch_all(&mut *transaction)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
                black_box(rows);
            }),
            Self::Postgres(connection) => runtime.block_on(async {
                let mut transaction = connection.begin_with(AssertSqlSafe(begin)).await.unwrap();
                let rows = sqlx::query(AssertSqlSafe(sql))
                    .fetch_all(&mut *transaction)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
                black_box(rows);
            }),
        }
    }
}

fn fake_execute(runtime: &Runtime, connection: &mut PgFakeConnection, sql: &str) {
    let result = runtime
        .block_on(sqlx::query(AssertSqlSafe(sql)).execute(connection))
        .unwrap();
    black_box(result);
}

fn fake_query(runtime: &Runtime, connection: &mut PgFakeConnection, sql: &str) {
    let rows = runtime
        .block_on(sqlx::query(AssertSqlSafe(sql)).fetch_all(connection))
        .unwrap();
    black_box(rows);
}

fn core_execute(session: &mut Session, sql: &str) {
    let results = session.execute(sql).unwrap();
    black_box(results);
}

fn insert_values_sql(table: &str, rows: usize) -> String {
    let mut sql = format!("INSERT INTO {table} VALUES ");
    for id in 1..=rows {
        if id != 1 {
            sql.push(',');
        }
        sql.push_str(&format!("({id}, 'benchmark')"));
    }
    sql
}

struct PostgresBenchmark {
    connection: PgConnection,
    _container: Option<Container<Postgres>>,
}

fn postgres_benchmark(runtime: &Runtime) -> PostgresBenchmark {
    let (container, url) = if let Ok(url) = env::var("PG_FAKE_DATABASE_URL") {
        println!("connect to manually setup postgres on {url}");
        (None, url)
    } else {
        if env::var_os("DOCKER_HOST").is_none() {
            let socket = PathBuf::from(env::var_os("HOME").expect("HOME must be set"))
                .join(".colima/default/docker.sock");
            if socket.exists() {
                unsafe { env::set_var("DOCKER_HOST", format!("unix://{}", socket.display())) };
            }
        }
        let container = Postgres::default()
            .with_tag("18")
            .start()
            .expect("PostgreSQL 18 container must start");
        let url = format!(
            "postgresql://postgres:postgres@{}:{}/postgres",
            container
                .get_host()
                .expect("container host must be available"),
            container
                .get_host_port_ipv4(5432)
                .expect("PostgreSQL port must be available")
        );
        println!("connect to postgres in container on {url}");
        (Some(container), url)
    };
    let mut connection = runtime
        .block_on(PgConnection::connect(&url))
        .expect("must connect SQLx to PostgreSQL 18");
    for sql in [
        "SELECT pg_advisory_lock(18818, 1)",
        "DROP SCHEMA IF EXISTS pgfake_benchmark CASCADE",
        "CREATE SCHEMA pgfake_benchmark",
        "SET search_path TO pgfake_benchmark",
    ] {
        runtime
            .block_on(sqlx::query(AssertSqlSafe(sql)).execute(&mut connection))
            .unwrap();
    }
    PostgresBenchmark {
        connection,
        _container: container,
    }
}

fn create_table_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    let create = "CREATE TABLE create_table (id INTEGER, name TEXT)";
    let drop = "DROP TABLE create_table";
    let mut group = criterion.benchmark_group(benchmarks::find_benchmark("create_table").name);

    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.execute(runtime, create);
                connection.execute(runtime, drop);
            });
        });
    }
    group.finish();
}

fn insert_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for create in [
        "CREATE TABLE insert_row (id INTEGER PRIMARY KEY CHECK (id > 0), name TEXT NOT NULL DEFAULT upper('benchmark'), CHECK (length(name) > 0))",
        "CREATE TABLE insert_row_with_defaults (id INTEGER PRIMARY KEY CHECK (id > 0), name TEXT NOT NULL DEFAULT upper('benchmark'), CHECK (length(name) > 0))",
    ] {
        for (_, connection) in connections.iter_mut() {
            connection.execute(runtime, create);
        }
    }
    let mut group = criterion.benchmark_group(benchmarks::find_benchmark("insert_row").name);

    for (name, connection) in connections.iter_mut() {
        let mut id = 0;
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                id += 1;
                connection.execute(
                    runtime,
                    &format!("INSERT INTO insert_row VALUES ({id}, 'benchmark')"),
                );
            });
        });
    }
    group.finish();

    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("insert_row_with_defaults").name);

    for (name, connection) in connections.iter_mut() {
        let mut id = 0;
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                id += 1;
                connection.execute(
                    runtime,
                    &format!("INSERT INTO insert_row_with_defaults (id) VALUES ({id})"),
                );
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "DROP TABLE insert_row, insert_row_with_defaults");
    }
}

fn update_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "CREATE TABLE update_row (id BIGINT PRIMARY KEY, amount INTEGER)",
        );
    }
    let mut group = criterion.benchmark_group(benchmarks::find_benchmark("update_row").name);

    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for id in 0..iterations {
                    connection
                        .execute(runtime, &format!("INSERT INTO update_row VALUES ({id}, 0)"));
                    let update =
                        format!("UPDATE update_row SET amount = amount + 1 WHERE id = {id}");
                    let started = Instant::now();
                    connection.execute(runtime, &update);
                    elapsed += started.elapsed();
                    connection.execute(runtime, &format!("DELETE FROM update_row WHERE id = {id}"));
                }
                elapsed
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "DROP TABLE update_row");
    }
}

fn delete_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "CREATE TABLE delete_row (id INTEGER)");
    }
    let mut group = criterion.benchmark_group(benchmarks::find_benchmark("delete_row").name);

    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for id in 0..iterations {
                    connection.execute(runtime, &format!("INSERT INTO delete_row VALUES ({id})"));
                    let delete = format!("DELETE FROM delete_row WHERE id = {id}");
                    let started = Instant::now();
                    connection.execute(runtime, &delete);
                    elapsed += started.elapsed();
                }
                elapsed
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "DROP TABLE delete_row");
    }
}

fn transaction_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "CREATE TABLE transaction_insert (id INTEGER)");
    }
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("transaction_insert").name);

    for (name, connection) in connections.iter_mut() {
        let mut id = 0;
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                id += 1;
                connection.execute_in_transaction(
                    runtime,
                    &format!("INSERT INTO transaction_insert VALUES ({id})"),
                );
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "DROP TABLE transaction_insert");
    }
}

fn repeatable_read_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "CREATE TABLE transaction_repeatable_read_select_for_update (id INTEGER)",
        );
        connection.execute(
            runtime,
            "INSERT INTO transaction_repeatable_read_select_for_update VALUES (1)",
        );
    }
    let select = "SELECT * FROM transaction_repeatable_read_select_for_update FOR UPDATE";
    let mut group = criterion.benchmark_group(
        benchmarks::find_benchmark("transaction_repeatable_read_select_for_update").name,
    );

    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch_in_transaction(
                    runtime,
                    "BEGIN ISOLATION LEVEL REPEATABLE READ",
                    select,
                );
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "DROP TABLE transaction_repeatable_read_select_for_update",
        );
    }
}

fn select_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for table in ["select_100_rows", "limit_offset_ordered_100_rows"] {
        let create = format!("CREATE TABLE {table} (id INTEGER, name TEXT)");
        let insert = insert_values_sql(table, 100);
        for (_, connection) in connections.iter_mut() {
            connection.execute(runtime, &create);
            connection.execute(runtime, &insert);
        }
    }
    let select = "SELECT * FROM select_100_rows";
    let mut group = criterion.benchmark_group(benchmarks::find_benchmark("select_100_rows").name);

    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch(runtime, select);
            });
        });
    }
    group.finish();

    let select =
        "SELECT id, name FROM limit_offset_ordered_100_rows ORDER BY id DESC LIMIT 10 OFFSET 40";
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("limit_offset_ordered_100_rows").name);

    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch(runtime, select);
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "DROP TABLE select_100_rows, limit_offset_ordered_100_rows",
        );
    }
}

fn order_by_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "CREATE TABLE order_by_100_rows (id INTEGER, bucket INTEGER)",
        );
    }
    for id in 1..=100 {
        let bucket = if id % 10 == 0 {
            "NULL".to_owned()
        } else {
            (id % 10).to_string()
        };
        for (_, connection) in connections.iter_mut() {
            connection.execute(
                runtime,
                &format!("INSERT INTO order_by_100_rows VALUES ({id}, {bucket})"),
            );
        }
    }
    let select = "SELECT id, bucket FROM order_by_100_rows ORDER BY bucket DESC NULLS LAST, id ASC";
    let mut group = criterion.benchmark_group(benchmarks::find_benchmark("order_by_100_rows").name);

    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch(runtime, select);
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "DROP TABLE order_by_100_rows");
    }
}

fn core_vs_sqlx_benchmark(criterion: &mut Criterion, runtime: &Runtime) {
    let mut core = Db::create().create_session();
    core_execute(
        &mut core,
        "CREATE TABLE adapter_overhead_select_100_rows (id INTEGER, name TEXT)",
    );
    core_execute(
        &mut core,
        &insert_values_sql("adapter_overhead_select_100_rows", 100),
    );
    core_execute(&mut core, "BEGIN");
    let query = "SELECT id, name FROM adapter_overhead_select_100_rows ORDER BY id";

    let mut sqlx = PgFakeConnection::new(Db::create());
    fake_execute(
        runtime,
        &mut sqlx,
        "CREATE TABLE adapter_overhead_select_100_rows (id INTEGER, name TEXT)",
    );
    fake_execute(
        runtime,
        &mut sqlx,
        &insert_values_sql("adapter_overhead_select_100_rows", 100),
    );
    fake_execute(runtime, &mut sqlx, "BEGIN");

    let mut group = criterion
        .benchmark_group(benchmarks::find_benchmark("adapter_overhead_select_100_rows").name);
    group.throughput(Throughput::Elements(100));
    group.bench_function("core", |benchmark| {
        benchmark.iter(|| {
            let result = core.query(query, &[]).unwrap();
            black_box(result);
        });
    });
    group.bench_function("sqlx", |benchmark| {
        benchmark.iter(|| {
            fake_query(runtime, &mut sqlx, query);
        });
    });
    group.finish();

    core_execute(&mut core, "ROLLBACK");
    fake_execute(runtime, &mut sqlx, "ROLLBACK");
}

fn parsed_vs_prepared_benchmark(criterion: &mut Criterion) {
    let mut session = Db::create().create_session();
    core_execute(
        &mut session,
        "CREATE TABLE core_parsed_vs_prepared_point_select (id INTEGER PRIMARY KEY, name TEXT)",
    );
    core_execute(
        &mut session,
        &insert_values_sql("core_parsed_vs_prepared_point_select", 100),
    );
    let query = "SELECT name FROM core_parsed_vs_prepared_point_select WHERE id = $1";
    let prepared = session.prepare(query).unwrap();
    core_execute(&mut session, "BEGIN");
    let parameters = [Value::Int4(50)];

    let mut group = criterion
        .benchmark_group(benchmarks::find_benchmark("core_parsed_vs_prepared_point_select").name);
    group.bench_function("parse_and_analyze", |benchmark| {
        benchmark.iter(|| {
            let result = session.query(query, &parameters).unwrap();
            black_box(result);
        });
    });
    group.bench_function("prepared_reuse", |benchmark| {
        benchmark.iter(|| {
            let result = session.query_prepared(&prepared, &parameters).unwrap();
            black_box(result);
        });
    });
    group.finish();

    core_execute(&mut session, "ROLLBACK");
}

fn transaction_history_benchmark(criterion: &mut Criterion) {
    let mut group = criterion
        .benchmark_group(benchmarks::find_benchmark("transaction_history_point_select").name);
    for completed in [1_u64, 100, 10_000, 100_000] {
        let mut session = Db::create().create_session();
        core_execute(
            &mut session,
            "CREATE TABLE transaction_history_probe (id INTEGER)",
        );
        core_execute(
            &mut session,
            "INSERT INTO transaction_history_probe VALUES (1)",
        );
        for _ in 0..completed {
            session.begin().unwrap().commit().unwrap();
        }
        let statement = session
            .prepare("SELECT id FROM transaction_history_probe")
            .unwrap();
        let mut transaction = session.begin_with(IsolationLevel::RepeatableRead).unwrap();
        group.bench_with_input(
            BenchmarkId::from_parameter(completed),
            &completed,
            |benchmark, _| {
                benchmark.iter(|| {
                    let result = transaction.query_prepared(&statement, &[]).unwrap();
                    black_box(result);
                });
            },
        );
        transaction.rollback().unwrap();
    }
    group.finish();
}

fn mvcc_version_chain_benchmark(criterion: &mut Criterion) {
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("mvcc_old_snapshot_read").name);
    for updates in [1_u64, 100, 10_000] {
        let db = Db::create();
        let mut setup = db.create_session();
        core_execute(
            &mut setup,
            "CREATE TABLE mvcc_chain_probe (id INTEGER PRIMARY KEY, amount INTEGER)",
        );
        core_execute(&mut setup, "INSERT INTO mvcc_chain_probe VALUES (1, 0)");
        let mut reader = db.create_session();
        let statement = reader
            .prepare("SELECT amount FROM mvcc_chain_probe WHERE id = 1")
            .unwrap();
        let mut reader = reader.begin_with(IsolationLevel::RepeatableRead).unwrap();
        black_box(reader.query_prepared(&statement, &[]).unwrap());

        let mut updater = db.create_session();
        let update = updater
            .prepare("UPDATE mvcc_chain_probe SET amount = amount + 1 WHERE id = 1")
            .unwrap();
        for _ in 0..updates {
            black_box(updater.execute_prepared(&update, &[]).unwrap());
        }

        group.bench_with_input(
            BenchmarkId::from_parameter(updates),
            &updates,
            |benchmark, _| {
                benchmark.iter(|| {
                    let result = reader.query_prepared(&statement, &[]).unwrap();
                    black_box(result);
                });
            },
        );
        reader.rollback().unwrap();
    }
    group.finish();
}

fn indexed_vs_scan_benchmark(criterion: &mut Criterion) {
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("point_lookup_index_vs_scan").name);
    for rows in [100_usize, 10_000] {
        group.throughput(Throughput::Elements(rows as u64));

        let mut indexed = Db::create().create_session();
        core_execute(
            &mut indexed,
            "CREATE TABLE indexed_lookup (id INTEGER PRIMARY KEY, name TEXT)",
        );
        core_execute(&mut indexed, &insert_values_sql("indexed_lookup", rows));
        let indexed_statement = indexed
            .prepare("SELECT name FROM indexed_lookup WHERE id = $1")
            .unwrap();
        let indexed_parameters = [Value::Int4(rows as i32)];
        let mut indexed = indexed.begin().unwrap();
        group.bench_with_input(
            BenchmarkId::new("unique_index", rows),
            &rows,
            |benchmark, _| {
                benchmark.iter(|| {
                    let result = indexed
                        .query_prepared(&indexed_statement, &indexed_parameters)
                        .unwrap();
                    black_box(result);
                });
            },
        );
        indexed.rollback().unwrap();

        let mut scanned = Db::create().create_session();
        core_execute(
            &mut scanned,
            "CREATE TABLE scanned_lookup (id INTEGER, name TEXT)",
        );
        core_execute(&mut scanned, &insert_values_sql("scanned_lookup", rows));
        let scanned_statement = scanned
            .prepare("SELECT name FROM scanned_lookup WHERE id = $1")
            .unwrap();
        let scanned_parameters = [Value::Int4(rows as i32)];
        let mut scanned = scanned.begin().unwrap();
        group.bench_with_input(
            BenchmarkId::new("heap_scan", rows),
            &rows,
            |benchmark, _| {
                benchmark.iter(|| {
                    let result = scanned
                        .query_prepared(&scanned_statement, &scanned_parameters)
                        .unwrap();
                    black_box(result);
                });
            },
        );
        scanned.rollback().unwrap();
    }
    group.finish();
}

fn concurrency_benchmark(criterion: &mut Criterion, runtime: &Runtime) {
    let db = Db::create();
    let mut setup = PgFakeConnection::new(db.clone());
    fake_execute(
        runtime,
        &mut setup,
        "CREATE TABLE concurrency_probe (id INTEGER PRIMARY KEY, amount INTEGER)",
    );
    fake_execute(
        runtime,
        &mut setup,
        "INSERT INTO concurrency_probe VALUES (1, 0), (2, 0)",
    );

    let mut sequential_first = PgFakeConnection::new(db.clone());
    let mut sequential_second = PgFakeConnection::new(db.clone());
    let mut parallel_first = PgFakeConnection::new(db.clone());
    let mut parallel_second = PgFakeConnection::new(db);
    for connection in [
        &mut sequential_first,
        &mut sequential_second,
        &mut parallel_first,
        &mut parallel_second,
    ] {
        fake_execute(runtime, connection, "BEGIN");
    }

    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("concurrent_uncontended_reads").name);
    group.throughput(Throughput::Elements(2));
    group.bench_function("sequential", |benchmark| {
        benchmark.iter(|| {
            runtime.block_on(async {
                let first = sqlx::query("SELECT amount FROM concurrency_probe WHERE id = $1")
                    .bind(1_i32)
                    .fetch_all(&mut sequential_first)
                    .await
                    .unwrap();
                let second = sqlx::query("SELECT amount FROM concurrency_probe WHERE id = $1")
                    .bind(2_i32)
                    .fetch_all(&mut sequential_second)
                    .await
                    .unwrap();
                black_box((first, second));
            });
        });
    });
    group.bench_function("parallel", |benchmark| {
        benchmark.iter(|| {
            runtime.block_on(async {
                let (first, second) = tokio::join!(
                    sqlx::query("SELECT amount FROM concurrency_probe WHERE id = $1")
                        .bind(1_i32)
                        .fetch_all(&mut parallel_first),
                    sqlx::query("SELECT amount FROM concurrency_probe WHERE id = $1")
                        .bind(2_i32)
                        .fetch_all(&mut parallel_second),
                );
                black_box((first.unwrap(), second.unwrap()));
            });
        });
    });
    group.finish();

    for connection in [
        &mut sequential_first,
        &mut sequential_second,
        &mut parallel_first,
        &mut parallel_second,
    ] {
        fake_execute(runtime, connection, "ROLLBACK");
    }

    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut setup = PgFakeConnection::new(db.clone());
    fake_execute(
        runtime,
        &mut setup,
        "CREATE TABLE contention_probe (id INTEGER PRIMARY KEY, amount INTEGER)",
    );
    fake_execute(
        runtime,
        &mut setup,
        "INSERT INTO contention_probe VALUES (1, 0)",
    );
    let mut first = PgFakeConnection::new(db.clone());
    let mut second = PgFakeConnection::new(db);
    let mut group = criterion
        .benchmark_group(benchmarks::find_benchmark("concurrent_same_row_contention").name);
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("wait_then_rollback", |benchmark| {
        benchmark.iter(|| {
            runtime.block_on(async {
                let mut first_transaction = first.begin().await.unwrap();
                sqlx::query("UPDATE contention_probe SET amount = amount + 1 WHERE id = 1")
                    .execute(&mut *first_transaction)
                    .await
                    .unwrap();
                let mut second_transaction = second.begin().await.unwrap();
                let waiter = async {
                    sqlx::query("UPDATE contention_probe SET amount = amount + 1 WHERE id = 1")
                        .execute(&mut *second_transaction)
                        .await
                        .unwrap();
                    second_transaction.rollback().await.unwrap();
                };
                let holder = async {
                    tokio::time::sleep(Duration::from_micros(100)).await;
                    first_transaction.rollback().await.unwrap();
                };
                tokio::join!(waiter, holder);
            });
        });
    });
    group.finish();
}

fn foreign_key_insert_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "CREATE TABLE foreign_key_insert_parent (id INTEGER PRIMARY KEY)",
        );
        connection.execute(
            runtime,
            "CREATE TABLE foreign_key_insert (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES foreign_key_insert_parent)",
        );
    }
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("foreign_key_insert").name);
    for (name, connection) in connections.iter_mut() {
        let mut id = 0;
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                id += 1;
                connection.execute(
                    runtime,
                    &format!("INSERT INTO foreign_key_insert_parent VALUES ({id})"),
                );
                connection.execute(
                    runtime,
                    &format!("INSERT INTO foreign_key_insert VALUES ({id}, {id})"),
                );
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "DROP TABLE foreign_key_insert, foreign_key_insert_parent",
        );
    }
}

fn benchmarks(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = postgres_benchmark(&runtime);
    {
        let mut fake = PgFakeConnection::new(Db::create());
        let mut connections = [
            ("pg_fake", BenchmarkConnection::PgFake(&mut fake)),
            (
                "postgres_18",
                BenchmarkConnection::Postgres(&mut postgres.connection),
            ),
        ];

        create_table_benchmark(criterion, &runtime, &mut connections);
        insert_benchmark(criterion, &runtime, &mut connections);
        update_benchmark(criterion, &runtime, &mut connections);
        delete_benchmark(criterion, &runtime, &mut connections);
        transaction_benchmark(criterion, &runtime, &mut connections);
        repeatable_read_benchmark(criterion, &runtime, &mut connections);
        select_benchmark(criterion, &runtime, &mut connections);
        order_by_benchmark(criterion, &runtime, &mut connections);
        core_vs_sqlx_benchmark(criterion, &runtime);
        parsed_vs_prepared_benchmark(criterion);
        transaction_history_benchmark(criterion);
        mvcc_version_chain_benchmark(criterion);
        indexed_vs_scan_benchmark(criterion);
        concurrency_benchmark(criterion, &runtime);
        foreign_key_insert_benchmark(criterion, &runtime, &mut connections);
        inner_join_benchmark(criterion, &runtime, &mut connections);
        derived_and_scalar_subquery_benchmark(criterion, &runtime, &mut connections);
        global_aggregate_benchmark(criterion, &runtime, &mut connections);
        grouped_aggregate_benchmark(criterion, &runtime, &mut connections);
    }
    runtime
        .block_on(
            sqlx::query("DROP SCHEMA pgfake_benchmark CASCADE").execute(&mut postgres.connection),
        )
        .unwrap();
}

fn global_aggregate_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    let values = (1..=100)
        .map(|id| format!("({id}, {})", id % 10))
        .collect::<Vec<_>>()
        .join(",");
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "CREATE TABLE global_aggregate_100_rows (id INTEGER, bucket INTEGER)",
        );
        connection.execute(
            runtime,
            &format!("INSERT INTO global_aggregate_100_rows VALUES {values}"),
        );
    }
    let query = "SELECT count(*), sum(id), avg(id), min(bucket), max(bucket) FROM global_aggregate_100_rows";
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("global_aggregate_100_rows").name);
    group.throughput(Throughput::Elements(100));
    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch(runtime, query);
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "DROP TABLE global_aggregate_100_rows");
    }
}

fn grouped_aggregate_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    let values = (1..=100)
        .map(|id| format!("({id}, {})", id % 10))
        .collect::<Vec<_>>()
        .join(",");
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "CREATE TABLE grouped_aggregate_100_rows (id INTEGER, bucket INTEGER)",
        );
        connection.execute(
            runtime,
            &format!("INSERT INTO grouped_aggregate_100_rows VALUES {values}"),
        );
    }
    let query = "SELECT bucket, count(*), sum(id) FROM grouped_aggregate_100_rows GROUP BY bucket HAVING count(*) > 5 ORDER BY bucket";
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("grouped_aggregate_100_rows").name);
    group.throughput(Throughput::Elements(100));
    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch(runtime, query);
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(runtime, "DROP TABLE grouped_aggregate_100_rows");
    }
}

fn derived_and_scalar_subquery_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    let values = (1..=100)
        .map(|id| format!("({id})"))
        .collect::<Vec<_>>()
        .join(",");
    for table in [
        "derived_and_scalar_subquery_100_rows",
        "correlated_exists_100_rows",
    ] {
        let create = format!("CREATE TABLE {table} (id INTEGER)");
        let insert = format!("INSERT INTO {table} VALUES {values}");
        for (_, connection) in connections.iter_mut() {
            connection.execute(runtime, &create);
            connection.execute(runtime, &insert);
        }
    }
    let query = "SELECT source.id FROM (SELECT id FROM derived_and_scalar_subquery_100_rows WHERE id <= (SELECT 100)) AS source WHERE source.id = ANY (SELECT id FROM derived_and_scalar_subquery_100_rows) ORDER BY source.id";
    let mut group = criterion
        .benchmark_group(benchmarks::find_benchmark("derived_and_scalar_subquery_100_rows").name);
    group.throughput(Throughput::Elements(100));
    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch(runtime, query);
            });
        });
    }
    group.finish();

    let query = "SELECT outer_row.id FROM correlated_exists_100_rows AS outer_row WHERE EXISTS (SELECT 1 FROM correlated_exists_100_rows AS inner_row WHERE inner_row.id = outer_row.id)";
    let mut group =
        criterion.benchmark_group(benchmarks::find_benchmark("correlated_exists_100_rows").name);
    group.throughput(Throughput::Elements(100));
    for (name, connection) in connections.iter_mut() {
        group.bench_function(*name, |benchmark| {
            benchmark.iter(|| {
                connection.fetch(runtime, query);
            });
        });
    }
    group.finish();
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "DROP TABLE derived_and_scalar_subquery_100_rows, correlated_exists_100_rows",
        );
    }
}

fn inner_join_benchmark(
    criterion: &mut Criterion,
    runtime: &Runtime,
    connections: &mut [NamedBenchmarkConnection<'_>],
) {
    let values = (1..=100)
        .map(|id| format!("({id}, {})", id % 10))
        .collect::<Vec<_>>()
        .join(",");
    for table in [
        "selective_inner_join_left",
        "selective_inner_join_right",
        "many_match_inner_join_left",
        "many_match_inner_join_right",
    ] {
        let create = format!("CREATE TABLE {table} (id INTEGER, bucket INTEGER)");
        let insert = format!("INSERT INTO {table} VALUES {values}");
        for (_, connection) in connections.iter_mut() {
            connection.execute(runtime, &create);
            connection.execute(runtime, &insert);
        }
    }
    for (name, query, expected) in [
        (
            "selective_inner_join",
            "SELECT left_row.id FROM selective_inner_join_left left_row INNER JOIN selective_inner_join_right right_row ON left_row.id = right_row.id WHERE left_row.id = 50",
            1,
        ),
        (
            "many_match_inner_join",
            "SELECT left_row.id FROM many_match_inner_join_left left_row INNER JOIN many_match_inner_join_right right_row ON left_row.bucket = right_row.bucket WHERE left_row.bucket = 0",
            100,
        ),
    ] {
        let mut group = criterion.benchmark_group(benchmarks::find_benchmark(name).name);
        group.throughput(Throughput::Elements(expected));
        for (connection_name, connection) in connections.iter_mut() {
            group.bench_function(*connection_name, |benchmark| {
                benchmark.iter(|| {
                    connection.fetch(runtime, query);
                });
            });
        }
        group.finish();
    }
    for (_, connection) in connections.iter_mut() {
        connection.execute(
            runtime,
            "DROP TABLE selective_inner_join_left, selective_inner_join_right, many_match_inner_join_left, many_match_inner_join_right",
        );
    }
}

criterion_group!(workloads, benchmarks);
criterion_main!(workloads);
