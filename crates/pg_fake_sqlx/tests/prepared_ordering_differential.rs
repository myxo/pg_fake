use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn compare_prepared_ordering_pages_and_projection_errors() {
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
        "CREATE TABLE ordered_pages (id integer PRIMARY KEY, price integer, label text)",
        "INSERT INTO ordered_pages VALUES (1,20,'b'), (2,NULL,'a'), (3,10,'c'), (4,20,NULL)",
        "SELECT id AS KEY, price, label FROM ordered_pages ORDER BY key DESC",
        "SELECT id AS PRICE, price AS original FROM ordered_pages ORDER BY price",
        "SELECT id, price FROM ordered_pages ORDER BY price DESC NULLS LAST, id LIMIT NULL OFFSET NULL",
        "SELECT 1 / (id - id) FROM ordered_pages ORDER BY id LIMIT 0",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "SELECT id, price, 100 / (id - 2) FROM ordered_pages ORDER BY price ASC NULLS FIRST, id DESC LIMIT 2 OFFSET 1",
        "SELECT id FROM ordered_pages ORDER BY id LIMIT -1",
        "SELECT id FROM ordered_pages ORDER BY id OFFSET -1",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id, price, label FROM ordered_pages",
        RowOrder::Unordered,
    );
    runtime.block_on(async {
        for persistent in [true, false] {
            for sql in [
                "SELECT id, price, label FROM ordered_pages WHERE id > $1 ORDER BY price DESC NULLS FIRST, id",
                "SELECT id, price, label FROM ordered_pages WHERE id > $1 ORDER BY price ASC NULLS LAST, id LIMIT 2 OFFSET 1",
                "SELECT id, price, label FROM ordered_pages WHERE id > $1 ORDER BY id + $1",
            ] {
                for minimum in [0i32, 2, 4, 0] {
                    let expected: Vec<(i32, Option<i32>, Option<String>)> = sqlx::query_as(sql).bind(minimum).persistent(persistent).fetch_all(&mut postgres).await.unwrap();
                    let actual: Vec<(i32, Option<i32>, Option<String>)> = sqlx::query_as(sql).bind(minimum).persistent(persistent).fetch_all(&mut fake).await.unwrap();
                    assert_eq!(actual, expected, "{sql}, parameter={minimum}, persistent={persistent}");
                }
            }
        }
    });
}

#[test]
#[ignore = "existing generic executor skips a projection error PostgreSQL raises before LIMIT"]
fn compare_generic_projection_error_before_ordered_limit() {
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
        "CREATE TABLE ordered_pages (id integer PRIMARY KEY, price integer)",
        "INSERT INTO ordered_pages VALUES (1,20), (2,NULL), (3,10), (4,20)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id, price, 100 / (id - 1) FROM ordered_pages ORDER BY price ASC NULLS FIRST, id DESC LIMIT (2) OFFSET 1",
        RowOrder::Ordered,
    );
}
