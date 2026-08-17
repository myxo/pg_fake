use std::{env, path::PathBuf, str::FromStr, time::Duration};

use bigdecimal::BigDecimal;
use pg_fake::api::Db;
use pg_fake_sqlx::{PgFakeConnectOptions, PgFakeConnection, PgFakePoolOptions};
use sqlx::{AssertSqlSafe, Column, Connection, Executor, Row, SqlStr, Statement, TypeInfo};
use sqlx_postgres::PgConnection;
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres as PostgresImage;

struct PostgresServer {
    url: String,
    _container: Option<Container<PostgresImage>>,
}

#[tokio::test]
async fn sqlx_queries_map_all_phase_one_types() {
    let mut connection = PgFakeConnection::new(Db::create());
    connection
        .execute(
            "CREATE TABLE values_table (
                enabled boolean,
                small smallint,
                regular integer,
                large bigint,
                real_value real,
                double_value double precision,
                decimal_value numeric(8, 2),
                text_value text,
                varying_value varchar(20),
                character_value char(4),
                bytes bytea
            )",
        )
        .await
        .unwrap();

    sqlx::query("INSERT INTO values_table VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)")
        .bind(true)
        .bind(2_i16)
        .bind(4_i32)
        .bind(8_i64)
        .bind(1.5_f32)
        .bind(2.5_f64)
        .bind(BigDecimal::from_str("12.34").unwrap())
        .bind("text")
        .bind(String::from("varying"))
        .bind("x")
        .bind(vec![0_u8, 1, 255])
        .execute(&mut connection)
        .await
        .unwrap();

    let row = sqlx::query("SELECT * FROM values_table")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert!(row.get::<bool, _>("enabled"));
    assert_eq!(row.get::<i16, _>("small"), 2);
    assert_eq!(row.get::<i32, _>("regular"), 4);
    assert_eq!(row.get::<i64, _>("large"), 8);
    assert_eq!(row.get::<f32, _>("real_value"), 1.5);
    assert_eq!(row.get::<f64, _>("double_value"), 2.5);
    assert_eq!(
        row.get::<BigDecimal, _>("decimal_value"),
        BigDecimal::from_str("12.34").unwrap()
    );
    assert_eq!(row.get::<String, _>("text_value"), "text");
    assert_eq!(row.get::<String, _>("varying_value"), "varying");
    assert_eq!(row.get::<String, _>("character_value"), "x   ");
    assert_eq!(row.get::<Vec<u8>, _>("bytes"), vec![0, 1, 255]);
    assert_eq!(row.columns()[6].type_info().name(), "NUMERIC");
}

#[tokio::test]
async fn sqlx_uuid_values_round_trip() {
    let mut connection = PgFakeConnection::new(Db::create());
    let value = uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7").unwrap();
    connection
        .execute("CREATE TABLE uuid_values (id UUID)")
        .await
        .unwrap();
    sqlx::query("INSERT INTO uuid_values VALUES ($1)")
        .bind(value)
        .execute(&mut connection)
        .await
        .unwrap();
    let row = sqlx::query("SELECT id FROM uuid_values")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<uuid::Uuid, _>(0), value);
    assert_eq!(row.columns()[0].type_info().name(), "UUID");
}

#[tokio::test]
async fn sqlx_timestamp_values_round_trip() {
    let mut connection = PgFakeConnection::new(Db::create());
    let local = chrono::NaiveDate::from_ymd_opt(2024, 2, 29)
        .unwrap()
        .and_hms_micro_opt(12, 34, 56, 789_000)
        .unwrap();
    let instant = chrono::DateTime::parse_from_rfc3339("2024-02-29T12:34:56+03:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    connection
        .execute("CREATE TABLE timestamps (local TIMESTAMP, instant TIMESTAMPTZ)")
        .await
        .unwrap();
    sqlx::query("INSERT INTO timestamps VALUES ($1, $2)")
        .bind(local)
        .bind(instant)
        .execute(&mut connection)
        .await
        .unwrap();
    let row = sqlx::query("SELECT local, instant FROM timestamps")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<chrono::NaiveDateTime, _>(0), local);
    assert_eq!(row.get::<chrono::DateTime<chrono::Utc>, _>(1), instant);
    assert_eq!(row.columns()[0].type_info().name(), "TIMESTAMP");
    assert_eq!(row.columns()[1].type_info().name(), "TIMESTAMPTZ");
}

#[tokio::test]
async fn sqlx_intervals_round_trip() {
    let mut connection = PgFakeConnection::new(Db::create());
    let interval = pg_fake::value::PgInterval {
        months: 1,
        days: 2,
        micros: 3_000_000,
    };
    connection
        .execute("CREATE TABLE intervals (value INTERVAL)")
        .await
        .unwrap();
    sqlx::query("INSERT INTO intervals VALUES ($1)")
        .bind(interval)
        .execute(&mut connection)
        .await
        .unwrap();
    let row = sqlx::query("SELECT value FROM intervals")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<pg_fake::value::PgInterval, _>(0), interval);
    assert_eq!(row.columns()[0].type_info().name(), "INTERVAL");
}

#[tokio::test]
async fn prepared_statements_transactions_and_pools_use_the_sqlx_api() {
    let db = Db::create();
    let pool = PgFakePoolOptions::new()
        .max_connections(2)
        .connect_with(PgFakeConnectOptions::new(db))
        .await
        .unwrap();
    pool.execute("CREATE TABLE users (id integer PRIMARY KEY, name text)")
        .await
        .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO users VALUES ($1, $2)")
        .bind(1_i32)
        .bind("committed")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let mut connection = pool.acquire().await.unwrap();
    let statement = connection
        .prepare(SqlStr::from_static("SELECT name FROM users WHERE id = $1"))
        .await
        .unwrap();
    let row = statement
        .query()
        .bind(1_i32)
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>(0), "committed");
    assert_eq!(statement.parameters().unwrap().left().unwrap().len(), 1);
    assert_eq!(statement.columns()[0].name(), "name");
    assert_eq!(statement.columns()[0].type_info().name(), "TEXT");

    let mut transaction = connection.begin().await.unwrap();
    sqlx::query("INSERT INTO users VALUES ($1, $2)")
        .bind(2_i32)
        .bind("rolled back")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    assert!(
        sqlx::query("SELECT id FROM users WHERE id = $1")
            .bind(2_i32)
            .fetch_optional(&mut *connection)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn sqlx_fetches_and_executes_returning_mutations() {
    let mut connection = PgFakeConnection::new(Db::create());
    connection
        .execute(
            "CREATE TABLE returning_rows (
                 id INTEGER PRIMARY KEY,
                 label VARCHAR(12) DEFAULT 'new'
             )",
        )
        .await
        .unwrap();

    let rows = sqlx::query(
        "INSERT INTO returning_rows (id, label) VALUES ($1, $2), ($3, DEFAULT)
         RETURNING id, label",
    )
    .bind(1_i32)
    .bind("first")
    .bind(2_i32)
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<i32, _>("id"), 1);
    assert_eq!(rows[0].get::<String, _>("label"), "first");
    assert_eq!(rows[1].get::<String, _>("label"), "new");
    assert_eq!(rows[1].columns()[1].type_info().name(), "VARCHAR");

    let statement = connection
        .prepare(SqlStr::from_static(
            "UPDATE returning_rows SET label = $1 RETURNING id, label AS updated_label",
        ))
        .await
        .unwrap();
    assert_eq!(statement.parameters().unwrap().left().unwrap().len(), 1);
    assert_eq!(statement.columns()[0].name(), "id");
    assert_eq!(statement.columns()[1].name(), "updated_label");
    assert_eq!(statement.columns()[1].type_info().name(), "VARCHAR");
    let updated = statement
        .query()
        .bind("updated")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert_eq!(updated.len(), 2);

    let affected = sqlx::query("DELETE FROM returning_rows RETURNING id")
        .execute(&mut connection)
        .await
        .unwrap();
    assert_eq!(affected.rows_affected(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row_lock_waits_run_on_the_blocking_pool() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = PgFakeConnection::new(db.clone());
    let mut second = PgFakeConnection::new(db);
    first
        .execute("CREATE TABLE counters (id integer PRIMARY KEY, value integer)")
        .await
        .unwrap();
    first
        .execute("INSERT INTO counters VALUES (1, 0)")
        .await
        .unwrap();

    let mut transaction = first.begin().await.unwrap();
    sqlx::query("UPDATE counters SET value = 1 WHERE id = 1")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let waiter = tokio::spawn(async move {
        sqlx::query("UPDATE counters SET value = 2 WHERE id = 1")
            .execute(&mut second)
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished());
    transaction.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlx_error_category_matches_postgres() {
    let server = tokio::task::spawn_blocking(start_postgres_server)
        .await
        .unwrap();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    sqlx::raw_sql(AssertSqlSafe(
        "CREATE TEMP TABLE pg_fake_sqlx_unique_values (id integer UNIQUE);
         INSERT INTO pg_fake_sqlx_unique_values VALUES (1)",
    ))
    .execute(&mut postgres)
    .await
    .unwrap();
    let expected = postgres
        .execute("INSERT INTO pg_fake_sqlx_unique_values VALUES (1)")
        .await
        .unwrap_err()
        .as_database_error()
        .and_then(|error| error.code())
        .expect("PostgreSQL unique violations must have a SQLSTATE")
        .into_owned();
    let mut connection = PgFakeConnection::new(Db::create());
    connection
        .execute("CREATE TABLE unique_values (id integer UNIQUE)")
        .await
        .unwrap();
    connection
        .execute("INSERT INTO unique_values VALUES (1)")
        .await
        .unwrap();
    let error = connection
        .execute("INSERT INTO unique_values VALUES (1)")
        .await
        .unwrap_err();
    let database_error = error.as_database_error().unwrap();
    assert_eq!(database_error.code().as_deref(), Some(expected.as_str()));
    assert!(database_error.is_unique_violation());
    drop(postgres);
    tokio::task::spawn_blocking(move || drop(server))
        .await
        .unwrap();
}

fn start_postgres_server() -> PostgresServer {
    let configured_url = env::var("PG_FAKE_DATABASE_URL").ok();
    if configured_url.is_none() && env::var_os("DOCKER_HOST").is_none() {
        let socket = PathBuf::from(env::var_os("HOME").expect("HOME must be set"))
            .join(".colima/default/docker.sock");
        if socket.exists() {
            unsafe { env::set_var("DOCKER_HOST", format!("unix://{}", socket.display())) };
        }
    }
    let container = configured_url.is_none().then(|| {
        PostgresImage::default()
            .with_tag("18")
            .start()
            .expect("must start PostgreSQL 18 container")
    });
    let url = configured_url.unwrap_or_else(|| {
        let container = container.as_ref().unwrap();
        format!(
            "postgresql://postgres:postgres@{}:{}/postgres",
            container.get_host().unwrap(),
            container.get_host_port_ipv4(5432).unwrap()
        )
    });
    PostgresServer {
        url,
        _container: container,
    }
}
