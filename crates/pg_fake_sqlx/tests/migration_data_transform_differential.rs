use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Executor, Row, Statement, TypeInfo};
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
        "SELECT id, count(*) OVER ordered, count(sort_key) OVER ordered, sum(id) OVER ordered, avg(id) OVER ordered, min(id) OVER ordered, max(id) OVER ordered FROM transform_window_edges WINDOW ordered AS (PARTITION BY payload ORDER BY sort_key NULLS FIRST) ORDER BY id",
        "SELECT id, sum(id) OVER (ORDER BY sort_key NULLS FIRST ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING), sum(id) OVER (ORDER BY sort_key NULLS FIRST RANGE BETWEEN 1 PRECEDING AND 1 FOLLOWING), sum(id) OVER (ORDER BY sort_key NULLS FIRST GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM transform_window_edges ORDER BY id",
        "SELECT id, first_value(id) OVER framed, last_value(id) OVER framed, nth_value(id, 2) OVER framed, count(*) FILTER (WHERE id % 2 = 1) OVER framed FROM transform_window_edges WINDOW framed AS (ORDER BY id ROWS BETWEEN CURRENT ROW AND 1 FOLLOWING) ORDER BY id",
        "SELECT payload, sum(count(*)) OVER (ORDER BY payload NULLS FIRST) FROM transform_window_edges GROUP BY payload HAVING count(*) > 0 ORDER BY payload NULLS FIRST",
        "SELECT id, count(*) OVER (ORDER BY id RANGE BETWEEN 2147483647 FOLLOWING AND UNBOUNDED FOLLOWING), count(*) OVER (ORDER BY id DESC RANGE BETWEEN 2147483647 PRECEDING AND UNBOUNDED FOLLOWING) FROM transform_window_edges ORDER BY id",
        "SELECT id, sum(id) OVER (ORDER BY d RANGE INTERVAL '1 day' PRECEDING), count(*) OVER (ORDER BY tm RANGE INTERVAL '30 minutes' PRECEDING), sum(id) OVER (ORDER BY ts RANGE INTERVAL '1 hour' PRECEDING), sum(id) OVER (ORDER BY tz RANGE INTERVAL '1 hour' PRECEDING), sum(id) OVER (ORDER BY iv RANGE INTERVAL '1 hour' PRECEDING) FROM (VALUES (1, '2024-01-01'::date, '00:00'::time, '2024-01-01 00:00'::timestamp, '2024-01-01 00:00:00+00'::timestamptz, INTERVAL '1 hour'), (2, '2024-01-02', '00:10', '2024-01-01 00:30', '2024-01-01 00:30:00+00', INTERVAL '90 minutes'), (3, '2024-02-01', '23:50', '2024-02-01 00:00', '2024-02-01 00:00:00+00', INTERVAL '3 hours')) AS temporal(id, d, tm, ts, tz, iv) ORDER BY id",
        "SELECT id, sum(id) OVER (ORDER BY ts RANGE INTERVAL '1 month -1 day' PRECEDING), sum(id) OVER (ORDER BY ts RANGE INTERVAL '-1 month 40 days' PRECEDING) FROM (VALUES (1, '2024-01-01'::timestamp), (2, '2024-01-15'), (3, '2024-02-01')) AS temporal(id, ts) ORDER BY id",
        "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '1 day' PRECEDING) FROM (VALUES ('01:00'::time), ('02:00')) AS temporal(tm)",
        "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '-1 day 1 hour' PRECEDING) FROM (VALUES ('01:00'::time), ('02:00')) AS temporal(tm)",
        "SELECT x, count(*) OVER (ORDER BY x RANGE CAST('Infinity' AS float4) PRECEDING) FROM (VALUES ('-Infinity'::float4), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT x, count(*) OVER (ORDER BY x DESC RANGE CAST('Infinity' AS float4) PRECEDING) FROM (VALUES ('-Infinity'::float4), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT x, count(*) OVER (ORDER BY x RANGE BETWEEN CURRENT ROW AND CAST('Infinity' AS float4) FOLLOWING) FROM (VALUES ('-Infinity'::float4), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT x, count(*) OVER (ORDER BY x DESC RANGE BETWEEN CURRENT ROW AND CAST('Infinity' AS float4) FOLLOWING) FROM (VALUES ('-Infinity'::float4), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT x, count(*) OVER (ORDER BY x RANGE CAST('Infinity' AS float8) PRECEDING) FROM (VALUES ('-Infinity'::float8), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT x, count(*) OVER (ORDER BY x DESC RANGE CAST('Infinity' AS float8) PRECEDING) FROM (VALUES ('-Infinity'::float8), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT x, count(*) OVER (ORDER BY x RANGE BETWEEN CURRENT ROW AND CAST('Infinity' AS float8) FOLLOWING) FROM (VALUES ('-Infinity'::float8), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT x, count(*) OVER (ORDER BY x DESC RANGE BETWEEN CURRENT ROW AND CAST('Infinity' AS float8) FOLLOWING) FROM (VALUES ('-Infinity'::float8), (1), ('Infinity')) AS values(x) ORDER BY x",
        "SELECT count(*) OVER (ORDER BY id RANGE -1 PRECEDING) FROM transform_window_edges WHERE false",
        "SELECT count(*) OVER (ORDER BY value RANGE -1 PRECEDING) FROM (VALUES (NULL::integer)) AS values(value)",
        "SELECT count(*) OVER (ORDER BY value RANGE CAST('NaN' AS float8) PRECEDING) FROM (VALUES (NULL::float8)) AS values(value)",
        "SELECT d, count(*) OVER (ORDER BY d RANGE BETWEEN CURRENT ROW AND INTERVAL '1 day' FOLLOWING) FROM (VALUES ('2024-01-01'::date), ('infinity'::date)) AS values(d) ORDER BY d",
        "SELECT d, count(*) OVER (ORDER BY d RANGE INTERVAL '1 day' PRECEDING) FROM (VALUES ('-infinity'::date), ('2024-01-01'::date)) AS values(d) ORDER BY d",
        "SELECT d, count(*) OVER (ORDER BY d DESC RANGE BETWEEN CURRENT ROW AND INTERVAL '1 day' FOLLOWING) FROM (VALUES ('-infinity'::date), ('2024-01-01'::date)) AS values(d) ORDER BY d",
        "SELECT d, count(*) OVER (ORDER BY d DESC RANGE INTERVAL '1 day' PRECEDING) FROM (VALUES ('2024-01-01'::date), ('infinity'::date)) AS values(d) ORDER BY d",
        "SELECT id, count(*) OVER (ORDER BY id ROWS '1' PRECEDING), count(*) OVER (ORDER BY id RANGE '1' PRECEDING) FROM transform_window_edges ORDER BY id",
        "SELECT count(*) OVER (ORDER BY ts RANGE '1 day' PRECEDING) FROM (VALUES ('2024-01-01'::timestamp), ('2024-01-02'::timestamp)) AS values(ts)",
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
        "SELECT lag(id) IGNORE NULLS OVER () FROM transform_window_edges",
        "SELECT lag(id) RESPECT NULLS OVER () FROM transform_window_edges",
        "SELECT lag(id, 1, 1, 1) OVER () FROM transform_window_edges",
        "SELECT nth_value(id, 0) OVER () FROM transform_window_edges",
        "SELECT lag(id, NULL, 'x') OVER () FROM transform_window_edges",
        "SELECT lag(id, 0, 'x') OVER () FROM transform_window_edges",
        "SELECT lag(id, 1, 'x') OVER () FROM transform_window_edges WHERE false",
        "SELECT nth_value(id, '2147483648') OVER () FROM transform_window_edges WHERE false",
        "SELECT lag(id, 1, 'x'::integer) OVER () FROM transform_window_edges WHERE false",
        "SELECT nth_value(id, '2147483648'::integer) OVER () FROM transform_window_edges WHERE false",
        "SELECT sum(id) OVER (ROWS UNBOUNDED FOLLOWING) FROM transform_window_edges",
        "SELECT sum(id) OVER (ROWS BETWEEN CURRENT ROW AND 1 PRECEDING) FROM transform_window_edges",
        "SELECT sum(id) OVER (RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM transform_window_edges",
        "SELECT sum(id) OVER (ORDER BY id ROWS -1 PRECEDING) FROM transform_window_edges",
        "SELECT row_number() OVER (ROWS UNBOUNDED FOLLOWING) FROM transform_window_edges",
        "SELECT lag(id) OVER (ROWS UNBOUNDED FOLLOWING) FROM transform_window_edges",
        "SELECT first_value(id) OVER (ROWS UNBOUNDED FOLLOWING) FROM transform_window_edges",
        "SELECT sum(id) OVER (ORDER BY id ROWS id PRECEDING) FROM transform_window_edges",
        "SELECT sum(id) OVER (ORDER BY id ROWS (SELECT id) PRECEDING) FROM transform_window_edges",
        "SELECT sum(id) OVER (ORDER BY id ROWS count(*) PRECEDING) FROM transform_window_edges",
        "SELECT sum(id) OVER (ORDER BY id ROWS row_number() OVER () PRECEDING) FROM transform_window_edges",
        "SELECT sum(id) OVER (ORDER BY id::double precision RANGE CAST('NaN' AS double precision) PRECEDING) FROM transform_window_edges",
        "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '1 day -1 hour' PRECEDING) FROM (VALUES ('01:00'::time), ('02:00')) AS temporal(tm)",
        "SELECT count(*) OVER (ORDER BY id::float8 RANGE CAST('-Infinity' AS float8) PRECEDING) FROM transform_window_edges",
        "SELECT count(*) OVER (ORDER BY value RANGE INTERVAL '-1 day' PRECEDING) FROM (VALUES ('-infinity'::date)) AS temporal(value)",
        "SELECT count(*) OVER (ORDER BY value DESC RANGE BETWEEN CURRENT ROW AND INTERVAL '-1 day' FOLLOWING) FROM (VALUES ('infinity'::date)) AS temporal(value)",
        "SELECT count(*) OVER (ORDER BY value DESC RANGE INTERVAL '-1 day' PRECEDING) FROM (VALUES ('-infinity'::timestamp)) AS temporal(value)",
        "SELECT count(*) OVER (ORDER BY value RANGE BETWEEN CURRENT ROW AND INTERVAL '-1 day' FOLLOWING) FROM (VALUES ('infinity'::timestamp)) AS temporal(value)",
        "SELECT count(*) OVER (ORDER BY value RANGE INTERVAL '-1 day' PRECEDING) FROM (VALUES ('-infinity'::timestamptz)) AS temporal(value)",
        "SELECT count(*) OVER (ORDER BY value DESC RANGE BETWEEN CURRENT ROW AND INTERVAL '-1 day' FOLLOWING) FROM (VALUES ('infinity'::timestamptz)) AS temporal(value)",
        "SELECT sum(id) OVER (ROWS -1 PRECEDING) FROM transform_window_edges WHERE false",
        "SELECT sum(id) OVER (ROWS NULL PRECEDING) FROM transform_window_edges WHERE false",
        "SELECT sum(id) OVER (ORDER BY id GROUPS -1 PRECEDING) FROM transform_window_edges WHERE false",
        "SELECT sum(id) OVER (ORDER BY id GROUPS (1 / 0) PRECEDING) FROM transform_window_edges WHERE false",
        "SELECT sum(id) OVER (ROWS ('-1') PRECEDING) FROM transform_window_edges WHERE false",
        "SELECT count(*) OVER (ORDER BY ts RANGE '-1 day' PRECEDING) FROM (VALUES ('infinity'::timestamp)) AS values(ts)",
        "SELECT sum(DISTINCT id) OVER () FROM transform_window_edges",
        "SELECT sum(id ORDER BY sort_key) OVER () FROM transform_window_edges",
        "SELECT sum(row_number() OVER ()) OVER () FROM transform_window_edges",
        "SELECT 9223372036854775807::bigint * 2::bigint",
        "SELECT 'not-a-uuid'::uuid",
        "SELECT 'not-a-number'::numeric",
        "SELECT INTERVAL '2562047789:00:00'",
        "SELECT '2000-01-01 00:00:00'::timestamp - INTERVAL '-2147483648 days'",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    let parameter_sql = "SELECT lag(id, $1, $2) OVER (ORDER BY id), \
                                lead(id, $1, $2) OVER (ORDER BY id), \
                                nth_value(id, $3) OVER (ORDER BY id) \
                         FROM transform_window_edges ORDER BY id";
    let postgres_rows = runtime
        .block_on(
            sqlx::query(parameter_sql)
                .bind(2_i32)
                .bind(-1_i32)
                .bind(2_i32)
                .fetch_all(&mut postgres),
        )
        .unwrap();
    let fake_rows = runtime
        .block_on(
            sqlx::query(parameter_sql)
                .bind(2_i32)
                .bind(-1_i32)
                .bind(2_i32)
                .fetch_all(&mut fake),
        )
        .unwrap();
    let postgres_values = postgres_rows
        .iter()
        .map(|row| {
            (0..3)
                .map(|index| row.get::<Option<i32>, _>(index))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let fake_values = fake_rows
        .iter()
        .map(|row| {
            (0..3)
                .map(|index| row.get::<Option<i32>, _>(index))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(fake_values, postgres_values);
    let postgres_window_metadata = postgres_rows[0]
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let fake_window_metadata = fake_rows[0]
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(fake_window_metadata, postgres_window_metadata);

    for sql in [
        "CREATE TABLE transform_window_times (id INTEGER, value TIMESTAMPTZ)",
        "INSERT INTO transform_window_times VALUES (1, '2024-01-01 00:00:00+00')",
        "SET TIME ZONE 'UTC'",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    let prepared_time_sql = "SELECT lag(value, 1, '2020-01-01 00:00') OVER (ORDER BY id) \
         FROM transform_window_times";
    let postgres_time_statement = runtime
        .block_on(postgres.prepare(prepared_time_sql))
        .unwrap();
    let fake_time_statement = runtime.block_on(fake.prepare(prepared_time_sql)).unwrap();
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SET TIME ZONE '+03:00'",
        RowOrder::Ordered,
    );
    let postgres_time = runtime
        .block_on(postgres_time_statement.query().fetch_one(&mut postgres))
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    let fake_time = runtime
        .block_on(fake_time_statement.query().fetch_one(&mut fake))
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    assert_eq!(fake_time, postgres_time);

    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SET TIME ZONE 'UTC'",
        RowOrder::Ordered,
    );
    let explicit_prepared_time_sql = "SELECT lag(value, 1, ('2020-01-01 00:00')::timestamptz) OVER (ORDER BY id) \
         FROM transform_window_times";
    let postgres_explicit_time_statement = runtime
        .block_on(postgres.prepare(explicit_prepared_time_sql))
        .unwrap();
    let fake_explicit_time_statement = runtime
        .block_on(fake.prepare(explicit_prepared_time_sql))
        .unwrap();
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SET TIME ZONE '+03:00'",
        RowOrder::Ordered,
    );
    let postgres_explicit_time = runtime
        .block_on(
            postgres_explicit_time_statement
                .query()
                .fetch_one(&mut postgres),
        )
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    let fake_explicit_time = runtime
        .block_on(fake_explicit_time_statement.query().fetch_one(&mut fake))
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    assert_eq!(fake_explicit_time, postgres_explicit_time);

    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SET search_path = pg_catalog, public",
        RowOrder::Ordered,
    );
    let postgres_replanned_explicit_time = runtime
        .block_on(
            postgres_explicit_time_statement
                .query()
                .fetch_one(&mut postgres),
        )
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    let fake_replanned_explicit_time = runtime
        .block_on(fake_explicit_time_statement.query().fetch_one(&mut fake))
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    assert_eq!(
        fake_replanned_explicit_time,
        postgres_replanned_explicit_time
    );
    let postgres_replanned_time = runtime
        .block_on(postgres_time_statement.query().fetch_one(&mut postgres))
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    let fake_replanned_time = runtime
        .block_on(fake_time_statement.query().fetch_one(&mut fake))
        .unwrap()
        .get::<chrono::DateTime<chrono::Utc>, _>(0);
    assert_eq!(fake_replanned_time, postgres_replanned_time);

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
