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

#[test]
fn ordinary_views_match_postgres_through_sqlx() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let suffix = std::process::id();
        let table = format!("pg_fake_view_source_{suffix}");
        let view = format!("pg_fake_view_{suffix}");
        let nested = format!("pg_fake_nested_view_{suffix}");

        for sql in [
            format!("CREATE TABLE public.{table} (id INTEGER, label VARCHAR(12), bucket INTEGER)"),
            format!("INSERT INTO public.{table} VALUES (1, 'one', 1), (2, 'two', 1), (3, 'three', 2)"),
            format!(
                "CREATE VIEW public.{view} (key, name, bucket) AS \
                 SELECT id, label, bucket FROM public.{table} WHERE id > 1"
            ),
            format!(
                "CREATE VIEW public.{nested} AS \
                 WITH grouped AS (SELECT bucket, count(*) AS total FROM public.{view} GROUP BY bucket) \
                 SELECT bucket, total FROM grouped"
            ),
            format!("COMMENT ON VIEW public.{view} IS 'compatibility view'"),
        ] {
            assert_execution_matches(&mut postgres, &mut fake, &sql).await;
        }

        let prepared = format!("SELECT name FROM public.{view} WHERE key > $1 ORDER BY key");
        let expected = sqlx::query(AssertSqlSafe(prepared.as_str()))
            .bind(1_i32)
            .fetch_all(&mut postgres)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect::<Vec<_>>();
        let actual = sqlx::query(AssertSqlSafe(prepared.as_str()))
            .bind(1_i32)
            .fetch_all(&mut fake)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);

        let aggregate = format!("SELECT bucket, total FROM public.{nested} ORDER BY bucket");
        let expected = sqlx::raw_sql(AssertSqlSafe(aggregate.as_str()))
            .fetch_all(&mut postgres)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get::<i32, _>(0), row.get::<i64, _>(1)))
            .collect::<Vec<_>>();
        let actual = sqlx::raw_sql(AssertSqlSafe(aggregate.as_str()))
            .fetch_all(&mut fake)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get::<i32, _>(0), row.get::<i64, _>(1)))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);

        for sql in [
            "BEGIN".to_owned(),
            format!(
                "CREATE OR REPLACE VIEW public.{view} (key, name, bucket) AS \
                 SELECT id, label, bucket FROM public.{table} WHERE id > 99"
            ),
            format!("COMMENT ON VIEW public.{view} IS NULL"),
            "ROLLBACK".to_owned(),
        ] {
            assert_execution_matches(&mut postgres, &mut fake, &sql).await;
        }

        let temporary = format!("CREATE TEMP TABLE {table} (id INTEGER, label VARCHAR(12), bucket INTEGER)");
        assert_execution_matches(&mut postgres, &mut fake, &temporary).await;
        let count = format!("SELECT count(*) FROM public.{view}");
        assert_i64_matches(&mut postgres, &mut fake, &count).await;

        let invalid = format!(
            "CREATE OR REPLACE VIEW public.{view} AS \
             SELECT label AS name, id AS key, bucket FROM public.{table}"
        );
        assert_execution_matches(&mut postgres, &mut fake, &invalid).await;

        for sql in [
            format!("DROP VIEW public.{view}"),
            format!("DROP TABLE public.{table}"),
            format!("DROP VIEW public.{nested}"),
            format!("DROP VIEW IF EXISTS public.{nested}"),
        ] {
            assert_execution_matches(&mut postgres, &mut fake, &sql).await;
        }
    });
}

#[test]
fn view_catalog_edge_cases_match_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let suffix = std::process::id();
        let table = format!("pg_fake_view_edges_{suffix}");
        let view = format!("{table}_view");
        let sequence = format!("{table}_sequence");
        let sequence_view = format!("{table}_sequence_view");

        for sql in [
            format!("CREATE TABLE {table} (a INTEGER, unused INTEGER)"),
            format!("INSERT INTO {table} VALUES (1, 8)"),
            format!("CREATE VIEW {view} AS SELECT * FROM {table}"),
            format!("ALTER TABLE {table} ADD COLUMN later INTEGER DEFAULT 2"),
        ] {
            assert_execution_matches(&mut postgres, &mut fake, &sql).await;
        }
        let select = format!("SELECT * FROM {view}");
        let expected = sqlx::raw_sql(AssertSqlSafe(select.as_str()))
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let actual = sqlx::raw_sql(AssertSqlSafe(select.as_str()))
            .fetch_one(&mut fake)
            .await
            .unwrap();
        assert_eq!(actual.len(), expected.len());
        assert_eq!(actual.len(), 2);

        for sql in [
            format!("CREATE SEQUENCE {sequence}"),
            format!("CREATE VIEW {sequence_view} AS SELECT nextval('{sequence}') AS value"),
            format!("DROP SEQUENCE {sequence}"),
            format!("ALTER TABLE {table} DROP COLUMN later"),
            format!("ALTER TABLE {table} ALTER COLUMN a TYPE BIGINT"),
        ] {
            assert_execution_matches(&mut postgres, &mut fake, &sql).await;
        }

        let prepared_sql = format!("SELECT a FROM {view}");
        let expected = sqlx::query(AssertSqlSafe(prepared_sql.as_str()))
            .fetch_one(&mut postgres)
            .await
            .unwrap()
            .get::<i32, _>(0);
        let actual = sqlx::query(AssertSqlSafe(prepared_sql.as_str()))
            .fetch_one(&mut fake)
            .await
            .unwrap()
            .get::<i32, _>(0);
        assert_eq!(actual, expected);
        let comment = format!("COMMENT ON VIEW {view} IS 'documentation only'");
        assert_execution_matches(&mut postgres, &mut fake, &comment).await;
        let expected = sqlx::query(AssertSqlSafe(prepared_sql.as_str()))
            .fetch_one(&mut postgres)
            .await
            .unwrap()
            .get::<i32, _>(0);
        let actual = sqlx::query(AssertSqlSafe(prepared_sql.as_str()))
            .fetch_one(&mut fake)
            .await
            .unwrap()
            .get::<i32, _>(0);
        assert_eq!(actual, expected);
    });
}

#[tokio::test]
async fn migration_view_and_trigger_renames_execute_through_sqlx() {
    let db = Db::create();
    let mut connection = PgFakeConnection::new(db.clone());
    sqlx::raw_sql(AssertSqlSafe("CREATE TABLE migration_source (id INTEGER)"))
        .execute(&mut connection)
        .await
        .unwrap();
    db.seed_trigger_catalog_for_test(
        "CREATE TRIGGER migration_audit BEFORE INSERT ON migration_source \
           FOR EACH ROW EXECUTE FUNCTION migration_audit()",
    )
    .unwrap();
    sqlx::raw_sql(AssertSqlSafe(
        "BEGIN; \
         CREATE VIEW migration_view AS SELECT id FROM migration_source; \
         COMMENT ON VIEW migration_view IS 'read compatibility'; \
         DROP VIEW IF EXISTS migration_view; \
         ALTER TRIGGER migration_audit ON migration_source RENAME TO migration_audit_v2; \
         COMMIT",
    ))
    .execute(&mut connection)
    .await
    .unwrap();

    let error = sqlx::raw_sql(AssertSqlSafe(
        "ALTER TRIGGER migration_audit ON migration_source RENAME TO ignored",
    ))
    .execute(&mut connection)
    .await
    .unwrap_err();
    assert_eq!(get_sqlstate(error), "42704");
    sqlx::raw_sql(AssertSqlSafe(
        "ALTER TRIGGER migration_audit_v2 ON migration_source RENAME TO migration_audit_v3",
    ))
    .execute(&mut connection)
    .await
    .unwrap();
}
