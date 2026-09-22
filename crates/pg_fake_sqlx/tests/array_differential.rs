use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Executor, Row, Statement, TypeInfo};
use sqlx_postgres::PgConnection;
use uuid::Uuid;

mod common;

use common::start_postgres_server;

fn get_sqlstate(error: sqlx::Error) -> String {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .expect("database errors must expose SQLSTATE")
        .into_owned()
}

#[tokio::test]
async fn matches_polymorphic_array_function_unknown_inference() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let sql = "SELECT array_append(NULL, 1)::text, array_prepend(1, NULL)::text, \
                      array_cat(ARRAY[1], NULL)::text, array_cat(NULL, ARRAY[1])::text, \
                      array_cat(ARRAY[1], '{2}')::text, array_position(NULL, 1)::text, \
                      array_positions(NULL, 1)::text, array_remove(NULL, 1)::text, \
                      array_replace(NULL, 1, 2)::text";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    for index in 0..actual.len() {
        assert_eq!(
            actual.get::<Option<String>, _>(index),
            expected.get::<Option<String>, _>(index),
            "column {index}"
        );
    }
}

#[tokio::test]
async fn matches_postgres_bigint_and_uuid_array_codecs() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let create = "CREATE TABLE array_codec_values (id INTEGER PRIMARY KEY, required BIGINT[] NOT NULL, optional BIGINT[], identifiers UUID[] NOT NULL)";
    postgres
        .execute("DROP TABLE IF EXISTS array_codec_values")
        .await
        .unwrap();
    postgres.execute(create).await.unwrap();
    fake.execute(create).await.unwrap();
    let identifiers = vec![
        Some(Uuid::parse_str("a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7").unwrap()),
        None,
        Some(Uuid::parse_str("018f0f60-4bc5-7d4c-8a4b-23e99e0d3a90").unwrap()),
    ];
    let required = vec![Some(-9_i64), None, Some(7_i64)];
    let rows = vec![
        (1, required, None, identifiers),
        (
            2,
            vec![Some(4_i64)],
            Some(vec![8_i64, 9_i64]),
            vec![Some(Uuid::nil())],
        ),
    ];
    for (id, required, optional, identifiers) in &rows {
        sqlx::query("INSERT INTO array_codec_values VALUES ($1, $2, $3, $4)")
            .bind(*id)
            .bind(required.clone())
            .bind(optional.clone())
            .bind(identifiers.clone())
            .execute(&mut postgres)
            .await
            .unwrap();
        sqlx::query("INSERT INTO array_codec_values VALUES ($1, $2, $3, $4)")
            .bind(*id)
            .bind(required.clone())
            .bind(optional.clone())
            .bind(identifiers.clone())
            .execute(&mut fake)
            .await
            .unwrap();
    }
    let expected =
        sqlx::query("SELECT required, optional, identifiers FROM array_codec_values ORDER BY id")
            .fetch_all(&mut postgres)
            .await
            .unwrap();
    let actual =
        sqlx::query("SELECT required, optional, identifiers FROM array_codec_values ORDER BY id")
            .fetch_all(&mut fake)
            .await
            .unwrap();
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(
            actual.get::<Vec<Option<i64>>, _>(0),
            expected.get::<Vec<Option<i64>>, _>(0)
        );
        assert_eq!(
            actual.get::<Option<Vec<i64>>, _>(1),
            expected.get::<Option<Vec<i64>>, _>(1)
        );
        assert_eq!(
            actual.get::<Vec<Option<Uuid>>, _>(2),
            expected.get::<Vec<Option<Uuid>>, _>(2)
        );
    }
    assert_eq!(actual[0].columns()[0].type_info().name(), "INT8[]");
    assert_eq!(actual[0].columns()[2].type_info().name(), "UUID[]");
}

#[tokio::test]
async fn matches_postgres_remaining_scalar_array_codecs() {
    use bigdecimal::BigDecimal;
    use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
    use serde_json::json;
    use sqlx::types::Json;

    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let create = "CREATE TABLE remaining_array_codecs (\
        booleans boolean[], smallints smallint[], integers integer[], reals real[], \
        doubles double precision[], numerics numeric[], strings text[], bytes bytea[], \
        dates date[], times time[], timestamps timestamp[], timestamptzs timestamptz[], \
        documents jsonb[])";
    postgres
        .execute("DROP TABLE IF EXISTS remaining_array_codecs")
        .await
        .unwrap();
    postgres.execute(create).await.unwrap();
    fake.execute(create).await.unwrap();

    let booleans = vec![Some(true), None, Some(false)];
    let smallints = vec![Some(-2_i16), None, Some(4_i16)];
    let integers = vec![Some(-3_i32), None, Some(5_i32)];
    let reals = vec![Some(1.25_f32), None, Some(-2.5_f32)];
    let doubles = vec![Some(1.5_f64), None, Some(-3.75_f64)];
    let numerics = vec![
        Some(BigDecimal::from(123)),
        None,
        Some(BigDecimal::from(-4)),
    ];
    let strings = vec![
        Some("plain".to_owned()),
        None,
        Some("comma,value".to_owned()),
    ];
    let bytes = vec![Some(vec![0_u8, 1, 255]), None, Some(Vec::new())];
    let dates = vec![
        Some(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
        None,
        Some(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()),
    ];
    let times = vec![
        Some(NaiveTime::from_hms_micro_opt(3, 4, 5, 600).unwrap()),
        None,
        Some(NaiveTime::from_hms_opt(23, 59, 59).unwrap()),
    ];
    let timestamps = vec![
        Some(NaiveDateTime::new(dates[0].unwrap(), times[0].unwrap())),
        None,
        Some(NaiveDateTime::new(dates[2].unwrap(), times[2].unwrap())),
    ];
    let timestamptzs = vec![
        Some(Utc.from_utc_datetime(&timestamps[0].unwrap())),
        None,
        Some(Utc.from_utc_datetime(&timestamps[2].unwrap())),
    ];
    let documents = vec![Some(Json(json!({"a": 1}))), None, Some(Json(json!([2, 3])))];
    let insert = "INSERT INTO remaining_array_codecs VALUES \
        ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)";
    sqlx::query(insert)
        .bind(booleans.clone())
        .bind(smallints.clone())
        .bind(integers.clone())
        .bind(reals.clone())
        .bind(doubles.clone())
        .bind(numerics.clone())
        .bind(strings.clone())
        .bind(bytes.clone())
        .bind(dates.clone())
        .bind(times.clone())
        .bind(timestamps.clone())
        .bind(timestamptzs.clone())
        .bind(documents.clone())
        .execute(&mut postgres)
        .await
        .unwrap();
    sqlx::query(insert)
        .bind(booleans)
        .bind(smallints)
        .bind(integers)
        .bind(reals)
        .bind(doubles)
        .bind(numerics)
        .bind(strings)
        .bind(bytes)
        .bind(dates)
        .bind(times)
        .bind(timestamps)
        .bind(timestamptzs)
        .bind(documents)
        .execute(&mut fake)
        .await
        .unwrap();

    let expected = sqlx::query("SELECT * FROM remaining_array_codecs")
        .fetch_one(&mut postgres)
        .await
        .unwrap();
    let actual = sqlx::query("SELECT * FROM remaining_array_codecs")
        .fetch_one(&mut fake)
        .await
        .unwrap();
    macro_rules! compare {
        ($index:expr, $type:ty) => {
            assert_eq!(
                actual.get::<Vec<Option<$type>>, _>($index),
                expected.get::<Vec<Option<$type>>, _>($index),
                "column {}",
                $index
            );
        };
    }
    compare!(0, bool);
    compare!(1, i16);
    compare!(2, i32);
    compare!(3, f32);
    compare!(4, f64);
    compare!(5, BigDecimal);
    compare!(6, String);
    compare!(7, Vec<u8>);
    compare!(8, NaiveDate);
    compare!(9, NaiveTime);
    compare!(10, NaiveDateTime);
    compare!(11, chrono::DateTime<Utc>);
    compare!(12, Json<serde_json::Value>);
}

#[tokio::test]
async fn matches_prepared_array_metadata_and_literals() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let sql = "SELECT ARRAY[1::BIGINT, NULL, -3], ARRAY['a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7'::UUID, NULL], ARRAY[]::BIGINT[]";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    for index in [0, 2] {
        assert_eq!(
            actual.get::<Vec<Option<i64>>, _>(index),
            expected.get::<Vec<Option<i64>>, _>(index),
            "{sql}"
        );
    }
    assert_eq!(
        actual.get::<Vec<Option<Uuid>>, _>(1),
        expected.get::<Vec<Option<Uuid>>, _>(1),
        "{sql}"
    );
    let sql = r#"SELECT '{-1,NULL,2}'::BIGINT[], '{a,"comma,value",NULL}'::TEXT[]"#;
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    assert_eq!(
        actual.get::<Vec<Option<i64>>, _>(0),
        expected.get::<Vec<Option<i64>>, _>(0),
        "{sql}"
    );
    assert_eq!(
        actual.get::<Vec<Option<String>>, _>(1),
        expected.get::<Vec<Option<String>>, _>(1),
        "{sql}"
    );
    let sql = "SELECT ARRAY['-1', NULL, '2']::TEXT[]::BIGINT[], ARRAY['a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7', NULL]::TEXT[]::UUID[]";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    assert_eq!(
        actual.get::<Vec<Option<i64>>, _>(0),
        expected.get::<Vec<Option<i64>>, _>(0),
        "{sql}"
    );
    assert_eq!(
        actual.get::<Vec<Option<Uuid>>, _>(1),
        expected.get::<Vec<Option<Uuid>>, _>(1),
        "{sql}"
    );
    let expected = postgres
        .prepare("SELECT $1::BIGINT[] AS numbers, $2::UUID[] AS identifiers, $3::TEXT[] AS labels")
        .await
        .unwrap();
    let actual = fake
        .prepare("SELECT $1::BIGINT[] AS numbers, $2::UUID[] AS identifiers, $3::TEXT[] AS labels")
        .await
        .unwrap();
    assert_eq!(
        actual
            .columns()
            .iter()
            .map(|column| (column.name(), column.type_info().name()))
            .collect::<Vec<_>>(),
        expected
            .columns()
            .iter()
            .map(|column| (column.name(), column.type_info().name()))
            .collect::<Vec<_>>()
    );
    for (actual, expected) in actual
        .parameters()
        .unwrap()
        .left()
        .unwrap()
        .iter()
        .zip(expected.parameters().unwrap().left().unwrap())
    {
        assert_eq!(actual.base.unwrap().map_to_oid(), expected.oid().unwrap().0);
    }
    for sql in [
        "SELECT array_length($1::INTEGER[], $2)",
        "SELECT cardinality($1::TEXT[])",
        "SELECT array_append($1::INTEGER[], $2)",
        "SELECT array_prepend($1, $2::UUID[])",
        "SELECT array_cat($1::TEXT[], $2)",
        "SELECT array_position($1::UUID[], $2, $3)",
        "SELECT array_positions($1::DATE[], $2)",
        "SELECT array_remove($1::BOOLEAN[], $2)",
        "SELECT array_replace($1::NUMERIC[], $2, $3)",
        "SELECT value FROM unnest($1::DATE[]) AS expanded(value)",
    ] {
        let expected = postgres.prepare(sql).await.unwrap();
        let actual = fake.prepare(sql).await.unwrap();
        assert_eq!(
            actual
                .parameters()
                .unwrap()
                .left()
                .unwrap()
                .iter()
                .map(|data_type| data_type.base.unwrap().map_to_oid())
                .collect::<Vec<_>>(),
            expected
                .parameters()
                .unwrap()
                .left()
                .unwrap()
                .iter()
                .map(|data_type| data_type.oid().unwrap().0)
                .collect::<Vec<_>>(),
            "parameter metadata for {sql}"
        );
        assert_eq!(
            actual
                .columns()
                .iter()
                .map(|column| column.type_info().name())
                .collect::<Vec<_>>(),
            expected
                .columns()
                .iter()
                .map(|column| column.type_info().name())
                .collect::<Vec<_>>(),
            "result metadata for {sql}"
        );
    }
}

#[tokio::test]
async fn matches_untyped_uuid_array_assignment_error() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let create = "CREATE TABLE array_assignment_errors (identifiers UUID[])";
    postgres
        .execute("DROP TABLE IF EXISTS array_assignment_errors")
        .await
        .unwrap();
    postgres.execute(create).await.unwrap();
    fake.execute(create).await.unwrap();
    let sql = "INSERT INTO array_assignment_errors VALUES (ARRAY['a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7', NULL])";
    let expected = postgres
        .execute(sql)
        .await
        .map(|_| ())
        .map_err(get_sqlstate);
    let actual = fake.execute(sql).await.map(|_| ()).map_err(get_sqlstate);
    assert_eq!(actual, expected, "{sql}");
    assert_eq!(actual.unwrap_err(), "42804");
}

#[tokio::test]
async fn matches_array_assignment_element_casts() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let create = "CREATE TABLE array_assignment_casts (texts TEXT[])";
    postgres
        .execute("DROP TABLE IF EXISTS array_assignment_casts")
        .await
        .unwrap();
    postgres.execute(create).await.unwrap();
    fake.execute(create).await.unwrap();
    for sql in [
        "INSERT INTO array_assignment_casts VALUES (ARRAY[1::BIGINT, NULL])",
        "INSERT INTO array_assignment_casts VALUES (ARRAY['a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7'::UUID, NULL])",
    ] {
        let expected = postgres
            .execute(sql)
            .await
            .map(|_| ())
            .map_err(get_sqlstate);
        let actual = fake.execute(sql).await.map(|_| ()).map_err(get_sqlstate);
        assert_eq!(actual, expected, "{sql}");
    }
    let expected = sqlx::query("SELECT texts FROM array_assignment_casts")
        .fetch_all(&mut postgres)
        .await
        .unwrap();
    let actual = sqlx::query("SELECT texts FROM array_assignment_casts")
        .fetch_all(&mut fake)
        .await
        .unwrap();
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(
            actual.get::<Vec<Option<String>>, _>(0),
            expected.get::<Vec<Option<String>>, _>(0)
        );
    }
}

#[tokio::test]
async fn matches_required_bigint_aggregation_and_subscripts() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let create = "CREATE TABLE required_bigint_arrays (id INTEGER PRIMARY KEY, issued_at BIGINT)";
    postgres
        .execute("DROP TABLE IF EXISTS required_bigint_arrays")
        .await
        .unwrap();
    postgres.execute(create).await.unwrap();
    fake.execute(create).await.unwrap();
    let insert = "INSERT INTO required_bigint_arrays VALUES (1, 10), (2, 20), (3, 30), (4, NULL)";
    postgres.execute(insert).await.unwrap();
    fake.execute(insert).await.unwrap();
    let sql = "SELECT max(issued_at) AS latest, \
                      (array_agg(issued_at ORDER BY issued_at DESC) \
                          FILTER (WHERE issued_at <= $1))[$2] AS second \
               FROM required_bigint_arrays";
    let expected_statement = postgres.prepare(sql).await.unwrap();
    let actual_statement = fake.prepare(sql).await.unwrap();
    assert_eq!(
        actual_statement
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|data_type| data_type.base.unwrap().map_to_oid())
            .collect::<Vec<_>>(),
        expected_statement
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|data_type| data_type.oid().unwrap().0)
            .collect::<Vec<_>>()
    );
    let expected = sqlx::query(sql)
        .bind(30_i64)
        .bind(2_i32)
        .fetch_one(&mut postgres)
        .await
        .unwrap();
    let actual = sqlx::query(sql)
        .bind(30_i64)
        .bind(2_i32)
        .fetch_one(&mut fake)
        .await
        .unwrap();
    assert_eq!(actual.get::<Option<i64>, _>(0), expected.get(0));
    assert_eq!(actual.get::<Option<i64>, _>(1), expected.get(1));
    assert_eq!(actual.columns()[0].type_info().name(), "INT8");
    assert_eq!(actual.columns()[1].type_info().name(), "INT8");

    let sql = "SELECT array_agg(issued_at ORDER BY id), \
                      array_agg(issued_at ORDER BY id) FILTER (WHERE false) \
               FROM required_bigint_arrays";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    assert_eq!(
        actual.get::<Vec<Option<i64>>, _>(0),
        expected.get::<Vec<Option<i64>>, _>(0)
    );
    assert_eq!(
        actual.get::<Option<Vec<Option<i64>>>, _>(1),
        expected.get::<Option<Vec<Option<i64>>>, _>(1)
    );
    let sql =
        "SELECT array_agg(issued_at ORDER BY issued_at) FROM required_bigint_arrays WHERE false";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    assert_eq!(
        actual.get::<Option<Vec<Option<i64>>>, _>(0),
        expected.get::<Option<Vec<Option<i64>>>, _>(0)
    );

    let sql = "SELECT (ARRAY[10::BIGINT, 20, 30])[$1]";
    let expected_statement = postgres.prepare(sql).await.unwrap();
    let actual_statement = fake.prepare(sql).await.unwrap();
    assert_eq!(
        actual_statement.parameters().unwrap().left().unwrap()[0]
            .base
            .unwrap()
            .map_to_oid(),
        expected_statement.parameters().unwrap().left().unwrap()[0]
            .oid()
            .unwrap()
            .0
    );
    assert_eq!(actual_statement.columns()[0].type_info().name(), "INT8");
    for index in [1_i32, 2, 4, 0, -1, i32::MIN] {
        let expected = sqlx::query(sql)
            .bind(index)
            .fetch_one(&mut postgres)
            .await
            .unwrap()
            .get::<Option<i64>, _>(0);
        let actual = sqlx::query(sql)
            .bind(index)
            .fetch_one(&mut fake)
            .await
            .unwrap()
            .get::<Option<i64>, _>(0);
        assert_eq!(actual, expected, "index {index}");
    }
    let sql = "SELECT (ARRAY[10::BIGINT, 20])[NULL::INTEGER], (NULL::BIGINT[])[1]";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    assert_eq!(actual.get::<Option<i64>, _>(0), expected.get(0));
    assert_eq!(actual.get::<Option<i64>, _>(1), expected.get(1));
    for sql in [
        "SELECT (ARRAY[10::BIGINT])['bad']",
        "SELECT (ARRAY[10::BIGINT])[1.5]",
    ] {
        let expected = postgres
            .execute(sql)
            .await
            .map(|_| ())
            .map_err(get_sqlstate);
        let actual = fake.execute(sql).await.map(|_| ()).map_err(get_sqlstate);
        assert_eq!(actual, expected, "{sql}");
    }
}

#[tokio::test]
async fn matches_required_uuid_any_and_all() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let create = "CREATE TABLE required_uuid_membership (id UUID PRIMARY KEY)";
    postgres
        .execute("DROP TABLE IF EXISTS required_uuid_membership")
        .await
        .unwrap();
    postgres.execute(create).await.unwrap();
    fake.execute(create).await.unwrap();
    let ids = [
        Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
        Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap(),
        Uuid::parse_str("00000000-0000-4000-8000-000000000003").unwrap(),
    ];
    for id in ids {
        sqlx::query("INSERT INTO required_uuid_membership VALUES ($1)")
            .bind(id)
            .execute(&mut postgres)
            .await
            .unwrap();
        sqlx::query("INSERT INTO required_uuid_membership VALUES ($1)")
            .bind(id)
            .execute(&mut fake)
            .await
            .unwrap();
    }
    let any_sql = "SELECT id FROM required_uuid_membership WHERE id = ANY((($1))) ORDER BY id";
    let expected_statement = postgres.prepare(any_sql).await.unwrap();
    let actual_statement = fake.prepare(any_sql).await.unwrap();
    assert_eq!(
        actual_statement.parameters().unwrap().left().unwrap()[0]
            .base
            .unwrap()
            .map_to_oid(),
        expected_statement.parameters().unwrap().left().unwrap()[0]
            .oid()
            .unwrap()
            .0
    );
    let candidates = vec![Some(ids[2]), Some(ids[0]), None, Some(ids[2])];
    let expected = sqlx::query(any_sql)
        .bind(candidates.clone())
        .fetch_all(&mut postgres)
        .await
        .unwrap();
    let actual = sqlx::query(any_sql)
        .bind(candidates)
        .fetch_all(&mut fake)
        .await
        .unwrap();
    assert_eq!(
        actual
            .iter()
            .map(|row| row.get::<Uuid, _>(0))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|row| row.get::<Uuid, _>(0))
            .collect::<Vec<_>>()
    );

    let sql = "SELECT id FROM required_uuid_membership WHERE id <> ALL($1) ORDER BY id";
    let excluded = vec![ids[0], ids[2]];
    let expected = sqlx::query(sql)
        .bind(excluded.clone())
        .fetch_all(&mut postgres)
        .await
        .unwrap();
    let actual = sqlx::query(sql)
        .bind(excluded)
        .fetch_all(&mut fake)
        .await
        .unwrap();
    assert_eq!(
        actual
            .iter()
            .map(|row| row.get::<Uuid, _>(0))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|row| row.get::<Uuid, _>(0))
            .collect::<Vec<_>>()
    );
    let sql = "SELECT \
        '00000000-0000-4000-8000-000000000001'::UUID = ANY(ARRAY[]::UUID[]), \
        '00000000-0000-4000-8000-000000000001'::UUID <> ALL(ARRAY[]::UUID[]), \
        '00000000-0000-4000-8000-000000000001'::UUID = ANY(ARRAY['00000000-0000-4000-8000-000000000002'::UUID, NULL]), \
        '00000000-0000-4000-8000-000000000001'::UUID <> ALL(ARRAY['00000000-0000-4000-8000-000000000002'::UUID, NULL]), \
        '00000000-0000-4000-8000-000000000001'::UUID = ANY(NULL::UUID[])";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    for index in 0..5 {
        assert_eq!(
            actual.get::<Option<bool>, _>(index),
            expected.get::<Option<bool>, _>(index),
            "column {index}"
        );
    }
    for sql in [
        "SELECT '00000000-0000-4000-8000-000000000001'::UUID = ALL($1)",
        "SELECT '00000000-0000-4000-8000-000000000001'::UUID <> ANY($1)",
        "SELECT '00000000-0000-4000-8000-000000000001'::UUID > ANY($1)",
        "SELECT 1::INTEGER = ANY($1)",
        "SELECT 1::INTEGER = ANY((($1)))",
        "SELECT '00000000-0000-4000-8000-000000000001'::UUID = ALL((($1)))",
    ] {
        let expected = postgres.prepare(sql).await.unwrap();
        let actual = fake.prepare(sql).await.unwrap();
        assert_eq!(
            actual.parameters().unwrap().left().unwrap()[0]
                .base
                .unwrap()
                .map_to_oid(),
            expected.parameters().unwrap().left().unwrap()[0]
                .oid()
                .unwrap()
                .0,
            "{sql}"
        );
    }
    let sql = "SELECT $1 = ANY($2)";
    let expected = postgres.prepare(sql).await.unwrap();
    let actual = fake.prepare(sql).await.unwrap();
    assert_eq!(
        actual
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|data_type| data_type.base.unwrap().map_to_oid())
            .collect::<Vec<_>>(),
        expected
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|data_type| data_type.oid().unwrap().0)
            .collect::<Vec<_>>(),
        "{sql}"
    );
}

#[tokio::test]
async fn matches_general_array_operators_functions_assignment_and_unnest() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());

    let sql = "SELECT \
        (ARRAY[1, 2] || ARRAY[3])::text, \
        (0 || ARRAY[1, 2])::text, \
        (ARRAY[1, 2] || 3)::text, \
        ARRAY[1, 1] @> ARRAY[1], \
        ARRAY[1] <@ ARRAY[1, 2], \
        ARRAY[1, 2] && ARRAY[2, 3], \
        ARRAY[1, NULL] = ARRAY[1, NULL], \
        2 = ANY(ARRAY[1, 2, NULL]), \
        3 <> ALL(ARRAY[1, 2])";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    for index in 0..3 {
        assert_eq!(
            actual.get::<String, _>(index),
            expected.get::<String, _>(index),
            "column {index}: {sql}"
        );
    }
    for index in 3..9 {
        assert_eq!(
            actual.get::<Option<bool>, _>(index),
            expected.get::<Option<bool>, _>(index),
            "column {index}: {sql}"
        );
    }
    let sql = "SELECT \
        ARRAY[NULL::integer] @> ARRAY[NULL::integer], \
        ARRAY[NULL::integer] && ARRAY[NULL::integer], \
        (NULL::integer[] || ARRAY[1])::text, \
        (ARRAY[1] || NULL::integer[])::text, \
        array_append(NULL::integer[], 1)::text, \
        array_prepend(1, NULL::integer[])::text, \
        array_remove(ARRAY[1, NULL, 2], NULL)::text, \
        (ARRAY['abcd']::varchar(3)[])::text, \
        array_append(ARRAY[1], 2::bigint)::text, \
        array_position(ARRAY[1, 2], 2::bigint)";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    for index in 0..2 {
        assert_eq!(
            actual.get::<bool, _>(index),
            expected.get::<bool, _>(index),
            "column {index}: {sql}"
        );
    }
    for index in 2..9 {
        assert_eq!(
            actual.get::<String, _>(index),
            expected.get::<String, _>(index),
            "column {index}: {sql}"
        );
    }
    assert_eq!(
        actual.get::<Option<i32>, _>(9),
        expected.get::<Option<i32>, _>(9),
        "{sql}"
    );

    let sql = "SELECT \
        array_length(ARRAY[10, 20], 1), cardinality(ARRAY[]::int[]), \
        array_lower(ARRAY[10, 20], 1), array_upper(ARRAY[10, 20], 1), \
        array_append(ARRAY[10], 20)::text, array_prepend(5, ARRAY[10])::text, \
        array_cat(ARRAY[1], ARRAY[2, 3])::text, \
        array_position(ARRAY[1, NULL, 2], NULL), \
        array_positions(ARRAY[1, 2, 1], 1)::text, \
        array_remove(ARRAY[1, 2, 1], 1)::text, \
        array_replace(ARRAY[1, 2, 1], 1, 9)::text";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    for index in 0..4 {
        assert_eq!(
            actual.get::<Option<i32>, _>(index),
            expected.get::<Option<i32>, _>(index),
            "column {index}: {sql}"
        );
    }
    for index in 4..7 {
        assert_eq!(
            actual.get::<String, _>(index),
            expected.get::<String, _>(index),
            "column {index}: {sql}"
        );
    }
    assert_eq!(
        actual.get::<Option<i32>, _>(7),
        expected.get::<Option<i32>, _>(7),
        "{sql}"
    );
    for index in 8..11 {
        assert_eq!(
            actual.get::<String, _>(index),
            expected.get::<String, _>(index),
            "column {index}: {sql}"
        );
    }

    let create = "CREATE TABLE general_array_expansion (id integer primary key, values integer[])";
    postgres
        .execute("DROP TABLE IF EXISTS general_array_expansion")
        .await
        .unwrap();
    postgres.execute(create).await.unwrap();
    fake.execute(create).await.unwrap();
    for sql in [
        "INSERT INTO general_array_expansion VALUES (1, ARRAY[10, 20]), (2, ARRAY[]::integer[]), (3, NULL)",
        "UPDATE general_array_expansion SET values[2] = 25 WHERE id = 1",
        "UPDATE general_array_expansion SET values[3] = 30 WHERE id = 1",
    ] {
        postgres.execute(sql).await.unwrap();
        fake.execute(sql).await.unwrap();
    }
    let sql = "SELECT values::text FROM general_array_expansion WHERE id = 1";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    assert_eq!(actual.get::<String, _>(0), expected.get::<String, _>(0));
    let sql = "UPDATE general_array_expansion SET values[$1] = $2 WHERE id = $3";
    let expected_statement = postgres.prepare(sql).await.unwrap();
    let actual_statement = fake.prepare(sql).await.unwrap();
    assert_eq!(
        actual_statement
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|data_type| data_type.base.unwrap().map_to_oid())
            .collect::<Vec<_>>(),
        expected_statement
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|data_type| data_type.oid().unwrap().0)
            .collect::<Vec<_>>()
    );
    sqlx::query(sql)
        .bind(1_i32)
        .bind(11_i32)
        .bind(1_i32)
        .execute(&mut postgres)
        .await
        .unwrap();
    sqlx::query(sql)
        .bind(1_i32)
        .bind(11_i32)
        .bind(1_i32)
        .execute(&mut fake)
        .await
        .unwrap();
    let sql = "SELECT values::text FROM general_array_expansion WHERE id = 1";
    let expected = sqlx::query(sql).fetch_one(&mut postgres).await.unwrap();
    let actual = sqlx::query(sql).fetch_one(&mut fake).await.unwrap();
    assert_eq!(actual.get::<String, _>(0), expected.get::<String, _>(0));
    for sql in [
        "SELECT t.id, u.value, u.position FROM general_array_expansion AS t \
         CROSS JOIN LATERAL unnest(t.values) WITH ORDINALITY AS u(value, position) \
         ORDER BY t.id, u.position",
        "SELECT t.id, u.value FROM general_array_expansion AS t \
         LEFT JOIN LATERAL unnest(t.values) AS u(value) ON true ORDER BY t.id, u.value",
        "SELECT t.id, u.value FROM general_array_expansion AS t, unnest(t.values) AS u(value) \
         ORDER BY t.id, u.value",
    ] {
        let expected = sqlx::query(sql).fetch_all(&mut postgres).await.unwrap();
        let actual = sqlx::query(sql).fetch_all(&mut fake).await.unwrap();
        assert_eq!(actual.len(), expected.len(), "{sql}");
        for (actual, expected) in actual.iter().zip(expected) {
            for index in 0..actual.len() {
                if index == 2 {
                    assert_eq!(
                        actual.get::<Option<i64>, _>(index),
                        expected.get::<Option<i64>, _>(index),
                        "{sql}"
                    );
                } else {
                    assert_eq!(
                        actual.get::<Option<i32>, _>(index),
                        expected.get::<Option<i32>, _>(index),
                        "{sql}"
                    );
                }
            }
        }
    }
    for sql in [
        "SELECT array_position(ARRAY[1, 2], 1, 0)",
        "UPDATE general_array_expansion SET values[NULL] = 1 WHERE id = 1",
    ] {
        let expected = postgres
            .execute(sql)
            .await
            .map(|_| ())
            .map_err(get_sqlstate);
        let actual = fake.execute(sql).await.map(|_| ()).map_err(get_sqlstate);
        assert_eq!(actual, expected, "{sql}");
    }
}
