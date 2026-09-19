use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Connection, Row};
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn matches_savepoint_recovery_and_catalog_changes() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for sql in [
        "SAVEPOINT s",
        "ROLLBACK TO s",
        "RELEASE s",
        "BEGIN",
        "SAVEPOINT s",
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
        "ROLLBACK TO s",
        "COMMIT",
        "BEGIN ISOLATION LEVEL REPEATABLE READ",
        "SAVEPOINT s",
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
        "ROLLBACK TO s",
        "SELECT 1",
        "ROLLBACK TO s",
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
        "ROLLBACK",
        "CREATE TABLE items(id SERIAL PRIMARY KEY, value INT)",
        "INSERT INTO items(value) VALUES(10)",
        "BEGIN",
        "INSERT INTO items(value) VALUES(20)",
        "SAVEPOINT s",
        "UPDATE items SET value = 30 WHERE id = 1",
        "SAVEPOINT s",
        "DELETE FROM items WHERE id = 2",
        "ROLLBACK TO s",
        "SELECT * FROM items ORDER BY id",
        "RELEASE s",
        "ROLLBACK TO s",
        "SELECT * FROM items ORDER BY id",
        "SAVEPOINT child",
        "INSERT INTO items(id, value) VALUES(1, 99)",
        "SELECT 1",
        "RELEASE child",
        "ROLLBACK TO child",
        "INSERT INTO items(value) VALUES(40)",
        "RELEASE child",
        "ROLLBACK TO s",
        "SELECT * FROM items ORDER BY id",
        "COMMIT",
        "SELECT * FROM items ORDER BY id",
        "SELECT nextval('items_id_seq')",
        "BEGIN",
        "SAVEPOINT ddl",
        "ALTER TABLE items ADD COLUMN extra TEXT DEFAULT 'x'",
        "UPDATE items SET extra = 'y'",
        "SAVEPOINT inner_ddl",
        "DROP TABLE items",
        "ROLLBACK TO inner_ddl",
        "SELECT * FROM items ORDER BY id",
        "ROLLBACK TO ddl",
        "SELECT * FROM items ORDER BY id",
        "CREATE TABLE scratch(id INT)",
        "INSERT INTO scratch VALUES(1)",
        "ROLLBACK TO ddl",
        "SELECT * FROM scratch",
        "ROLLBACK TO ddl",
        "COMMIT",
        "SELECT * FROM items ORDER BY id",
        "BEGIN",
        "SAVEPOINT trunc",
        "TRUNCATE items RESTART IDENTITY",
        "INSERT INTO items(value) VALUES(100)",
        "SAVEPOINT trunc2",
        "TRUNCATE items RESTART IDENTITY",
        "INSERT INTO items(value) VALUES(200)",
        "ROLLBACK TO trunc2",
        "SELECT * FROM items ORDER BY id",
        "ROLLBACK TO trunc",
        "SELECT * FROM items ORDER BY id",
        "SELECT nextval('items_id_seq')",
        "COMMIT",
        "BEGIN",
        "SAVEPOINT s",
        "ROLLBACK TO missing",
        "ROLLBACK TO s",
        "SELECT 1",
        "RELEASE s",
        "ROLLBACK TO s",
        "ROLLBACK",
        "BEGIN",
        "SAVEPOINT \"Mixed\"",
        "SAVEPOINT mixed",
        "RELEASE mixed",
        "ROLLBACK TO \"Mixed\"",
        "COMMIT",
        "CREATE TABLE parent(id INT PRIMARY KEY)",
        "CREATE TABLE child(id INT REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
        "BEGIN",
        "SAVEPOINT fk",
        "INSERT INTO child VALUES(1)",
        "ROLLBACK TO fk",
        "COMMIT",
        "BEGIN",
        "INSERT INTO parent VALUES(1)",
        "SAVEPOINT fk",
        "INSERT INTO child VALUES(2)",
        "SET CONSTRAINTS ALL IMMEDIATE",
        "ROLLBACK TO fk",
        "INSERT INTO child VALUES(1)",
        "COMMIT",
        "SELECT * FROM child",
        "SET TIME ZONE 'UTC'",
        "BEGIN",
        "SET LOCAL TIME ZONE '+03:00'",
        "SAVEPOINT settings",
        "SET LOCAL TIME ZONE '+09:00'",
        "SHOW timezone",
        "ROLLBACK TO settings",
        "SHOW timezone",
        "SET TIME ZONE 'UTC'",
        "ROLLBACK TO settings",
        "SHOW timezone",
        "COMMIT",
        "SHOW timezone",
    ] {
        if sql.starts_with("SET CONSTRAINTS") {
            runtime.block_on(async {
                let expected = sqlx::raw_sql(sql)
                    .execute(&mut postgres)
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        error
                            .as_database_error()
                            .unwrap()
                            .code()
                            .unwrap()
                            .into_owned()
                    });
                let actual = sqlx::raw_sql(sql)
                    .execute(&mut fake)
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        error
                            .as_database_error()
                            .unwrap()
                            .code()
                            .unwrap()
                            .into_owned()
                    });
                assert_eq!(actual, expected, "{sql}");
            });
        } else {
            assert_statement_allow_error(
                &runtime,
                &mut postgres,
                &mut fake,
                sql,
                RowOrder::Ordered,
            );
        }
    }
}

#[test]
fn matches_savepoint_lock_release_and_preservation() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut other = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake = PgFakeConnection::new(db.clone());
    let mut fake_other = PgFakeConnection::new(db);
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT 1",
        RowOrder::Unordered,
    );
    for (holder, sql) in [
        (true, "CREATE TABLE locks(id INT PRIMARY KEY, value INT)"),
        (true, "INSERT INTO locks VALUES(1,10),(2,20)"),
        (true, "BEGIN"),
        (true, "SELECT * FROM locks WHERE id = 1 FOR KEY SHARE"),
        (true, "SELECT pg_advisory_xact_lock(100)"),
        (true, "SAVEPOINT s"),
        (true, "SELECT * FROM locks WHERE id = 1 FOR UPDATE"),
        (true, "UPDATE locks SET value = 21 WHERE id = 2"),
        (true, "SELECT pg_advisory_xact_lock(101)"),
        (true, "SELECT pg_advisory_lock(102)"),
        (true, "ROLLBACK TO s"),
        (false, "SELECT * FROM locks WHERE id = 1 FOR UPDATE NOWAIT"),
        (
            false,
            "SELECT * FROM locks WHERE id = 1 FOR NO KEY UPDATE NOWAIT",
        ),
        (false, "SELECT * FROM locks WHERE id = 2 FOR UPDATE NOWAIT"),
        (
            false,
            "SELECT pg_try_advisory_xact_lock(100), pg_try_advisory_xact_lock(101), pg_try_advisory_xact_lock(102)",
        ),
        (true, "SAVEPOINT child"),
        (true, "SELECT * FROM locks WHERE id = 2 FOR UPDATE"),
        (true, "SELECT 1 / 0"),
        (false, "SELECT * FROM locks WHERE id = 2 FOR UPDATE NOWAIT"),
        (true, "ROLLBACK TO child"),
        (true, "LOCK TABLE locks IN ACCESS EXCLUSIVE MODE"),
        (true, "ROLLBACK TO child"),
        (false, "SELECT * FROM locks"),
        (true, "COMMIT"),
        (true, "SELECT pg_advisory_unlock(102)"),
    ] {
        let (postgres, fake) = if holder {
            (&mut postgres, &mut fake)
        } else {
            (&mut other, &mut fake_other)
        };
        assert_statement_allow_error(&runtime, postgres, fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn executes_nested_sqlx_transactions() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = start_isolated_postgres_server();
    runtime.block_on(async {
        macro_rules! run_nested_transactions {
            ($connection:expr) => {{
                let mut connection = $connection;
                sqlx::raw_sql("CREATE TABLE nested(id INT PRIMARY KEY)")
                    .execute(&mut connection)
                    .await
                    .unwrap();
                let mut outer = connection.begin().await.unwrap();
                sqlx::query("INSERT INTO nested VALUES(1)")
                    .execute(&mut *outer)
                    .await
                    .unwrap();
                {
                    let mut inner = outer.begin().await.unwrap();
                    sqlx::query("INSERT INTO nested VALUES(2)")
                        .execute(&mut *inner)
                        .await
                        .unwrap();
                    inner.commit().await.unwrap();
                }
                {
                    let mut inner = outer.begin().await.unwrap();
                    sqlx::query("INSERT INTO nested VALUES(3)")
                        .execute(&mut *inner)
                        .await
                        .unwrap();
                    inner.rollback().await.unwrap();
                }
                {
                    let mut inner = outer.begin().await.unwrap();
                    sqlx::query("INSERT INTO nested VALUES(4)")
                        .execute(&mut *inner)
                        .await
                        .unwrap();
                    let mut deepest = inner.begin().await.unwrap();
                    sqlx::query("INSERT INTO nested VALUES(5)")
                        .execute(&mut *deepest)
                        .await
                        .unwrap();
                }
                sqlx::query("INSERT INTO nested VALUES(6)")
                    .execute(&mut *outer)
                    .await
                    .unwrap();
                outer.commit().await.unwrap();
                let rows = sqlx::query("SELECT id FROM nested ORDER BY id")
                    .fetch_all(&mut connection)
                    .await
                    .unwrap();
                assert_eq!(
                    rows.iter()
                        .map(|row| row.get::<i32, _>(0))
                        .collect::<Vec<_>>(),
                    vec![1, 2, 6]
                );
            }};
        }
        run_nested_transactions!(PgFakeConnection::new(Db::create()));
        run_nested_transactions!(PgConnection::connect(&server.url).await.unwrap());
    });
}

#[test]
fn preserves_repeatable_read_snapshot_across_savepoint_recovery() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut reader = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut writer = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake_reader = PgFakeConnection::new(db.clone());
    let mut fake_writer = PgFakeConnection::new(db);
    for (read, sql) in [
        (
            false,
            "CREATE TABLE snapshot_rows(id INT PRIMARY KEY, value INT)",
        ),
        (false, "INSERT INTO snapshot_rows VALUES(1,10)"),
        (true, "BEGIN ISOLATION LEVEL REPEATABLE READ"),
        (true, "SAVEPOINT s"),
        (true, "SELECT * FROM snapshot_rows ORDER BY id"),
        (false, "UPDATE snapshot_rows SET value = 20"),
        (true, "ROLLBACK TO s"),
        (true, "SELECT * FROM snapshot_rows ORDER BY id"),
        (true, "UPDATE snapshot_rows SET value = 30"),
        (true, "ROLLBACK TO s"),
        (true, "SELECT * FROM snapshot_rows ORDER BY id"),
        (true, "INSERT INTO snapshot_rows VALUES(2,40)"),
        (true, "COMMIT"),
        (false, "SELECT * FROM snapshot_rows ORDER BY id"),
    ] {
        let (postgres, fake) = if read {
            (&mut reader, &mut fake_reader)
        } else {
            (&mut writer, &mut fake_writer)
        };
        assert_statement_allow_error(&runtime, postgres, fake, sql, RowOrder::Ordered);
    }
}
