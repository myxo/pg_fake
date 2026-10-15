use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{RowOrder, assert_statement, start_isolated_postgres_server};

#[test]
fn compares_complete_composite_access_and_mutations() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for sql in [
        "CREATE TABLE composite_keyed (group_id INTEGER, code TEXT, payload TEXT)",
        "CREATE UNIQUE INDEX composite_keyed_pair ON composite_keyed(group_id, code)",
        "CREATE TABLE composite_reverse (group_id INTEGER, code TEXT, payload TEXT)",
        "CREATE UNIQUE INDEX composite_reverse_pair ON composite_reverse(code, group_id)",
        "CREATE TABLE composite_many (group_id INTEGER, code TEXT, payload TEXT)",
        "CREATE INDEX composite_many_pair ON composite_many(group_id, code)",
        "CREATE TABLE composite_parent (id INTEGER PRIMARY KEY, group_id INTEGER, code TEXT)",
        "CREATE INDEX composite_parent_group ON composite_parent(group_id)",
        "INSERT INTO composite_keyed VALUES (1, 'a', 'first'), (1, 'b', 'second'), \
         (2, 'a', 'third'), (NULL, 'a', 'null group'), (3, NULL, 'null code')",
        "INSERT INTO composite_reverse VALUES (1, 'a', 'reverse first'), \
         (1, 'b', 'reverse second'), (2, 'a', 'reverse third')",
        "INSERT INTO composite_many VALUES (1, 'a', 'first'), (1, 'a', 'second'), \
         (1, 'b', 'third')",
        "INSERT INTO composite_parent VALUES (10, 1, 'a'), (11, 1, 'b'), \
         (12, 2, 'a'), (13, 3, NULL), (14, 1, 'a')",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    for sql in [
        "SELECT payload FROM composite_keyed WHERE group_id = 1 AND code = 'a'",
        "SELECT payload FROM composite_keyed WHERE code = 'a' AND group_id = 2",
        "SELECT payload FROM composite_reverse WHERE group_id = 1 AND code = 'a'",
        "SELECT payload FROM composite_keyed WHERE group_id = 1 AND code = 'a' AND payload = 'wrong'",
        "SELECT payload FROM composite_keyed WHERE group_id = NULL AND code = 'a'",
        "SELECT payload FROM composite_keyed WHERE group_id = 1 ORDER BY code",
        "SELECT payload FROM composite_many WHERE group_id = 1 AND code = 'a' ORDER BY payload",
        "SELECT p.id, k.payload FROM composite_parent p JOIN composite_keyed k \
         ON k.code = p.code AND k.group_id = p.group_id WHERE p.id = 10",
        "SELECT p.id, k.payload FROM composite_parent p JOIN composite_keyed k \
         ON k.code = p.code AND k.group_id = p.group_id WHERE p.id = 13",
        "SELECT payload FROM composite_keyed WHERE code = CAST('a' AS TEXT) AND group_id = 1",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    runtime.block_on(async {
        let sql = "SELECT payload FROM composite_keyed WHERE group_id = $1 AND code = $2";
        for (group_id, code) in [(1_i32, "a"), (1, "b"), (1, "missing"), (2, "a")] {
            let expected: Vec<(String,)> = sqlx::query_as(sql)
                .bind(group_id)
                .bind(code)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(String,)> = sqlx::query_as(sql)
                .bind(group_id)
                .bind(code)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
        let sql = "SELECT p.id, k.payload FROM composite_parent p JOIN composite_keyed k \
                   ON k.code = p.code AND k.group_id = p.group_id WHERE p.id = $1";
        for id in [10_i32, 11, 12, 13, 14] {
            let expected: Vec<(i32, String)> = sqlx::query_as(sql)
                .bind(id)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32, String)> = sqlx::query_as(sql)
                .bind(id)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
        let sql = "SELECT p.id, r.payload FROM composite_parent p JOIN composite_reverse r \
                   ON r.group_id = p.group_id AND r.code = p.code \
                   WHERE p.group_id = $1 AND p.code = $2";
        let mut expected: Vec<(i32, String)> = sqlx::query_as(sql)
            .bind(1_i32)
            .bind("a")
            .fetch_all(&mut postgres)
            .await
            .unwrap();
        let mut actual: Vec<(i32, String)> = sqlx::query_as(sql)
            .bind(1_i32)
            .bind("a")
            .fetch_all(&mut fake)
            .await
            .unwrap();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 2);
    });
    for sql in [
        "BEGIN",
        "UPDATE composite_keyed SET payload = 'changed' WHERE code = 'a' AND group_id = 1",
        "SELECT payload FROM composite_keyed WHERE group_id = 1 AND code = 'a'",
        "ROLLBACK",
        "SELECT payload FROM composite_keyed WHERE group_id = 1 AND code = 'a'",
        "BEGIN",
        "DELETE FROM composite_keyed WHERE group_id = 1 AND code = 'a'",
        "SELECT payload FROM composite_keyed WHERE group_id = 1 AND code = 'a'",
        "ROLLBACK",
        "SELECT payload FROM composite_keyed WHERE group_id = 1 AND code = 'a'",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}
