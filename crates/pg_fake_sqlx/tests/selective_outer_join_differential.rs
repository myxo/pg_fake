use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{RowOrder, assert_statement, start_isolated_postgres_server};

#[test]
fn compares_selective_outer_join_rows_and_filters() {
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
        "CREATE TABLE outer_left (id INTEGER PRIMARY KEY, key_value INTEGER)",
        "CREATE TABLE outer_right (id INTEGER PRIMARY KEY, key_value INTEGER, active BOOLEAN)",
        "CREATE INDEX outer_right_key ON outer_right(key_value)",
        "INSERT INTO outer_left VALUES (1, 10), (2, 20), (3, 30), (4, NULL)",
        "INSERT INTO outer_right VALUES (11, 10, TRUE), (12, 10, FALSE), (21, 20, FALSE)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    for sql in [
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 1 ORDER BY r.id",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 3",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 4",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 99",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 3 FOR UPDATE OF l",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 2 AND r.id IS NULL",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 3 AND r.id IS NULL",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value AND r.active = TRUE WHERE l.id = 2",
        "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE l.id = 1 OR l.id = 3 ORDER BY l.id, r.id",
        "SELECT l.id, r.id FROM outer_left l RIGHT JOIN outer_right r \
         ON r.key_value = l.key_value WHERE r.id = 21",
        "SELECT l.id, r.id FROM outer_left l FULL OUTER JOIN outer_right r \
         ON r.key_value = l.key_value WHERE r.id IS NULL ORDER BY l.id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    runtime.block_on(async {
        let sql = "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
                   ON r.key_value = l.key_value WHERE l.id = $1 ORDER BY r.id";
        for id in [1_i32, 2, 3, 4, 99] {
            let expected: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
                .bind(id)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32, Option<i32>)> = sqlx::query_as(sql)
                .bind(id)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected, "id={id}");
        }
    });
}
