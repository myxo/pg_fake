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
