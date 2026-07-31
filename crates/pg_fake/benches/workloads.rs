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
use pg_fake::api::Db;
use postgres::{Client, NoTls, SimpleQueryMessage};
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;

static TABLE_NUMBER: AtomicU64 = AtomicU64::new(1);
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

struct PostgresBenchmark {
    client: Client,
    _container: Option<Container<Postgres>>,
}

fn postgres_benchmark() -> PostgresBenchmark {
    let _environment_lock = ENVIRONMENT_LOCK
        .lock()
        .expect("environment mutex must not be poisoned");
    if let Ok(url) = env::var("PG_FAKE_BENCHMARK_DATABASE_URL") {
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

fn create_table_benchmark(criterion: &mut Criterion, postgres: &mut Client) {
    let fake_table = unique_table_name("create_fake");
    let postgres_table = unique_table_name("create_postgres");
    let fake_create = format!("CREATE TABLE {fake_table} (id INTEGER, name TEXT)");
    let fake_drop = format!("DROP TABLE {fake_table}");
    let postgres_create = format!("CREATE TABLE {postgres_table} (id INTEGER, name TEXT)");
    let postgres_drop = format!("DROP TABLE {postgres_table}");
    let db = Db::new();
    let mut fake = db.session();
    let mut group = criterion.benchmark_group("create_table");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            assert_eq!(fake.execute(&fake_create).unwrap(), 0);
            assert_eq!(fake.execute(&fake_drop).unwrap(), 1);
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

fn insert_benchmark(criterion: &mut Criterion, postgres: &mut Client) {
    let fake_table = unique_table_name("insert_fake");
    let postgres_table = unique_table_name("insert_postgres");
    let db = Db::new();
    let mut fake = db.session();
    assert_eq!(
        fake.execute(&format!(
            "CREATE TABLE {fake_table} (id INTEGER, name TEXT)"
        ))
        .unwrap(),
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
    let mut fake_id = 0;
    let mut postgres_id = 0;
    let mut group = criterion.benchmark_group("insert_row");

    group.bench_function("pg_fake", |benchmark| {
        benchmark.iter(|| {
            fake_id += 1;
            assert_eq!(
                fake.execute(&format!(
                    "INSERT INTO {fake_table} VALUES ({fake_id}, 'benchmark')"
                ))
                .unwrap(),
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
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn update_benchmark(criterion: &mut Criterion, postgres: &mut Client) {
    let fake_table = unique_table_name("update_fake");
    let postgres_table = unique_table_name("update_postgres");
    let db = Db::new();
    let mut fake = db.session();
    assert_eq!(
        fake.execute(&format!(
            "CREATE TABLE {fake_table} (id INTEGER, amount INTEGER)"
        ))
        .unwrap(),
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
        fake.execute(&format!("INSERT INTO {fake_table} VALUES (1, 0)"))
            .unwrap(),
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
        benchmark.iter(|| assert_eq!(fake.execute(&fake_update).unwrap(), 1));
    });
    group.bench_function("postgres_18", |benchmark| {
        benchmark.iter(|| assert_eq!(postgres.execute(&postgres_update, &[]).unwrap(), 1));
    });
    group.finish();
    postgres
        .execute(&format!("DROP TABLE {postgres_table}"), &[])
        .unwrap();
}

fn delete_benchmark(criterion: &mut Criterion, postgres: &mut Client) {
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
                let db = Db::new();
                let mut fake = db.session();
                fake.execute(&format!("CREATE TABLE {fake_table} (id INTEGER)"))
                    .unwrap();
                fake.execute(&format!("INSERT INTO {fake_table} VALUES ({id})"))
                    .unwrap();
                let delete = format!("DELETE FROM {fake_table} WHERE id = {id}");
                let started = Instant::now();
                assert_eq!(fake.execute(&delete).unwrap(), 1);
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

fn transaction_benchmark(criterion: &mut Criterion, postgres: &mut Client) {
    let fake_table = unique_table_name("transaction_fake");
    let postgres_table = unique_table_name("transaction_postgres");
    let db = Db::new();
    let mut fake = db.session();
    assert_eq!(
        fake.execute(&format!("CREATE TABLE {fake_table} (id INTEGER)"))
            .unwrap(),
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
            assert_eq!(fake.execute("BEGIN").unwrap(), 0);
            assert_eq!(
                fake.execute(&format!("INSERT INTO {fake_table} VALUES ({fake_id})"))
                    .unwrap(),
                1
            );
            assert_eq!(fake.execute("COMMIT").unwrap(), 0);
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

fn select_benchmark(criterion: &mut Criterion, postgres: &mut Client) {
    let fake_table = unique_table_name("select_fake");
    let postgres_table = unique_table_name("select_postgres");
    let db = Db::new();
    let mut fake = db.session();
    assert_eq!(
        fake.execute(&format!(
            "CREATE TABLE {fake_table} (id INTEGER, name TEXT)"
        ))
        .unwrap(),
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
            fake.execute(&format!(
                "INSERT INTO {fake_table} VALUES ({id}, 'benchmark')"
            ))
            .unwrap(),
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
            let result = fake.query(&fake_select, &[]).unwrap();
            assert_eq!(result.rows.len(), 100);
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

    create_table_benchmark(criterion, &mut postgres.client);
    insert_benchmark(criterion, &mut postgres.client);
    update_benchmark(criterion, &mut postgres.client);
    delete_benchmark(criterion, &mut postgres.client);
    transaction_benchmark(criterion, &mut postgres.client);
    select_benchmark(criterion, &mut postgres.client);
}

criterion_group!(workloads, benchmarks);
criterion_main!(workloads);
