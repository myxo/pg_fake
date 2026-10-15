use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{RowOrder, assert_statement, start_isolated_postgres_server};

#[test]
fn compares_prefix_reads_and_changed_keys() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake = PgFakeConnection::new(db.clone());
    for sql in [
        "CREATE TABLE prefix_diff (id INTEGER PRIMARY KEY, tenant INTEGER, code INTEGER, payload TEXT)",
        "CREATE UNIQUE INDEX prefix_diff_key ON prefix_diff(tenant, code)",
        "CREATE TABLE prefix_duplicates (id INTEGER PRIMARY KEY, tenant INTEGER, code INTEGER)",
        "CREATE INDEX prefix_duplicates_key ON prefix_duplicates(tenant, code)",
        "CREATE TABLE prefix_triple (tenant INTEGER, region INTEGER, code TEXT)",
        "CREATE UNIQUE INDEX prefix_triple_key ON prefix_triple(tenant, region, code)",
        "INSERT INTO prefix_diff VALUES (1, 1, 10, 'first'), (2, 1, 20, 'second'), \
         (3, 2, 30, 'third'), (4, NULL, 40, 'null tenant'), \
         (5, 1, NULL, 'null code'), (6, 1, NULL, 'second null')",
        "INSERT INTO prefix_duplicates VALUES (1, 1, 10), (2, 1, 10), (3, 1, 20), (4, 2, 30)",
        "INSERT INTO prefix_triple VALUES (1, 2, 'a'), (1, 2, 'b'), (1, 2, NULL), \
         (1, 3, 'c'), (2, 2, 'd')",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    for sql in [
        "SELECT id, code FROM prefix_diff WHERE tenant = 1 ORDER BY id",
        "SELECT id, code FROM prefix_diff WHERE tenant = 99 ORDER BY id",
        "SELECT id, code FROM prefix_diff WHERE tenant = NULL ORDER BY id",
        "SELECT id, code FROM prefix_diff WHERE tenant IS NULL ORDER BY id",
        "SELECT id, code FROM prefix_diff WHERE code = 10 ORDER BY id",
        "SELECT id, code FROM prefix_diff WHERE tenant = 1 AND code = 10 ORDER BY id",
        "SELECT id, code FROM prefix_diff WHERE tenant = 1 AND code IS NULL ORDER BY id",
        "SELECT id, code FROM prefix_duplicates WHERE tenant = 1 ORDER BY id",
        "SELECT code FROM prefix_triple WHERE region = 2 AND tenant = 1 ORDER BY code",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    runtime.block_on(async {
        let sql = "SELECT id, code FROM prefix_diff WHERE tenant = $1 ORDER BY id";
        for tenant in [1_i32, 2, 99] {
            let expected: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
                .bind(tenant)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
                .bind(tenant)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
    });

    let mut postgres_reader = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake_reader = PgFakeConnection::new(db);
    runtime.block_on(async {
        sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut postgres_reader)
            .await
            .unwrap();
        sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut fake_reader)
            .await
            .unwrap();
        let sql = "SELECT id, code FROM prefix_diff WHERE tenant = 1 ORDER BY id";
        let expected: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
            .fetch_all(&mut postgres_reader)
            .await
            .unwrap();
        let actual: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
            .fetch_all(&mut fake_reader)
            .await
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 4);
        sqlx::query("UPDATE prefix_diff SET code = 11 WHERE id = 1")
            .execute(&mut postgres)
            .await
            .unwrap();
        sqlx::query("UPDATE prefix_diff SET code = 11 WHERE id = 1")
            .execute(&mut fake)
            .await
            .unwrap();
        let expected: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
            .fetch_all(&mut postgres_reader)
            .await
            .unwrap();
        let actual: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
            .fetch_all(&mut fake_reader)
            .await
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual[0], (1, Some(10)));
        sqlx::query("ROLLBACK")
            .execute(&mut postgres_reader)
            .await
            .unwrap();
        sqlx::query("ROLLBACK")
            .execute(&mut fake_reader)
            .await
            .unwrap();
    });
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id, code FROM prefix_diff WHERE tenant = 1 ORDER BY id",
        RowOrder::Ordered,
    );
    for sql in [
        "BEGIN",
        "UPDATE prefix_diff SET tenant = 2, code = 31 WHERE id = 1",
        "SELECT id FROM prefix_diff WHERE tenant = 1 ORDER BY id",
        "ROLLBACK",
        "SELECT id FROM prefix_diff WHERE tenant = 1 ORDER BY id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}
