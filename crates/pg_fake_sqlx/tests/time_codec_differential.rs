#![cfg(feature = "time")]

use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Executor, Row, Statement, TypeInfo};
use sqlx_postgres::PgConnection;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

mod common;

use common::start_postgres_server;

#[tokio::test]
async fn matches_postgres_offset_datetime_codec() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let offset = UtcOffset::from_hms(5, 30, 0).unwrap();
    let values = [
        OffsetDateTime::from_unix_timestamp_nanos(1_704_164_645_123_456_789)
            .unwrap()
            .to_offset(offset),
        OffsetDateTime::from_unix_timestamp_nanos(-1_500_000_001).unwrap(),
        OffsetDateTime::from_unix_timestamp_nanos(1_704_164_645_123_456_000).unwrap(),
        OffsetDateTime::from_unix_timestamp_nanos(-1_234_000).unwrap(),
        OffsetDateTime::from_unix_timestamp_nanos(1_234_567).unwrap(),
        OffsetDateTime::from_unix_timestamp_nanos(-1_234_567).unwrap(),
    ];

    for value in values {
        let expected = sqlx::query("SELECT $1::timestamptz")
            .bind(value)
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let actual = sqlx::query("SELECT $1::timestamptz")
            .bind(value)
            .fetch_one(&mut fake)
            .await
            .unwrap();
        let expected = expected.get::<OffsetDateTime, _>(0);
        let actual = actual.get::<OffsetDateTime, _>(0);
        assert_eq!(actual, expected);
        assert_eq!(actual.offset(), UtcOffset::UTC);
    }
}

#[tokio::test]
async fn matches_nullable_storage_and_prepared_metadata() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let sql = "CREATE TABLE time_codec_values (id INTEGER PRIMARY KEY, required TIMESTAMPTZ NOT NULL, optional TIMESTAMPTZ)";
    postgres
        .execute("DROP TABLE IF EXISTS time_codec_values")
        .await
        .unwrap();
    postgres.execute(sql).await.unwrap();
    fake.execute(sql).await.unwrap();
    let negative = OffsetDateTime::from_unix_timestamp_nanos(-1_234_000).unwrap();
    let positive = OffsetDateTime::from_unix_timestamp_nanos(1_704_164_645_123_456_000)
        .unwrap()
        .to_offset(UtcOffset::from_hms(-7, 0, 0).unwrap());

    sqlx::query("INSERT INTO time_codec_values VALUES ($1, $2, $3)")
        .bind(1_i32)
        .bind(negative)
        .bind(None::<OffsetDateTime>)
        .execute(&mut postgres)
        .await
        .unwrap();
    sqlx::query("INSERT INTO time_codec_values VALUES ($1, $2, $3)")
        .bind(1_i32)
        .bind(negative)
        .bind(None::<OffsetDateTime>)
        .execute(&mut fake)
        .await
        .unwrap();
    sqlx::query("INSERT INTO time_codec_values VALUES ($1, $2, $3)")
        .bind(2_i32)
        .bind(positive)
        .bind(Some(positive))
        .execute(&mut postgres)
        .await
        .unwrap();
    let expected_error = sqlx::query("INSERT INTO time_codec_values VALUES ($1, $2, $3)")
        .bind(3_i32)
        .bind(None::<OffsetDateTime>)
        .bind(negative)
        .execute(&mut postgres)
        .await
        .unwrap_err();
    let actual_error = sqlx::query("INSERT INTO time_codec_values VALUES ($1, $2, $3)")
        .bind(3_i32)
        .bind(None::<OffsetDateTime>)
        .bind(negative)
        .execute(&mut fake)
        .await
        .unwrap_err();
    assert_eq!(
        actual_error.as_database_error().unwrap().code(),
        expected_error.as_database_error().unwrap().code()
    );
    sqlx::query("INSERT INTO time_codec_values VALUES ($1, $2, $3)")
        .bind(2_i32)
        .bind(positive)
        .bind(Some(positive))
        .execute(&mut fake)
        .await
        .unwrap();

    let expected = sqlx::query("SELECT required, optional FROM time_codec_values ORDER BY id")
        .fetch_all(&mut postgres)
        .await
        .unwrap();
    let actual = sqlx::query("SELECT required, optional FROM time_codec_values ORDER BY id")
        .fetch_all(&mut fake)
        .await
        .unwrap();
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(
            actual.get::<OffsetDateTime, _>(0),
            expected.get::<OffsetDateTime, _>(0)
        );
        assert_eq!(
            actual.get::<Option<OffsetDateTime>, _>(1),
            expected.get::<Option<OffsetDateTime>, _>(1)
        );
    }
    assert_eq!(actual[0].get::<Option<OffsetDateTime>, _>(1), None);
    assert!(actual[1].get::<Option<OffsetDateTime>, _>(1).is_some());
    assert_eq!(actual[0].columns()[0].type_info().name(), "TIMESTAMPTZ");

    let expected = postgres
        .prepare("SELECT $1::timestamptz AS observed_at")
        .await
        .unwrap();
    let actual = fake
        .prepare("SELECT $1::timestamptz AS observed_at")
        .await
        .unwrap();
    assert_eq!(
        actual.columns()[0].type_info().name(),
        expected.columns()[0].type_info().name()
    );
    assert_eq!(
        actual.parameters().unwrap().left().unwrap()[0].name(),
        "TIMESTAMPTZ"
    );
    postgres
        .execute("DROP TABLE time_codec_values")
        .await
        .unwrap();
}

#[tokio::test]
async fn rejects_invalid_and_out_of_range_values_like_postgres() {
    let server = start_postgres_server();
    let mut postgres = PgConnection::connect(&server.url).await.unwrap();
    let mut fake = PgFakeConnection::new(Db::create());

    let out_of_range = PrimitiveDateTime::MIN.assume_utc();
    let expected = sqlx::query("SELECT $1::timestamptz")
        .bind(out_of_range)
        .fetch_one(&mut postgres)
        .await;
    let actual = sqlx::query("SELECT $1::timestamptz")
        .bind(out_of_range)
        .fetch_one(&mut fake)
        .await;
    let expected = expected.unwrap_err();
    let actual = actual.unwrap_err();
    assert_eq!(
        actual.as_database_error().unwrap().code(),
        expected.as_database_error().unwrap().code()
    );

    let expected = sqlx::query("SELECT 'not a timestamp'::timestamptz")
        .fetch_one(&mut postgres)
        .await
        .unwrap_err();
    let actual = sqlx::query("SELECT 'not a timestamp'::timestamptz")
        .fetch_one(&mut fake)
        .await
        .unwrap_err();
    assert_eq!(
        actual.as_database_error().unwrap().code(),
        expected.as_database_error().unwrap().code()
    );
}
