use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn compares_guarded_indexed_mutations() {
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
        "CREATE TABLE guarded_mutation (id INTEGER PRIMARY KEY, group_id INTEGER, marker INTEGER, deleted_at INTEGER)",
        "INSERT INTO guarded_mutation VALUES (1, 10, 0, NULL), (2, 20, 0, NULL), (3, 30, 0, 1)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    for predicate in [
        "id = 1 AND group_id = 10",
        "group_id = 10 AND (id = 1 AND deleted_at IS NULL)",
        "(1 = id) AND group_id = 10",
        "id = 1 AND group_id = 20",
        "id = 3 AND deleted_at IS NULL",
        "id = 1 AND id = 2",
        "id = NULL AND group_id = 10",
        "id = 99 AND group_id = 10",
        "(id = 1 OR id = 2) AND group_id = 10",
    ] {
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "BEGIN",
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!(
                "UPDATE guarded_mutation SET marker = marker + 1 WHERE {predicate} RETURNING id, marker"
            ),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "SELECT id, marker FROM guarded_mutation ORDER BY id",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "ROLLBACK",
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "BEGIN",
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!("DELETE FROM guarded_mutation WHERE {predicate} RETURNING id"),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "ROLLBACK",
            RowOrder::Unordered,
        );
    }
    for sql in [
        "UPDATE guarded_mutation SET marker = 1 WHERE id = 1 AND group_id = 20",
        "DELETE FROM guarded_mutation WHERE id = 99 AND group_id = 10",
        "UPDATE guarded_mutation SET marker = 1 WHERE id = 1 AND 1 / 0 = 0",
        "UPDATE guarded_mutation SET marker = 1 WHERE id = 99 AND 1 / 0 = 0",
        "UPDATE guarded_mutation SET marker = 1 WHERE group_id = 20 AND id = 'invalid'",
        "UPDATE guarded_mutation SET marker = 1 WHERE FALSE AND id = 'invalid'",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    runtime.block_on(async {
        let sql = "UPDATE guarded_mutation SET marker = marker + 1 WHERE group_id = $2 AND (id = $1 AND deleted_at IS NULL) RETURNING id, marker";
        for (id, group_id) in [(1_i32, 10_i32), (1, 20), (3, 30), (99, 10)] {
            let expected: Vec<(i32, i32)> = sqlx::query_as(sql)
                .bind(id)
                .bind(group_id)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32, i32)> = sqlx::query_as(sql)
                .bind(id)
                .bind(group_id)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected, "id={id}, group_id={group_id}");
        }
    });
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id, marker FROM guarded_mutation ORDER BY id",
        RowOrder::Ordered,
    );
}
