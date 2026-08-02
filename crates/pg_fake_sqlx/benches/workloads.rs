use std::{
    env,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
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
    let mut fake = PgFakeConnection::new(Db::new());
    assert_eq!(
        fake_execute(
            runtime,
            &mut fake,
            &format!("CREATE TABLE {fake_table} (id INTEGER, amount INTEGER)"),
        ),
        0
    );
    assert_eq!(
        postgres
            .execute(
                &format!("CREATE TABLE {postgres_table} (id INTEGER, amount INTEGER)"),
                &[],
            )
            .unwrap(),
        0
    );
    assert_eq!(
        fake_execute(
            runtime,
            &mut fake,
            &format!("INSERT INTO {fake_table} VALUES (1, 0)"),
        ),
        1
    );
    assert_eq!(
        postgres
            .execute(&format!("INSERT INTO {postgres_table} VALUES (1, 0)"), &[])
            .unwrap(),
        1
    );
    let fake_update = format!("UPDATE {fake_table} SET amount = amount + 1 WHERE id = 1");
    let postgres_update = format!("UPDATE {postgres_table} SET amount = amount + 1 WHERE id = 1");
    let mut group = criterion.benchmark_group("update_row");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| assert_eq!(fake_execute(runtime, &mut fake, &fake_update), 1));
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| assert_eq!(postgres.execute(&postgres_update, &[]).unwrap(), 1));
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
}

criterion_group!(workloads, benchmarks);
criterion_main!(workloads);
