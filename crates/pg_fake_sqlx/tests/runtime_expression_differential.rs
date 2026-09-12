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
fn matches_runtime_expression_fixtures() {
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
    for statement in
        pg_fake::parser::parse(include_str!("fixtures/runtime_expressions.sql")).unwrap()
    {
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &statement.to_string(),
            RowOrder::Ordered,
        );
    }
    for sql in [
        "SELECT to_timestamp('NaN'::double precision)",
        "SELECT to_timestamp(1e20)",
        "SELECT to_timestamp(-1e20)",
        "SELECT to_timestamp('invalid')",
        "SELECT to_timestamp(true)",
        "SELECT floor('invalid')",
        "SELECT floor(true)",
        "SELECT date_trunc('day', NULL)",
        "SELECT to_char(NULL, 'YYYY')",
        "SELECT date_trunc('invalid', 'infinity'::timestamp)",
        "SELECT date_trunc('invalid', '2000-01-01'::timestamp)",
        "SELECT '2000-01-01'::timestamp AT TIME ZONE 'not_a_zone'",
        "SELECT date_trunc('day', '2000-01-01'::timestamptz, 'not_a_zone')",
        "SELECT date_trunc('day', 'infinity'::timestamptz, 'not_a_zone')",
        "SELECT 'xy' LIKE 'x\\'",
        "SELECT 'x' LIKE '%\\'",
        "SELECT 'x' LIKE 'x' ESCAPE 'ab'",
        "SELECT 1 LIKE '1'",
        "SELECT regexp_like('x','[')",
        "SELECT regexp_like('x','x{256}')",
        "SELECT regexp_like('a','a++')",
        "SELECT regexp_like('a','a**')",
        "SELECT regexp_like('a','a{1}{2}')",
        "SELECT regexp_like('a','a{1')",
        "SELECT regexp_like('a','^*')",
        "SELECT regexp_like('a','$+')",
        "SELECT regexp_like('a','a{1}*')",
        "SELECT regexp_like('a','[a-b-c]')",
        "SELECT regexp_like('a','[]a-b-c]')",
        "SELECT date_trunc('timezone', '2000-01-01'::timestamp)",
        "SELECT regexp_like('x','x','g')",
        "SELECT 1 !~* 'x'",
        "SELECT 'x' ~* '['",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for unit in [
        "microseconds",
        "milliseconds",
        "second",
        "minute",
        "hour",
        "day",
        "week",
        "month",
        "quarter",
        "year",
    ] {
        for date in [
            "1969-12-31 23:59:59.999999",
            "2000-01-01 00:00:00.000001",
            "2024-02-29 23:59:59.123456",
        ] {
            let sql = format!(
                "SELECT date_trunc('{unit}', '{date}'::timestamp), date_trunc('{unit}', '{date}+00'::timestamptz), date_trunc('{unit}', 'infinity'::timestamp)"
            );
            assert_statement(&runtime, &mut postgres, &mut fake, &sql, RowOrder::Ordered);
        }
    }
    for zone in ["+03:00", "-05:30", "UTC"] {
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!("SET TIME ZONE '{zone}'"),
            RowOrder::Ordered,
        );
        for sql in [
            "SELECT to_char('2000-01-01 00:00:00+00'::timestamptz, 'YYYY-MM-DD HH24:MI:SS TZH:TZM OF')",
            "SELECT extract(epoch FROM date_trunc('day', '2000-01-01 12:00:00+00'::timestamptz))",
            "SELECT to_char('2000-01-01'::date, 'YYYY-MM-DD HH24:MI:SS OF')",
            "SELECT '2000-01-01'::date AT TIME ZONE 'UTC'",
            "SELECT DATE '2000-01-01' AT TIME ZONE 'UTC'",
            "SELECT to_char(TIMESTAMPTZ '2000-01-01', 'YYYY-MM-DD HH24:MI:SS OF')",
            "SELECT to_char('2000-01-01'::timestamptz, 'YYYY-MM-DD HH24:MI:SS OF')",
            "SELECT to_char(date_trunc('day', '2000-01-01'::timestamp, 'UTC'), 'YYYY-MM-DD HH24:MI:SS OF')",
        ] {
            assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
        }
    }
    for sql in [
        "BEGIN",
        "UPDATE runtime_values SET amount = floor(amount)",
        "UPDATE runtime_values SET occurred = to_timestamp('NaN'::double precision)",
        "SELECT * FROM runtime_values",
        "ROLLBACK",
        "SELECT * FROM runtime_values ORDER BY id",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn matches_runtime_prepared_metadata_and_decoding() {
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
        "SELECT to_timestamp($1)",
        "SELECT floor($1)",
        "SELECT floor($1::numeric)",
        "SELECT $1::integer AS input, floor($1)",
        "SELECT $1::timestamp AS input, $1 AT TIME ZONE 'UTC'",
        "SELECT $1::date AS input, to_char($1, 'YYYY')",
        "SELECT $1::timestamp AS input, date_trunc('day', $1, 'UTC')",
        "SELECT to_char($1::timestamp, $2)",
        "SELECT date_trunc($1, $2::timestamp)",
        "SELECT date_trunc($1, $2::timestamptz, $3)",
        "SELECT $1 AT TIME ZONE $2",
        "SELECT $1::timestamp AT TIME ZONE $2",
        "SELECT $1 LIKE $2, $1 ILIKE $2, $1 !~* $2",
        "SELECT regexp_like($1, $2, $3)",
        "SELECT string_agg($1::text, $2 ORDER BY value) FROM (VALUES (1), (2)) AS input(value)",
    ] {
        let expected = runtime
            .block_on(postgres.prepare(SqlStr::from_static(sql)))
            .unwrap();
        let actual = runtime
            .block_on(fake.prepare(SqlStr::from_static(sql)))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(
            actual
                .columns()
                .iter()
                .map(|c| (c.name().to_owned(), c.type_info().name().to_owned()))
                .collect::<Vec<_>>(),
            expected
                .columns()
                .iter()
                .map(|c| (c.name().to_owned(), c.type_info().name().to_owned()))
                .collect::<Vec<_>>(),
            "{sql}"
        );
        let actual_parameters = actual
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|t| t.name().to_owned())
            .collect::<Vec<_>>();
        let expected_parameters = expected
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|t| t.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(actual_parameters, expected_parameters, "{sql}");
    }
    for sql in [
        "SELECT $1::boolean AS value, to_timestamp($1)",
        "SELECT $1::integer AS value, $1 LIKE '%'",
        "SELECT $1::integer AS value, $1 ~* '%'",
        "SELECT $1::integer AS value, '2000-01-01'::timestamp AT TIME ZONE $1",
    ] {
        let expected = runtime
            .block_on(postgres.prepare(SqlStr::from_static(sql)))
            .unwrap_err();
        let actual = runtime
            .block_on(fake.prepare(SqlStr::from_static(sql)))
            .unwrap_err();
        assert_eq!(
            actual.as_database_error().unwrap().code(),
            expected.as_database_error().unwrap().code(),
            "{sql}"
        );
    }
    let sql = "SELECT to_timestamp($1), floor($1), to_char(to_timestamp($1), $2), $3 ILIKE $4, regexp_like($3, $5, 'i')";
    runtime
        .block_on(sqlx::query("SET TIME ZONE 'UTC'").execute(&mut postgres))
        .unwrap();
    let expected = runtime
        .block_on(
            sqlx::query(sql)
                .bind(-1.25_f64)
                .bind("YYYY-MM-DD HH24:MI:SS.US")
                .bind("Alpha")
                .bind("a%")
                .bind("^a")
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let actual = runtime
        .block_on(
            sqlx::query(sql)
                .bind(-1.25_f64)
                .bind("YYYY-MM-DD HH24:MI:SS.US")
                .bind("Alpha")
                .bind("a%")
                .bind("^a")
                .fetch_one(&mut fake),
        )
        .unwrap();
    assert_eq!(
        actual.get::<chrono::DateTime<chrono::Utc>, _>(0),
        expected.get::<chrono::DateTime<chrono::Utc>, _>(0)
    );
    assert_eq!(actual.get::<f64, _>(1), expected.get::<f64, _>(1));
    assert_eq!(actual.get::<String, _>(2), expected.get::<String, _>(2));
    assert_eq!(actual.get::<bool, _>(3), expected.get::<bool, _>(3));
    assert_eq!(actual.get::<bool, _>(4), expected.get::<bool, _>(4));
    let mut session = db.create_session();
    let prepared = session
        .prepare("SELECT to_char('2000-01-01+00'::timestamptz, 'HH24:MI OF')")
        .unwrap();
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows[0][0],
        pg_fake::value::Value::Text("00:00 +00".into())
    );
    session.execute("SET TIME ZONE '+03:00'").unwrap();
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows[0][0],
        pg_fake::value::Value::Text("21:00 -03".into())
    );
}

#[test]
fn rejects_runtime_shapes_outside_the_documented_scope() {
    let db = Db::create();
    let mut session = db.create_session();
    for sql in [
        "SELECT to_timestamp('2024-01-01', 'YYYY-MM-DD')",
        "SELECT to_char(12, '999')",
        "SELECT to_char('2000-01-01'::timestamp, 'MONTH')",
        "SELECT date_trunc('day', '1 day'::interval)",
        "SELECT '2000-01-01'::timestamp AT TIME ZONE INTERVAL '1 hour'",
        "SELECT date_trunc('century', '2000-01-01'::timestamp)",
        "SELECT regexp_like('abc', '(?=a)')",
        "SELECT regexp_like('a1', '\\d')",
        "SELECT regexp_like('abc', '[[:alpha:]]')",
        "SELECT regexp_like('abc', '[a-z&&b]')",
        "SELECT regexp_like('abc', 'a', 'n')",
        "SELECT 'É' ILIKE 'é'",
        "SELECT 'é' ~* '.'",
        "SELECT 'a' LIKE 'a' COLLATE \"C\"",
        "SELECT string_agg(DISTINCT label, ',') FROM (VALUES ('a')) AS input(label)",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            pg_fake::error::SqlState::FeatureNotSupported,
            "{sql}"
        );
    }
}
