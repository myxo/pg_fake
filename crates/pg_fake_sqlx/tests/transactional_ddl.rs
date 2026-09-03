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

async fn get_text_postgres(connection: &mut PgConnection, sql: &str) -> String {
    sqlx::raw_sql(AssertSqlSafe(sql))
        .fetch_one(connection)
        .await
        .unwrap()
        .try_get(0)
        .unwrap()
}

async fn get_text_fake(connection: &mut PgFakeConnection, sql: &str) -> String {
    sqlx::raw_sql(AssertSqlSafe(sql))
        .fetch_one(connection)
        .await
        .unwrap()
        .try_get(0)
        .unwrap()
}

async fn assert_i64_matches(postgres: &mut PgConnection, fake: &mut PgFakeConnection, sql: &str) {
    assert_eq!(
        get_i64_fake(fake, sql).await,
        get_i64_postgres(postgres, sql).await
    );
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

#[test]
fn qualified_and_temporary_relations_match_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres_first = PgConnection::connect(&server.url).await.unwrap();
        let mut postgres_second = PgConnection::connect(&server.url).await.unwrap();
        let db = Db::create();
        let mut fake_first = PgFakeConnection::new(db.clone());
        let mut fake_second = PgFakeConnection::new(db);
        let table = format!("pg_fake_temp_{}", std::process::id());
        let cleanup = format!("DROP TABLE IF EXISTS public.{table}");
        sqlx::raw_sql(AssertSqlSafe(cleanup.as_str()))
            .execute(&mut postgres_first)
            .await
            .unwrap();

        for sql in [
            format!("CREATE TABLE public.{table} (id BIGINT)"),
            format!("INSERT INTO public.{table} VALUES (1)"),
            format!("CREATE TEMP TABLE {table} (id BIGINT)"),
            format!("INSERT INTO pg_temp.{table} VALUES (2)"),
        ] {
            assert_execution_matches(&mut postgres_first, &mut fake_first, &sql).await;
        }
        for sql in [
            format!("CREATE TEMPORARY TABLE pg_temp.{table} (id BIGINT)"),
            format!("INSERT INTO {table} VALUES (3)"),
        ] {
            assert_execution_matches(&mut postgres_second, &mut fake_second, &sql).await;
        }

        assert_i64_matches(
            &mut postgres_first,
            &mut fake_first,
            &format!("SELECT id FROM {table}"),
        )
        .await;
        assert_i64_matches(
            &mut postgres_first,
            &mut fake_first,
            &format!("SELECT id FROM public.{table}"),
        )
        .await;
        assert_i64_matches(
            &mut postgres_second,
            &mut fake_second,
            &format!("SELECT id FROM pg_temp.{table}"),
        )
        .await;

        let fleeting = format!("{table}_fleeting");
        for sql in [
            "BEGIN".to_owned(),
            format!("CREATE TEMP TABLE {fleeting} (id INTEGER) ON COMMIT DROP"),
            "COMMIT".to_owned(),
            format!("SELECT * FROM pg_temp.{fleeting}"),
        ] {
            assert_execution_matches(&mut postgres_first, &mut fake_first, &sql).await;
        }

        let serial_table = format!("{table}_serial");
        for sql in [
            format!("CREATE TABLE public.{serial_table} (id SERIAL)"),
            format!("CREATE TEMP TABLE {serial_table} (id SERIAL)"),
        ] {
            assert_execution_matches(&mut postgres_first, &mut fake_first, &sql).await;
        }
        let serial_lookup = format!("SELECT pg_get_serial_sequence('public.{serial_table}', 'id')");
        assert_eq!(
            get_text_fake(&mut fake_first, &serial_lookup).await,
            get_text_postgres(&mut postgres_first, &serial_lookup).await
        );

        let default_sequence = format!("{table}_default_ids");
        let default_table = format!("{table}_defaulted");
        for sql in [
            format!("CREATE SEQUENCE public.{default_sequence}"),
            format!(
                "CREATE TABLE public.{default_table} \
                 (id BIGINT DEFAULT nextval('{default_sequence}'))"
            ),
            format!("CREATE TEMP SEQUENCE {default_sequence} START WITH 100"),
        ] {
            assert_execution_matches(&mut postgres_first, &mut fake_first, &sql).await;
        }
        assert_i64_matches(
            &mut postgres_first,
            &mut fake_first,
            &format!("INSERT INTO public.{default_table} DEFAULT VALUES RETURNING id"),
        )
        .await;
        assert_execution_matches(
            &mut postgres_first,
            &mut fake_first,
            &format!("DROP SEQUENCE public.{default_sequence}"),
        )
        .await;
        assert_execution_matches(
            &mut postgres_first,
            &mut fake_first,
            &format!(
                "CREATE TABLE public.{table}_missing_default \
                 (id BIGINT DEFAULT nextval('{table}_missing_sequence'))"
            ),
        )
        .await;

        let quoted_table = format!("PgFakeMixed{}", std::process::id());
        let quoted_create = format!("CREATE TABLE public.\"{quoted_table}\" (\"ID\" SERIAL)");
        assert_execution_matches(&mut postgres_first, &mut fake_first, &quoted_create).await;
        let quoted_lookup =
            format!("SELECT pg_get_serial_sequence('public.\"{quoted_table}\"', 'ID')");
        let fake_sequence = get_text_fake(&mut fake_first, &quoted_lookup).await;
        let postgres_sequence = get_text_postgres(&mut postgres_first, &quoted_lookup).await;
        assert_eq!(fake_sequence, postgres_sequence);
        assert_i64_matches(
            &mut postgres_first,
            &mut fake_first,
            &format!("SELECT nextval('{fake_sequence}')"),
        )
        .await;
        for sql in [
            format!("SELECT pg_get_serial_sequence('{table}_missing_schema.items', 'id')"),
            format!("SELECT pg_get_serial_sequence('public.{table}_missing', 'id')"),
            format!("SELECT pg_get_serial_sequence('public.{default_sequence}', 'id')"),
        ] {
            assert_execution_matches(&mut postgres_first, &mut fake_first, &sql).await;
        }

        let temporary_parent = format!("{table}_temporary_parent");
        let permanent_parent = format!("{table}_permanent_parent");
        for sql in [
            format!("CREATE TEMP TABLE {temporary_parent} (id INTEGER PRIMARY KEY)"),
            format!(
                "CREATE TABLE public.{table}_permanent_child \
                 (parent_id INTEGER REFERENCES {temporary_parent}(id))"
            ),
            format!("CREATE TABLE public.{permanent_parent} (id INTEGER PRIMARY KEY)"),
            format!(
                "CREATE TEMP TABLE {table}_temporary_child \
                 (parent_id INTEGER REFERENCES public.{permanent_parent}(id))"
            ),
            format!(
                "CREATE SEQUENCE public.{table}_cross_owned \
                 OWNED BY {temporary_parent}.id"
            ),
        ] {
            assert_execution_matches(&mut postgres_first, &mut fake_first, &sql).await;
        }

        let cleanup = format!(
            "DROP TABLE IF EXISTS public.{table}, public.{serial_table}, \
             public.{permanent_parent}, public.{default_table}, public.\"{quoted_table}\""
        );
        sqlx::raw_sql(AssertSqlSafe(cleanup.as_str()))
            .execute(&mut postgres_first)
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
