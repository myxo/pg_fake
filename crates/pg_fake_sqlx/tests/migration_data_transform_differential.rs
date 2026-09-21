use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Row, TypeInfo};
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;

use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn matches_migration_data_transform_queries() {
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
        "CREATE TABLE transform_source (id INTEGER, label TEXT, payload JSONB)",
        "CREATE TABLE transform_destination (id INTEGER PRIMARY KEY, amount BIGINT)",
        "CREATE TABLE transform_window_edges (id INTEGER, sort_key INTEGER, payload JSONB)",
        r#"INSERT INTO transform_source VALUES
             (3, NULL, NULL),
             (1, ' alpha ', '{"amount":{"value":"7"}}'),
             (2, 'beta', '{"amount":{"value":"12"}}'),
             (4, NULL, NULL)"#,
        r#"INSERT INTO transform_window_edges VALUES
             (1, 1, '{"a":1,"b":2}'),
             (2, 1, '{"b":2,"a":1}'),
             (3, NULL, '1'),
             (4, NULL, '1.0'),
             (5, 2, NULL)"#,
        "SELECT id, row_number() OVER (ORDER BY id DESC NULLS FIRST) FROM transform_source ORDER BY id",
        "SELECT id, count(*) OVER (PARTITION BY payload) FROM transform_source ORDER BY id",
        "SELECT id, row_number() OVER (ORDER BY sort_key NULLS FIRST), count(*) OVER (PARTITION BY payload) FROM transform_window_edges ORDER BY id",
        "SELECT id, rank() OVER ordered, dense_rank() OVER ordered, percent_rank() OVER ordered, cume_dist() OVER ordered, ntile(3) OVER ordered FROM transform_window_edges WINDOW base AS (PARTITION BY payload), ordered AS (base ORDER BY sort_key NULLS FIRST) ORDER BY id",
        "SELECT id, lag(id, 1, -1) OVER ordered, lead(id, 2, -2) OVER ordered, first_value(id) OVER ordered, last_value(id) OVER ordered, nth_value(id, 2) OVER ordered FROM transform_window_edges WINDOW ordered AS (PARTITION BY payload ORDER BY sort_key NULLS FIRST) ORDER BY id",
        "SELECT payload, rank() OVER (ORDER BY payload) FROM transform_window_edges GROUP BY payload HAVING count(*) > 0 ORDER BY payload NULLS FIRST",
        "SELECT id, row_number() OVER (ORDER BY sort_key), count(*) OVER (PARTITION BY payload) FROM transform_window_edges WHERE false",
        "SELECT count(*), max(id), string_agg(btrim(label), ':' ORDER BY id DESC) FROM transform_source",
        "SELECT count(*), max(id), string_agg(label, ',' ORDER BY id) FROM transform_source WHERE false",
        "SELECT string_agg(value, delimiter ORDER BY ordering), (SELECT count(*) FROM transform_source WHERE false), (SELECT max(id) FROM transform_source WHERE false) FROM (VALUES ('a', '-', 1), ('b', NULL, 2), (NULL, ':', 3), ('c', '/', 4)) AS edges(value, delimiter, ordering)",
        "SELECT NULL IS DISTINCT FROM NULL, 1 IS DISTINCT FROM NULL, 1 IS NOT DISTINCT FROM 1, 2 IN (1, 2, NULL)",
        "SELECT EXISTS (SELECT 1 WHERE true), NOT EXISTS (SELECT 1 WHERE false), 'ABC-12' ~ '^[A-Z]{3}-([0-9]{2})$', 'ABC' ~ '^ABC(-[0-9]{1,2})?$'",
        "SELECT coalesce(NULL::bigint, 2::numeric::bigint), btrim(' x '), jsonb_typeof('{}'), CASE WHEN true THEN 3::bigint ELSE 4 END",
        "SELECT '123e4567-e89b-12d3-a456-426614174000'::uuid::text, 2::numeric * 3::bigint > 5::numeric",
        "SELECT extract(epoch FROM '1970-01-01 00:00:05.6+00'::timestamptz), extract(epoch FROM '1969-12-31 23:59:59.6+00'::timestamptz), extract(epoch FROM '1970-01-01 00:00:05.6+00'::timestamptz)::bigint, (CURRENT_TIMESTAMP + INTERVAL '7 days') = (CURRENT_TIMESTAMP + INTERVAL '168 hours')",
        "WITH selected AS MATERIALIZED (SELECT id, (payload #>> '{amount,value}')::numeric::bigint AS amount FROM transform_source WHERE payload IS NOT NULL) INSERT INTO transform_destination SELECT id, amount FROM selected ORDER BY id ON CONFLICT (id) DO NOTHING",
        "UPDATE transform_destination AS destination SET amount = source.amount * 2 FROM (SELECT id, amount FROM transform_destination) AS source WHERE destination.id = source.id",
        "UPDATE transform_destination AS destination SET amount = (SELECT source.amount + 1 FROM transform_destination source WHERE source.id = destination.id ORDER BY source.id LIMIT 1)",
        "SELECT destination.id, (SELECT max(candidate.amount) FROM transform_destination candidate WHERE candidate.id <= destination.id LIMIT 1) FROM transform_destination destination ORDER BY destination.id",
        "WITH left_side AS (SELECT * FROM transform_destination), right_side AS (SELECT * FROM transform_source) SELECT left_side.id, right_side.label FROM left_side LEFT JOIN right_side ON left_side.id = right_side.id ORDER BY left_side.id LIMIT 2",
        "DO $$ DECLARE total BIGINT; maximum BIGINT; labels TEXT; BEGIN SELECT count(*), max(id), string_agg(label, ',' ORDER BY id) INTO total, maximum, labels FROM transform_source; IF total IS DISTINCT FROM 4 OR maximum IS DISTINCT FROM 4 OR labels IS DISTINCT FROM ' alpha ,beta' THEN RAISE EXCEPTION 'unexpected aggregate values'; END IF; END $$",
    ] {
        let row_order =
            if sql.trim_start().starts_with("SELECT") || sql.trim_start().starts_with("WITH") {
                RowOrder::Ordered
            } else {
                RowOrder::Unordered
            };
        assert_statement(&runtime, &mut postgres, &mut fake, sql, row_order);
    }

    for sql in [
        "SELECT 'x' ~ '[unterminated'",
        "SELECT 'x' ~ 'x{256}'",
        "SELECT 1 ~ '1'",
        "SELECT count()",
        "SELECT count() OVER (PARTITION BY id) FROM transform_source",
        "SELECT 9223372036854775807::bigint * 2::bigint",
        "SELECT 'not-a-uuid'::uuid",
        "SELECT 'not-a-number'::numeric",
        "SELECT INTERVAL '2562047789:00:00'",
        "SELECT '2000-01-01 00:00:00'::timestamp - INTERVAL '-2147483648 days'",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    for sql in [
        "BEGIN",
        "UPDATE transform_destination SET amount = 99 WHERE id = 1",
        "ROLLBACK",
        "SELECT * FROM transform_destination ORDER BY id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    let metadata_sql = "SELECT count(*), max(id), string_agg(label, ',' ORDER BY id), \
                              btrim(' x '), \
                              extract(epoch FROM '1970-01-01 00:00:05.6+00'::timestamptz), \
                              CURRENT_TIMESTAMP + INTERVAL '7 days' \
                        FROM transform_source";
    let postgres_row = runtime
        .block_on(sqlx::query(metadata_sql).fetch_one(&mut postgres))
        .unwrap();
    let fake_row = runtime
        .block_on(sqlx::query(metadata_sql).fetch_one(&mut fake))
        .unwrap();
    let postgres_metadata = postgres_row
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let fake_metadata = fake_row
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(fake_metadata, postgres_metadata);
}
