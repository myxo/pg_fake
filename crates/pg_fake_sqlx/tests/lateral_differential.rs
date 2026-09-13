use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Executor, Row, SqlStr, Statement, TypeInfo};
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn matches_lateral_fixtures_and_errors() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT 1",
        RowOrder::Unordered,
    );
    for statement in pg_fake::parser::parse(include_str!("fixtures/lateral.sql")).unwrap() {
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &statement.to_string(),
            RowOrder::Ordered,
        );
    }
    for sql in [
        "SELECT * FROM lateral_parents p CROSS JOIN (SELECT p.id) x",
        "SELECT * FROM LATERAL (SELECT p.id) x, lateral_parents p",
        "SELECT * FROM lateral_parents p RIGHT JOIN LATERAL (SELECT p.id) x ON TRUE",
        "SELECT * FROM lateral_parents p FULL JOIN LATERAL (SELECT p.id) x ON TRUE",
        "SELECT * FROM lateral_parents p CROSS JOIN LATERAL (SELECT p.missing) x",
        "SELECT * FROM lateral_parents p CROSS JOIN lateral_children c CROSS JOIN LATERAL (SELECT id) x",
        "SELECT * FROM lateral_parents p CROSS JOIN LATERAL (SELECT p.id) x(a,b)",
        "SELECT * FROM lateral_parents p CROSS JOIN LATERAL (SELECT c.id FROM lateral_children c WHERE p.id) x",
        "SELECT * FROM lateral_parents p CROSS JOIN LATERAL (SELECT p.id LIMIT -1) x",
        "SELECT * FROM lateral_parents p, lateral_children c JOIN LATERAL (SELECT p.id) x ON p.id = x.id",
        "SELECT p.id, x.v FROM lateral_parents p CROSS JOIN LATERAL (SELECT sum(p.id) AS v) x",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn rejects_unsupported_lateral_table_functions() {
    let db = Db::create();
    let mut session = db.create_session();
    for sql in [
        "SELECT * FROM (VALUES (2)) p(id), LATERAL generate_series(1, p.id) x",
        "SELECT * FROM LATERAL unnest(ARRAY[1, 2]) x",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            pg_fake::error::SqlState::FeatureNotSupported,
            "{sql}"
        );
    }
}

#[test]
fn matches_lateral_prepared_parameters() {
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
        "SELECT x.v FROM (VALUES (1), (2)) p(id) CROSS JOIN LATERAL (SELECT p.id + $1 AS v) x",
        "SELECT p.id, x.v FROM (VALUES (1), (2)) p(id) LEFT JOIN LATERAL (SELECT p.id + $1 AS v LIMIT $2) x ON TRUE ORDER BY p.id",
        "SELECT x.v FROM (VALUES ('abc'::varchar(12))) p(label) CROSS JOIN LATERAL (SELECT p.label AS v WHERE p.label = $1) x",
        "SELECT x.* FROM (VALUES (1, 'abc'::varchar(12), '{}'::jsonb)) p(id, label, payload) CROSS JOIN LATERAL (SELECT p.*) x",
        "SELECT x.id FROM (VALUES (1)) p(id) CROSS JOIN LATERAL (SELECT p.id FROM (VALUES (2)) p(id) UNION ALL SELECT p.id) x",
        "SELECT x.v FROM (VALUES (1)) p(id) CROSS JOIN LATERAL (WITH y AS (SELECT p.id AS v) SELECT v FROM y) x",
        "SELECT p.id, x.v FROM (VALUES (1), (2)) p(id) CROSS JOIN LATERAL (WITH y AS (SELECT p.id AS v) SELECT z.v FROM (WITH y AS (SELECT v + 10 AS v FROM y) SELECT v FROM y) z) x ORDER BY p.id",
    ] {
        let expected = runtime
            .block_on(postgres.prepare(SqlStr::from_static(sql)))
            .unwrap();
        let actual = runtime
            .block_on(fake.prepare(SqlStr::from_static(sql)))
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(
            actual
                .columns()
                .iter()
                .map(|c| (c.name(), c.type_info().name()))
                .collect::<Vec<_>>(),
            expected
                .columns()
                .iter()
                .map(|c| (c.name(), c.type_info().name()))
                .collect::<Vec<_>>(),
            "{sql}"
        );
        assert_eq!(
            actual
                .parameters()
                .unwrap()
                .left()
                .unwrap()
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>(),
            expected
                .parameters()
                .unwrap()
                .left()
                .unwrap()
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>(),
            "{sql}"
        );
    }
    let sql = "SELECT p.id, x.v FROM (VALUES (1), (2)) p(id) LEFT JOIN LATERAL (SELECT p.id + $1 AS v LIMIT $2) x ON TRUE ORDER BY p.id";
    for limit in [0_i64, 1] {
        let expected = runtime
            .block_on(
                sqlx::query(sql)
                    .bind(7_i32)
                    .bind(limit)
                    .fetch_all(&mut postgres),
            )
            .unwrap();
        let actual = runtime
            .block_on(
                sqlx::query(sql)
                    .bind(7_i32)
                    .bind(limit)
                    .fetch_all(&mut fake),
            )
            .unwrap();
        assert_eq!(
            actual
                .iter()
                .map(|r| (r.get::<i32, _>(0), r.get::<Option<i32>, _>(1)))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|r| (r.get::<i32, _>(0), r.get::<Option<i32>, _>(1)))
                .collect::<Vec<_>>()
        );
    }
}
