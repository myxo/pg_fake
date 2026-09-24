#![cfg(feature = "time")]

use pg_fake_sqlx::{Db, PgFakeConnection};
use serde_json::Value as JsonValue;
use sqlx::{Column, Connection, Executor, Row, Statement as _, TypeInfo};
use sqlx_postgres::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

mod common;
#[path = "common/differential.rs"]
mod differential;

use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

const SHARED_SQL: &[&str] = &[
    "CREATE TABLE phase3_items (id INTEGER PRIMARY KEY, quantity BIGINT, payload JSONB, tags UUID[], due TIMESTAMPTZ)",
    "CREATE VIEW phase3_item_view AS SELECT id, quantity, payload, tags, due FROM phase3_items",
    "BEGIN",
    "SET LOCAL application_name = 'phase3-integration'",
    "INSERT INTO phase3_items VALUES (1, 2, '{\"label\":\"first\"}'::jsonb, ARRAY['11111111-1111-1111-1111-111111111111'::uuid], '2025-01-02 03:04:05+00'::timestamptz)",
    "SAVEPOINT retry",
    "UPDATE phase3_items SET quantity = 99 WHERE id = 1",
    "ROLLBACK TO SAVEPOINT retry",
    "RELEASE SAVEPOINT retry",
    "WITH incoming(id, quantity) AS (VALUES (1, 3), (2, 4)) INSERT INTO phase3_items (id, quantity, payload, tags, due) SELECT id, quantity, '{\"label\":\"upsert\"}'::jsonb, ARRAY['22222222-2222-2222-2222-222222222222'::uuid], '2025-01-03 04:05:06+00'::timestamptz FROM incoming ON CONFLICT (id) DO UPDATE SET quantity = EXCLUDED.quantity RETURNING id, quantity",
    "SELECT id, payload->>'label', array_length(tags, 1), sum(quantity) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM phase3_item_view ORDER BY id",
    "CREATE TABLE phase3_tx_log (id INTEGER PRIMARY KEY)",
    "SELECT id FROM phase3_items ORDER BY id FOR UPDATE",
    "COMMIT",
];

#[test]
fn integrates_phase3_queries_through_native_and_sqlx() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake = PgFakeConnection::new(db.clone());
    let mut native = db.snapshot().create_session();

    for sql in SHARED_SQL {
        let order = if sql.starts_with("WITH incoming") {
            RowOrder::Unordered
        } else {
            RowOrder::Ordered
        };
        assert_statement(&runtime, &mut postgres, &mut fake, sql, order);
        let result = native.execute(sql).unwrap();
        assert_eq!(result.len(), 1, "native execution of {sql}");
    }

    let query = "SELECT id, quantity, payload, tags, due FROM phase3_items WHERE id = $1";
    let postgres_statement = runtime.block_on(postgres.prepare(query)).unwrap();
    let fake_statement = runtime.block_on(fake.prepare(query)).unwrap();
    let postgres_columns = postgres_statement
        .columns()
        .iter()
        .map(|column| (column.name(), column.type_info().name()))
        .collect::<Vec<_>>();
    let fake_columns = fake_statement
        .columns()
        .iter()
        .map(|column| (column.name(), column.type_info().name()))
        .collect::<Vec<_>>();
    assert_eq!(fake_columns, postgres_columns);

    let postgres_row = runtime
        .block_on(sqlx::query(query).bind(1_i32).fetch_one(&mut postgres))
        .unwrap();
    let fake_row = runtime
        .block_on(sqlx::query(query).bind(1_i32).fetch_one(&mut fake))
        .unwrap();
    assert_eq!(
        fake_row.get::<i32, _>("id"),
        postgres_row.get::<i32, _>("id")
    );
    assert_eq!(
        fake_row.get::<i64, _>("quantity"),
        postgres_row.get::<i64, _>("quantity")
    );
    assert_eq!(
        fake_row.get::<JsonValue, _>("payload"),
        postgres_row.get::<JsonValue, _>("payload")
    );
    assert_eq!(
        fake_row.get::<Vec<Uuid>, _>("tags"),
        postgres_row.get::<Vec<Uuid>, _>("tags")
    );
    assert_eq!(
        fake_row.get::<OffsetDateTime, _>("due"),
        postgres_row.get::<OffsetDateTime, _>("due")
    );
    let postgres_empty = runtime
        .block_on(sqlx::query(query).bind(999_i32).fetch_all(&mut postgres))
        .unwrap();
    let fake_empty = runtime
        .block_on(sqlx::query(query).bind(999_i32).fetch_all(&mut fake))
        .unwrap();
    assert!(postgres_empty.is_empty() && fake_empty.is_empty());

    let native_rows = native
        .query("SELECT id, quantity FROM phase3_items ORDER BY id", &[])
        .unwrap();
    assert_eq!(native_rows.rows.len(), 2);
    assert_eq!(native_rows.rows[0][0].format_postgres_text(), "1");
    assert_eq!(native_rows.rows[0][1].format_postgres_text(), "3");

    let mut postgres_second = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake_second = PgFakeConnection::new(db);
    for sql in [
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "SELECT quantity FROM phase3_items WHERE id = 1",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "UPDATE phase3_items SET quantity = quantity + 1 WHERE id = 2",
        "COMMIT",
    ] {
        assert_statement(
            &runtime,
            &mut postgres_second,
            &mut fake_second,
            sql,
            RowOrder::Ordered,
        );
    }
    for sql in [
        "SELECT id, quantity FROM phase3_items ORDER BY id",
        "COMMIT",
        "SELECT id, quantity FROM phase3_items ORDER BY id",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn rolls_back_phase3_catalog_changes_through_sqlx() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for sql in [
        "BEGIN",
        "CREATE TABLE phase3_rolled_back (id INTEGER PRIMARY KEY)",
        "INSERT INTO phase3_rolled_back VALUES (1)",
        "ROLLBACK",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id FROM phase3_rolled_back",
        RowOrder::Ordered,
    );
}
