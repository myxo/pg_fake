use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{AssertSqlSafe, Connection, Row};
use sqlx_postgres::PgConnection;

mod common;

fn get_sqlstate(error: sqlx::Error) -> String {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .expect("database errors must expose SQLSTATE")
        .into_owned()
}

async fn assert_execution_matches(
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
    sql: &str,
) {
    let expected = sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(&mut *postgres)
        .await
        .map(|_| ())
        .map_err(get_sqlstate);
    let actual = sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(&mut *fake)
        .await
        .map(|_| ())
        .map_err(get_sqlstate);
    assert_eq!(actual, expected, "SQL: {sql}");
}

async fn get_i64_postgres(connection: &mut PgConnection, sql: &str) -> i64 {
    sqlx::raw_sql(AssertSqlSafe(sql))
        .fetch_one(connection)
        .await
        .unwrap()
        .try_get(0)
        .unwrap()
}

async fn get_i64_fake(connection: &mut PgFakeConnection, sql: &str) -> i64 {
    sqlx::raw_sql(AssertSqlSafe(sql))
        .fetch_one(connection)
        .await
        .unwrap()
        .try_get(0)
        .unwrap()
}

#[test]
fn explicit_table_and_sequence_ddl_matches_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let table = format!("pg_fake_transactional_ddl_{}", std::process::id());
        let sequence = format!("{table}_id_seq");
        let cleanup = format!("DROP TABLE IF EXISTS {table}");
        sqlx::raw_sql(AssertSqlSafe(cleanup.as_str()))
            .execute(&mut postgres)
            .await
            .unwrap();

        for sql in [
            "BEGIN".to_owned(),
            format!("CREATE TABLE {table} (id SERIAL PRIMARY KEY, value INTEGER UNIQUE)"),
            format!("INSERT INTO {table} (value) VALUES (10)"),
            "ROLLBACK".to_owned(),
            format!("SELECT * FROM {table}"),
            format!("CREATE TABLE {table} (id SERIAL PRIMARY KEY, value INTEGER UNIQUE)"),
        ] {
            assert_execution_matches(&mut postgres, &mut fake, &sql).await;
        }

        let first = format!("SELECT nextval('{sequence}')");
        assert_eq!(
            get_i64_fake(&mut fake, &first).await,
            get_i64_postgres(&mut postgres, &first).await
        );
        assert_execution_matches(&mut postgres, &mut fake, "BEGIN").await;
        assert_eq!(
            get_i64_fake(&mut fake, &first).await,
            get_i64_postgres(&mut postgres, &first).await
        );
        assert_execution_matches(&mut postgres, &mut fake, &format!("DROP TABLE {table}")).await;
        let missing = format!("SELECT nextval('{sequence}')");
        assert_execution_matches(&mut postgres, &mut fake, &missing).await;
        assert_execution_matches(&mut postgres, &mut fake, "ROLLBACK").await;
        assert_eq!(
            get_i64_fake(&mut fake, &first).await,
            get_i64_postgres(&mut postgres, &first).await
        );

        sqlx::raw_sql(AssertSqlSafe(cleanup.as_str()))
            .execute(&mut postgres)
            .await
            .unwrap();
    });
}

#[test]
fn repeatable_read_ddl_conflicts_match_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut postgres_writer = PgConnection::connect(&server.url).await.unwrap();
        let db = Db::create();
        let mut fake = PgFakeConnection::new(db.clone());
        let mut fake_writer = PgFakeConnection::new(db);
        let table = format!("pg_fake_repeatable_ddl_{}", std::process::id());
        let cleanup = format!("DROP TABLE IF EXISTS {table}");
        sqlx::raw_sql(AssertSqlSafe(cleanup.as_str()))
            .execute(&mut postgres)
            .await
            .unwrap();

        assert_execution_matches(
            &mut postgres,
            &mut fake,
            "BEGIN ISOLATION LEVEL REPEATABLE READ",
        )
        .await;
        assert_execution_matches(&mut postgres, &mut fake, "SELECT 1").await;
        assert_execution_matches(
            &mut postgres_writer,
            &mut fake_writer,
            &format!("CREATE TABLE {table} (id INTEGER)"),
        )
        .await;
        assert_execution_matches(
            &mut postgres,
            &mut fake,
            &format!("CREATE TABLE {table} (id INTEGER)"),
        )
        .await;
        assert_execution_matches(&mut postgres, &mut fake, "ROLLBACK").await;

        assert_execution_matches(
            &mut postgres,
            &mut fake,
            "BEGIN ISOLATION LEVEL REPEATABLE READ",
        )
        .await;
        assert_execution_matches(&mut postgres, &mut fake, "SELECT 1").await;
        assert_execution_matches(
            &mut postgres_writer,
            &mut fake_writer,
            &format!("DROP TABLE {table}"),
        )
        .await;
        assert_execution_matches(&mut postgres, &mut fake, &format!("DROP TABLE {table}")).await;
        assert_execution_matches(&mut postgres, &mut fake, "ROLLBACK").await;

        sqlx::raw_sql(AssertSqlSafe(cleanup.as_str()))
            .execute(&mut postgres)
            .await
            .unwrap();
    });
}

#[tokio::test]
async fn closing_a_connection_aborts_open_ddl() {
    let db = Db::create();
    let mut abandoned = PgFakeConnection::new(db.clone());
    sqlx::raw_sql(AssertSqlSafe("BEGIN; CREATE TABLE abandoned (id INTEGER)"))
        .execute(&mut abandoned)
        .await
        .unwrap();
    abandoned.close().await.unwrap();

    let mut successor = PgFakeConnection::new(db);
    sqlx::raw_sql(AssertSqlSafe("CREATE TABLE abandoned (id INTEGER)"))
        .execute(&mut successor)
        .await
        .unwrap();
}
