use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Executor, Statement, TypeInfo};
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn matches_text_hash_values_and_signatures() {
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
        "SELECT hashtext(''), hashtext('abc'), hashtext('Привет'), hashtext('🙂é')",
        "SELECT hashtextextended('',0), hashtextextended('abc',0), hashtextextended('Привет',1)",
        "SELECT hashtextextended('seed-min','-9223372036854775808'::BIGINT), hashtextextended('seed-max',9223372036854775807::BIGINT)",
        "SELECT hashtext(NULL), hashtextextended(NULL,0), hashtextextended('value',NULL)",
        "SELECT hashtext('value'::VARCHAR), hashtext('value'::CHAR(8)), hashtextextended('value'::VARCHAR,1::SMALLINT)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    let long = "Ab🙂Ж".repeat(2_048);
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        &format!("SELECT hashtext('{long}'), hashtextextended('{long}',-1)"),
        RowOrder::Ordered,
    );
    for sql in [
        "SELECT hashtext()",
        "SELECT hashtext('a','b')",
        "SELECT hashtext(1)",
        "SELECT hashtext(TRUE)",
        "SELECT hashtextextended('a')",
        "SELECT hashtextextended('a',1,2)",
        "SELECT hashtextextended(1,2)",
        "SELECT hashtextextended('a',1.5)",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn matches_prepared_hash_metadata_and_parameters() {
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
        "SELECT hashtext($1::text)",
        "SELECT hashtextextended($1, 0)",
        "SELECT hashtextextended($1, $2)",
        "SELECT hashtext($1), $1::INTEGER AS cast_value",
        "SELECT pg_advisory_xact_lock(hashtext($1::text))",
        "SELECT pg_try_advisory_xact_lock(hashtext($1::text))",
        "SELECT pg_advisory_xact_lock($1, hashtext($2))",
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
    ] {
        let expected = runtime.block_on(postgres.prepare(sql)).unwrap();
        let actual = runtime
            .block_on(fake.prepare(sql))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
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
                .collect::<Vec<_>>(),
            "{sql}"
        );
        for (expected, actual) in expected.columns().iter().zip(actual.columns()) {
            assert_eq!(
                actual.type_info().base.unwrap().map_to_oid(),
                expected.type_info().oid().unwrap().0,
                "{sql}"
            );
        }
        assert_eq!(
            actual
                .parameters()
                .unwrap()
                .left()
                .unwrap()
                .iter()
                .map(TypeInfo::name)
                .collect::<Vec<_>>(),
            expected
                .parameters()
                .unwrap()
                .left()
                .unwrap()
                .iter()
                .map(TypeInfo::name)
                .collect::<Vec<_>>(),
            "{sql}"
        );
        for (expected, actual) in expected
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .zip(actual.parameters().unwrap().left().unwrap())
        {
            assert_eq!(
                actual.base.unwrap().map_to_oid(),
                expected.oid().unwrap().0,
                "{sql}"
            );
        }
    }
    for sql in [
        "SELECT hashtextextended($1,$1)",
        "SELECT $1::INTEGER, hashtext($1)",
    ] {
        let expected = runtime.block_on(postgres.prepare(sql)).unwrap_err();
        let actual = runtime.block_on(fake.prepare(sql)).unwrap_err();
        assert_eq!(
            actual.as_database_error().unwrap().code(),
            expected.as_database_error().unwrap().code(),
            "{sql}"
        );
    }

    let sql = "SELECT hashtext($1), hashtextextended($1,$2)";
    let expected: (i32, i64) = runtime
        .block_on(
            sqlx::query_as(sql)
                .bind("prepared-🙂")
                .bind(i64::MIN)
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let actual: (i32, i64) = runtime
        .block_on(
            sqlx::query_as(sql)
                .bind("prepared-🙂")
                .bind(i64::MIN)
                .fetch_one(&mut fake),
        )
        .unwrap();
    assert_eq!(actual, expected);

    let _: () = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_advisory_xact_lock(hashtext($1::text))")
                .bind("prepared-lock")
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let _: () = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_advisory_xact_lock(hashtext($1::text))")
                .bind("prepared-lock")
                .fetch_one(&mut fake),
        )
        .unwrap();
    let expected: bool = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtext($1::text))")
                .bind("prepared-try-lock")
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let actual: bool = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtext($1::text))")
                .bind("prepared-try-lock")
                .fetch_one(&mut fake),
        )
        .unwrap();
    assert_eq!(actual, expected);
    let _: () = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_advisory_xact_lock($1,hashtext($2))")
                .bind(17_i32)
                .bind("prepared-pair")
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let _: () = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_advisory_xact_lock($1,hashtext($2))")
                .bind(17_i32)
                .bind("prepared-pair")
                .fetch_one(&mut fake),
        )
        .unwrap();
    let _: () = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind("prepared-extended")
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let _: () = runtime
        .block_on(
            sqlx::query_scalar("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind("prepared-extended")
                .fetch_one(&mut fake),
        )
        .unwrap();
}

#[test]
fn composes_text_hashes_with_advisory_locks() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = (0..2)
        .map(|_| {
            runtime
                .block_on(PgConnection::connect(&server.url))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let db = Db::create();
    let mut fake = (0..2)
        .map(|_| PgFakeConnection::new(db.clone()))
        .collect::<Vec<_>>();
    for (session, sql) in [
        (0, "BEGIN"),
        (0, "SELECT pg_advisory_xact_lock(hashtext('same-text'))"),
        (1, "SELECT pg_try_advisory_xact_lock(hashtext('same-text'))"),
        (
            1,
            "SELECT pg_try_advisory_xact_lock(hashtext('different-text'))",
        ),
        (0, "COMMIT"),
        (1, "SELECT pg_try_advisory_xact_lock(hashtext('same-text'))"),
        (0, "BEGIN"),
        (
            0,
            "SELECT pg_advisory_xact_lock(hashtextextended('extended',-9223372036854775808))",
        ),
        (
            1,
            "SELECT pg_try_advisory_xact_lock(hashtextextended('extended',-9223372036854775808))",
        ),
        (
            1,
            "SELECT pg_try_advisory_xact_lock(hashtextextended('extended',9223372036854775807))",
        ),
        (0, "ROLLBACK"),
        (
            1,
            "SELECT pg_try_advisory_xact_lock(hashtextextended('extended',-9223372036854775808))",
        ),
        (0, "BEGIN"),
        (0, "SELECT pg_advisory_xact_lock(17,hashtext('pair'))"),
        (1, "SELECT pg_try_advisory_xact_lock(17,hashtext('pair'))"),
        (0, "COMMIT"),
        (1, "SELECT pg_try_advisory_xact_lock(17,hashtext('pair'))"),
    ] {
        assert_statement(
            &runtime,
            &mut postgres[session],
            &mut fake[session],
            sql,
            RowOrder::Unordered,
        );
    }
}
