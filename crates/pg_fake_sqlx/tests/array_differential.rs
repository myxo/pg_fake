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
        "SELECT $1 = ANY($2)",
        "SELECT 1::INTEGER = ANY((($1)))",
        "SELECT '00000000-0000-4000-8000-000000000001'::UUID = ALL((($1)))",
    ] {
        assert_eq!(
            get_sqlstate(fake.prepare(sql).await.unwrap_err()),
            "0A000",
            "{sql}"
        );
    }
}
