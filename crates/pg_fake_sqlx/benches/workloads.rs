use std::{
    env,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use pg_fake::{
    api::{IsolationLevel, Session, StatementResult},
    value::Value,
};
use pg_fake_sqlx::{Db, PgFakeConnection, PgFakeRow};
use postgres::{Client, NoTls, SimpleQueryMessage};
use sqlx::{AssertSqlSafe, Connection};
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::runtime::Runtime;

static TABLE_NUMBER: AtomicU64 = AtomicU64::new(1);
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

fn fake_execute(runtime: &Runtime, connection: &mut PgFakeConnection, sql: &str) -> u64 {
    runtime
        .block_on(sqlx::query(AssertSqlSafe(sql)).execute(connection))
        .unwrap()
        .rows_affected()
}

fn fake_query(runtime: &Runtime, connection: &mut PgFakeConnection, sql: &str) -> Vec<PgFakeRow> {
    runtime
        .block_on(sqlx::query(AssertSqlSafe(sql)).fetch_all(connection))
        .unwrap()
}

fn core_execute(session: &mut Session, sql: &str) -> u64 {
    let results = session.execute(sql).unwrap();
    assert_eq!(results.len(), 1);
    match results.into_iter().next().unwrap() {
        StatementResult::Affected(rows) => rows,
        StatementResult::Query(_) => panic!("expected an affected-row result"),
    }
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
    client: Client,
    _container: Option<Container<Postgres>>,
}

fn postgres_benchmark() -> PostgresBenchmark {
    let _environment_lock = ENVIRONMENT_LOCK
        .lock()
        .expect("environment mutex must not be poisoned");
    if let Ok(url) = env::var("PG_FAKE_DATABASE_URL") {
        println!("connect to manually setup postgres on {url}");
        return PostgresBenchmark {
            client: Client::connect(&url, NoTls).expect("must connect to PostgreSQL 18"),
            _container: None,
        };
    }
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
    PostgresBenchmark {
        client: Client::connect(&url, NoTls).expect("must connect to PostgreSQL 18"),
        _container: Some(container),
    }
}

fn unique_table_name(workload: &str) -> String {
    format!(
        "pg_fake_benchmark_{workload}_{}_{}",
        std::process::id(),
        TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
    )
}

fn create_table_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("create_fake");
    let postgres_table = unique_table_name("create_postgres");
    let fake_create = format!("CREATE TABLE {fake_table} (id INTEGER, name TEXT)");
    let fake_drop = format!("DROP TABLE {fake_table}");
    let postgres_create = format!("CREATE TABLE {postgres_table} (id INTEGER, name TEXT)");
    let postgres_drop = format!("DROP TABLE {postgres_table}");
    let mut fake = PgFakeConnection::new(Db::new());
    let mut group = criterion.benchmark_group("create_table");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            assert_eq!(fake_execute(runtime, &mut fake, &fake_create), 0);
            assert_eq!(fake_execute(runtime, &mut fake, &fake_drop), 1);
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            assert_eq!(postgres.execute(&postgres_create, &[]).unwrap(), 0);
            assert_eq!(postgres.execute(&postgres_drop, &[]).unwrap(), 0);
        });
    });
    group.finish();
}

fn insert_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("insert_fake");
    let postgres_table = unique_table_name("insert_postgres");
    let mut fake = PgFakeConnection::new(Db::new());
    assert_eq!(
        fake_execute(
            runtime,
            &mut fake,
            &format!(
                "CREATE TABLE {fake_table} (id INTEGER PRIMARY KEY CHECK (id > 0), name TEXT NOT NULL DEFAULT upper('benchmark'), CHECK (length(name) > 0))"
            )
        ),
        0
    );
    assert_eq!(
        postgres
            .execute(
                &format!(
                    "CREATE TABLE {postgres_table} (id INTEGER PRIMARY KEY CHECK (id > 0), name TEXT NOT NULL DEFAULT upper('benchmark'), CHECK (length(name) > 0))"
                ),
                &[],
            )
            .unwrap(),
        0
    );
    let mut fake_id = 0;
    let mut postgres_id = 0;
    let mut group = criterion.benchmark_group("insert_row");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            fake_id += 1;
            assert_eq!(
                fake_execute(
                    runtime,
                    &mut fake,
                    &format!("INSERT INTO {fake_table} VALUES ({fake_id}, 'benchmark')")
                ),
                1
            );
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            postgres_id += 1;
            assert_eq!(
                postgres
                    .execute(
                        &format!(
                            "INSERT INTO {postgres_table} VALUES ({postgres_id}, 'benchmark')"
                        ),
                        &[],
                    )
                    .unwrap(),
                1
            );
        });
    });
    group.finish();

    let mut group = criterion.benchmark_group("insert_row_with_defaults");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            fake_id += 1;
            assert_eq!(
                fake_execute(
                    runtime,
                    &mut fake,
                    &format!("INSERT INTO {fake_table} (id) VALUES ({fake_id})"),
                ),
                1
            );
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            postgres_id += 1;
            assert_eq!(
                postgres
                    .execute(
                        &format!("INSERT INTO {postgres_table} (id) VALUES ({postgres_id})"),
                        &[],
                    )
                    .unwrap(),
                1
            );
        });
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn update_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("update_fake");
    let postgres_table = unique_table_name("update_postgres");
    let fake_create = format!("CREATE TABLE {fake_table} (id BIGINT PRIMARY KEY, amount INTEGER)");
    let fake_insert = format!("INSERT INTO {fake_table} VALUES (1, 0)");
    let fake_update = format!("UPDATE {fake_table} SET amount = amount + 1 WHERE id = 1");
    let postgres_insert = format!("INSERT INTO {postgres_table} VALUES ($1, 0)");
    let postgres_update = format!("UPDATE {postgres_table} SET amount = amount + 1 WHERE id = $1");
    let postgres_delete = format!("DELETE FROM {postgres_table} WHERE id = $1");
    assert_eq!(
        postgres
            .execute(
                &format!("CREATE TABLE {postgres_table} (id BIGINT PRIMARY KEY, amount INTEGER)"),
                &[],
            )
            .unwrap(),
        0
    );
    let mut group = criterion.benchmark_group("update_row");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                let mut fake = PgFakeConnection::new(Db::new());
                fake_execute(runtime, &mut fake, &fake_create);
                fake_execute(runtime, &mut fake, &fake_insert);
                let started = Instant::now();
                assert_eq!(fake_execute(runtime, &mut fake, &fake_update), 1);
                elapsed += started.elapsed();
            }
            elapsed
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for id in 0..iterations {
                postgres.execute(&postgres_insert, &[&(id as i64)]).unwrap();
                let started = Instant::now();
                assert_eq!(
                    postgres.execute(&postgres_update, &[&(id as i64)]).unwrap(),
                    1
                );
                elapsed += started.elapsed();
                postgres.execute(&postgres_delete, &[&(id as i64)]).unwrap();
            }
            elapsed
        });
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn delete_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("delete_fake");
    let postgres_table = unique_table_name("delete_postgres");
    postgres
        .execute(&format!("CREATE TABLE {postgres_table} (id INTEGER)"), &[])
        .unwrap();
    let mut group = criterion.benchmark_group("delete_row");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for id in 0..iterations {
                let mut fake = PgFakeConnection::new(Db::new());
                fake_execute(
                    runtime,
                    &mut fake,
                    &format!("CREATE TABLE {fake_table} (id INTEGER)"),
                );
                fake_execute(
                    runtime,
                    &mut fake,
                    &format!("INSERT INTO {fake_table} VALUES ({id})"),
                );
                let delete = format!("DELETE FROM {fake_table} WHERE id = {id}");
                let started = Instant::now();
                assert_eq!(fake_execute(runtime, &mut fake, &delete), 1);
                elapsed += started.elapsed();
            }
            elapsed
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for id in 0..iterations {
                postgres
                    .execute(&format!("INSERT INTO {postgres_table} VALUES ({id})"), &[])
                    .unwrap();
                let delete = format!("DELETE FROM {postgres_table} WHERE id = {id}");
                let started = Instant::now();
                assert_eq!(postgres.execute(&delete, &[]).unwrap(), 1);
                elapsed += started.elapsed();
            }
            elapsed
        });
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn transaction_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("transaction_fake");
    let postgres_table = unique_table_name("transaction_postgres");
    let mut fake = PgFakeConnection::new(Db::new());
    assert_eq!(
        fake_execute(
            runtime,
            &mut fake,
            &format!("CREATE TABLE {fake_table} (id INTEGER)"),
        ),
        0
    );
    assert_eq!(
        postgres
            .execute(&format!("CREATE TABLE {postgres_table} (id INTEGER)"), &[])
            .unwrap(),
        0
    );
    let mut fake_id = 0;
    let mut postgres_id = 0;
    let mut group = criterion.benchmark_group("transaction_insert");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            fake_id += 1;
            runtime.block_on(async {
                let mut transaction = fake.begin().await.unwrap();
                assert_eq!(
                    sqlx::query(AssertSqlSafe(format!(
                        "INSERT INTO {fake_table} VALUES ({fake_id})"
                    )))
                    .execute(&mut *transaction)
                    .await
                    .unwrap()
                    .rows_affected(),
                    1
                );
                transaction.commit().await.unwrap();
            });
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            postgres_id += 1;
            assert_eq!(postgres.execute("BEGIN", &[]).unwrap(), 0);
            assert_eq!(
                postgres
                    .execute(
                        &format!("INSERT INTO {postgres_table} VALUES ({postgres_id})"),
                        &[],
                    )
                    .unwrap(),
                1
            );
            assert_eq!(postgres.execute("COMMIT", &[]).unwrap(), 0);
        });
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn repeatable_read_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("repeatable_read_fake");
    let postgres_table = unique_table_name("repeatable_read_postgres");
    let mut fake = PgFakeConnection::new(Db::new());
    fake_execute(
        runtime,
        &mut fake,
        &format!("CREATE TABLE {fake_table} (id INTEGER)"),
    );
    fake_execute(
        runtime,
        &mut fake,
        &format!("INSERT INTO {fake_table} VALUES (1)"),
    );
    postgres
        .execute(&format!("CREATE TABLE {postgres_table} (id INTEGER)"), &[])
        .unwrap();
    postgres
        .execute(&format!("INSERT INTO {postgres_table} VALUES (1)"), &[])
        .unwrap();
    let fake_select = format!("SELECT * FROM {fake_table} FOR UPDATE");
    let postgres_select = format!("SELECT * FROM {postgres_table} FOR UPDATE");
    let mut group = criterion.benchmark_group("transaction_repeatable_read_select_for_update");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            let result = runtime.block_on(async {
                let mut transaction = fake
                    .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
                    .await
                    .unwrap();
                let result = sqlx::query(AssertSqlSafe(fake_select.as_str()))
                    .fetch_all(&mut *transaction)
                    .await
                    .unwrap();
                transaction.commit().await.unwrap();
                result
            });
            assert_eq!(result.len(), 1);
            black_box(result);
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            postgres
                .execute("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
                .unwrap();
            let result = postgres.simple_query(&postgres_select).unwrap();
            assert_eq!(
                result
                    .iter()
                    .filter(|message| matches!(message, SimpleQueryMessage::Row(_)))
                    .count(),
                1
            );
            postgres.execute("COMMIT", &[]).unwrap();
            black_box(result);
        });
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn select_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("select_fake");
    let postgres_table = unique_table_name("select_postgres");
    let mut fake = PgFakeConnection::new(Db::new());
    assert_eq!(
        fake_execute(
            runtime,
            &mut fake,
            &format!("CREATE TABLE {fake_table} (id INTEGER, name TEXT)"),
        ),
        0
    );
    assert_eq!(
        postgres
            .execute(
                &format!("CREATE TABLE {postgres_table} (id INTEGER, name TEXT)"),
                &[],
            )
            .unwrap(),
        0
    );
    for id in 1..=100 {
        assert_eq!(
            fake_execute(
                runtime,
                &mut fake,
                &format!("INSERT INTO {fake_table} VALUES ({id}, 'benchmark')"),
            ),
            1
        );
        assert_eq!(
            postgres
                .execute(
                    &format!("INSERT INTO {postgres_table} VALUES ({id}, 'benchmark')"),
                    &[],
                )
                .unwrap(),
            1
        );
    }
    let fake_select = format!("SELECT * FROM {fake_table}");
    let postgres_select = format!("SELECT * FROM {postgres_table}");
    let mut group = criterion.benchmark_group("select_100_rows");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            let result = fake_query(runtime, &mut fake, &fake_select);
            assert_eq!(result.len(), 100);
            black_box(result);
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            let result = postgres.simple_query(&postgres_select).unwrap();
            assert_eq!(
                result
                    .iter()
                    .filter(|message| matches!(message, SimpleQueryMessage::Row(_)))
                    .count(),
                100
            );
            black_box(result);
        });
    });
    group.finish();

    let fake_select =
        format!("SELECT id, name FROM {fake_table} ORDER BY id DESC LIMIT 10 OFFSET 40");
    let postgres_select =
        format!("SELECT id, name FROM {postgres_table} ORDER BY id DESC LIMIT 10 OFFSET 40");
    let mut group = criterion.benchmark_group("limit_offset_ordered_100_rows");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            let result = fake_query(runtime, &mut fake, &fake_select);
            assert_eq!(result.len(), 10);
            black_box(result);
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            let result = postgres.simple_query(&postgres_select).unwrap();
            assert_eq!(
                result
                    .iter()
                    .filter(|message| matches!(message, SimpleQueryMessage::Row(_)))
                    .count(),
                10
            );
            black_box(result);
        });
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn order_by_benchmark(criterion: &mut Criterion, runtime: &Runtime, postgres: &mut Client) {
    let fake_table = unique_table_name("order_by_fake");
    let postgres_table = unique_table_name("order_by_postgres");
    let mut fake = PgFakeConnection::new(Db::new());
    assert_eq!(
        fake_execute(
            runtime,
            &mut fake,
            &format!("CREATE TABLE {fake_table} (id INTEGER, bucket INTEGER)"),
        ),
        0
    );
    assert_eq!(
        postgres
            .execute(
                &format!("CREATE TABLE {postgres_table} (id INTEGER, bucket INTEGER)"),
                &[],
            )
            .unwrap(),
        0
    );
    for id in 1..=100 {
        let bucket = if id % 10 == 0 {
            "NULL".to_owned()
        } else {
            (id % 10).to_string()
        };
        assert_eq!(
            fake_execute(
                runtime,
                &mut fake,
                &format!("INSERT INTO {fake_table} VALUES ({id}, {bucket})"),
            ),
            1
        );
        assert_eq!(
            postgres
                .execute(
                    &format!("INSERT INTO {postgres_table} VALUES ({id}, {bucket})"),
                    &[],
                )
                .unwrap(),
            1
        );
    }
    let fake_select =
        format!("SELECT id, bucket FROM {fake_table} ORDER BY bucket DESC NULLS LAST, id ASC");
    let postgres_select =
        format!("SELECT id, bucket FROM {postgres_table} ORDER BY bucket DESC NULLS LAST, id ASC");
    let mut group = criterion.benchmark_group("order_by_100_rows");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            let result = fake_query(runtime, &mut fake, &fake_select);
            assert_eq!(result.len(), 100);
            black_box(result);
        });
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| {
            let result = postgres.simple_query(&postgres_select).unwrap();
            assert_eq!(
                result
                    .iter()
                    .filter(|message| matches!(message, SimpleQueryMessage::Row(_)))
                    .count(),
                100
            );
            black_box(result);
        });
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn core_vs_sqlx_benchmark(criterion: &mut Criterion, runtime: &Runtime) {
    let core_table = unique_table_name("adapter_core");
    let mut core = Db::new().session();
    core_execute(
        &mut core,
        &format!("CREATE TABLE {core_table} (id INTEGER, name TEXT)"),
    );
    core_execute(&mut core, &insert_values_sql(&core_table, 100));
    core_execute(&mut core, "BEGIN");
    let core_query = format!("SELECT id, name FROM {core_table} ORDER BY id");

    let sqlx_table = unique_table_name("adapter_sqlx");
    let mut sqlx = PgFakeConnection::new(Db::new());
    fake_execute(
        runtime,
        &mut sqlx,
        &format!("CREATE TABLE {sqlx_table} (id INTEGER, name TEXT)"),
    );
    fake_execute(runtime, &mut sqlx, &insert_values_sql(&sqlx_table, 100));
    fake_execute(runtime, &mut sqlx, "BEGIN");
    let sqlx_query = format!("SELECT id, name FROM {sqlx_table} ORDER BY id");

    let mut group = criterion.benchmark_group("adapter_overhead_select_100_rows");
    group.throughput(Throughput::Elements(100));
    group.bench_function("core", |benchmark| {
        benchmark.iter(|| {
            let result = core.query(&core_query, &[]).unwrap();
            assert_eq!(result.rows.len(), 100);
            black_box(result);
        });
    });
    group.bench_function("sqlx", |benchmark| {
        benchmark.iter(|| {
            let result = fake_query(runtime, &mut sqlx, &sqlx_query);
            assert_eq!(result.len(), 100);
            black_box(result);
        });
    });
    group.finish();

    core_execute(&mut core, "ROLLBACK");
    fake_execute(runtime, &mut sqlx, "ROLLBACK");
}

fn parsed_vs_prepared_benchmark(criterion: &mut Criterion) {
    let table = unique_table_name("prepared_core");
    let mut session = Db::new().session();
    core_execute(
        &mut session,
        &format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY, name TEXT)"),
    );
    core_execute(&mut session, &insert_values_sql(&table, 100));
    let query = format!("SELECT name FROM {table} WHERE id = $1");
    let prepared = session.prepare(&query).unwrap();
    core_execute(&mut session, "BEGIN");
    let parameters = [Value::Int4(50)];

    let mut group = criterion.benchmark_group("core_parsed_vs_prepared_point_select");
    group.bench_function("parse_and_analyze", |benchmark| {
        benchmark.iter(|| {
            let result = session.query(&query, &parameters).unwrap();
            assert_eq!(result.rows.len(), 1);
            black_box(result);
        });
    });
    group.bench_function("prepared_reuse", |benchmark| {
        benchmark.iter(|| {
            let result = session.query_prepared(&prepared, &parameters).unwrap();
            assert_eq!(result.rows.len(), 1);
            black_box(result);
        });
    });
    group.finish();

    core_execute(&mut session, "ROLLBACK");
}

fn transaction_history_benchmark(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("transaction_history_point_select");
    for completed in [1_u64, 100, 10_000, 100_000] {
        let mut session = Db::new().session();
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
                    assert_eq!(result.rows.len(), 1);
                    black_box(result);
                });
            },
        );
        transaction.rollback().unwrap();
    }
    group.finish();
}

fn mvcc_version_chain_benchmark(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("mvcc_old_snapshot_read");
    for updates in [1_u64, 100, 10_000] {
        let db = Db::new();
        let mut setup = db.session();
        core_execute(
            &mut setup,
            "CREATE TABLE mvcc_chain_probe (id INTEGER PRIMARY KEY, amount INTEGER)",
        );
        core_execute(&mut setup, "INSERT INTO mvcc_chain_probe VALUES (1, 0)");
        let mut reader = db.session();
        let statement = reader
            .prepare("SELECT amount FROM mvcc_chain_probe WHERE id = 1")
            .unwrap();
        let mut reader = reader.begin_with(IsolationLevel::RepeatableRead).unwrap();
        assert_eq!(
            reader.query_prepared(&statement, &[]).unwrap().rows.len(),
            1
        );

        let mut updater = db.session();
        let update = updater
            .prepare("UPDATE mvcc_chain_probe SET amount = amount + 1 WHERE id = 1")
            .unwrap();
        for _ in 0..updates {
            assert_eq!(updater.execute_prepared(&update, &[]).unwrap(), 1);
        }

        group.bench_with_input(
            BenchmarkId::from_parameter(updates),
            &updates,
            |benchmark, _| {
                benchmark.iter(|| {
                    let result = reader.query_prepared(&statement, &[]).unwrap();
                    assert_eq!(result.rows, vec![vec![Value::Int4(0)]]);
                    black_box(result);
                });
            },
        );
        reader.rollback().unwrap();
    }
    group.finish();
}

fn indexed_vs_scan_benchmark(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("point_lookup_index_vs_scan");
    for rows in [100_usize, 10_000] {
        group.throughput(Throughput::Elements(rows as u64));

        let mut indexed = Db::new().session();
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
                    assert_eq!(result.rows.len(), 1);
                    black_box(result);
                });
            },
        );
        indexed.rollback().unwrap();

        let mut scanned = Db::new().session();
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
                    assert_eq!(result.rows.len(), 1);
                    black_box(result);
                });
            },
        );
        scanned.rollback().unwrap();
    }
    group.finish();
}

fn concurrency_benchmark(criterion: &mut Criterion, runtime: &Runtime) {
    let db = Db::new();
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

    let mut group = criterion.benchmark_group("concurrent_uncontended_reads");
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

    let db = Db::builder().lock_timeout(Duration::from_secs(2)).build();
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
    let mut group = criterion.benchmark_group("concurrent_same_row_contention");
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

fn benchmarks(criterion: &mut Criterion) {
    let mut postgres = postgres_benchmark();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    create_table_benchmark(criterion, &runtime, &mut postgres.client);
    insert_benchmark(criterion, &runtime, &mut postgres.client);
    update_benchmark(criterion, &runtime, &mut postgres.client);
    delete_benchmark(criterion, &runtime, &mut postgres.client);
    transaction_benchmark(criterion, &runtime, &mut postgres.client);
    repeatable_read_benchmark(criterion, &runtime, &mut postgres.client);
    select_benchmark(criterion, &runtime, &mut postgres.client);
    order_by_benchmark(criterion, &runtime, &mut postgres.client);
    core_vs_sqlx_benchmark(criterion, &runtime);
    parsed_vs_prepared_benchmark(criterion);
    transaction_history_benchmark(criterion);
    mvcc_version_chain_benchmark(criterion);
    indexed_vs_scan_benchmark(criterion);
    concurrency_benchmark(criterion, &runtime);
}

criterion_group!(workloads, benchmarks);
criterion_main!(workloads);
