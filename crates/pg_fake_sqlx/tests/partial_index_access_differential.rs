use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{RowOrder, assert_statement, start_isolated_postgres_server};

#[test]
fn compares_partial_index_reads_and_membership_changes() {
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
        "CREATE TABLE partial_diff (id INTEGER PRIMARY KEY, account_id INTEGER,
         group_id INTEGER, deleted_at INTEGER, role TEXT)",
        "CREATE UNIQUE INDEX partial_diff_pair ON partial_diff(group_id, account_id)
         WHERE deleted_at IS NULL",
        "CREATE INDEX partial_diff_account ON partial_diff(account_id, group_id)
         WHERE deleted_at IS NULL",
        "INSERT INTO partial_diff VALUES (1, 10, 20, NULL, 'active'),
         (2, 10, 20, 1, 'deleted'), (3, 10, 21, NULL, 'second'),
         (4, 11, 22, 1, 'deleted only')",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    for sql in [
        "SELECT id, role FROM partial_diff WHERE account_id = 10 AND deleted_at IS NULL ORDER BY id",
        "SELECT id, role FROM partial_diff WHERE (deleted_at IS NULL) AND (account_id = 10) ORDER BY id",
        "SELECT id, role FROM partial_diff WHERE group_id = 20 AND account_id = 10 AND deleted_at IS NULL ORDER BY id",
        "SELECT id, role FROM partial_diff WHERE account_id = 11 AND deleted_at IS NULL ORDER BY id",
        "SELECT id, role FROM partial_diff WHERE account_id = 10 ORDER BY id",
        "SELECT id, role FROM partial_diff WHERE account_id = 10 AND (deleted_at IS NULL OR role = 'deleted') ORDER BY id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    runtime.block_on(async {
        let sql = "SELECT id, role FROM partial_diff WHERE account_id = $1 AND deleted_at IS NULL ORDER BY id";
        for account in [10_i32, 11, 99, 10] {
            let expected: Vec<(i32, String)> = sqlx::query_as(sql)
                .bind(account)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32, String)> = sqlx::query_as(sql)
                .bind(account)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
        let sql = "SELECT id FROM partial_diff WHERE account_id = $1 AND deleted_at = $2 ORDER BY id";
        for deleted_at in [Some(1_i32), None, Some(2), Some(1)] {
            let expected: Vec<(i32,)> = sqlx::query_as(sql)
                .bind(10_i32)
                .bind(deleted_at)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32,)> = sqlx::query_as(sql)
                .bind(10_i32)
                .bind(deleted_at)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
    });
    let mut postgres_reader = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake_reader = PgFakeConnection::new(db);
    for sql in [
        "BEGIN ISOLATION LEVEL REPEATABLE READ",
        "SELECT id FROM partial_diff WHERE group_id = 20 AND account_id = 10 AND deleted_at IS NULL",
    ] {
        assert_statement(
            &runtime,
            &mut postgres_reader,
            &mut fake_reader,
            sql,
            RowOrder::Ordered,
        );
    }
    for sql in [
        "UPDATE partial_diff SET deleted_at = 2 WHERE id = 1",
        "UPDATE partial_diff SET deleted_at = NULL WHERE id = 2",
        "SELECT id FROM partial_diff WHERE group_id = 20 AND account_id = 10 AND deleted_at IS NULL",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement(
        &runtime,
        &mut postgres_reader,
        &mut fake_reader,
        "SELECT id FROM partial_diff WHERE group_id = 20 AND account_id = 10 AND deleted_at IS NULL",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres_reader,
        &mut fake_reader,
        "ROLLBACK",
        RowOrder::Unordered,
    );
    for sql in [
        "BEGIN",
        "SAVEPOINT before_change",
        "UPDATE partial_diff SET deleted_at = 3 WHERE id = 2",
        "ROLLBACK TO SAVEPOINT before_change",
        "COMMIT",
        "SELECT id FROM partial_diff WHERE group_id = 20 AND account_id = 10 AND deleted_at IS NULL",
        "DROP INDEX partial_diff_pair",
        "DROP INDEX partial_diff_account",
        "SELECT id FROM partial_diff WHERE group_id = 20 AND account_id = 10 AND deleted_at IS NULL",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "CREATE TABLE evolved_partial_diff (id INTEGER PRIMARY KEY, key_value INTEGER)",
        "INSERT INTO evolved_partial_diff VALUES (1, 7)",
        "ALTER TABLE evolved_partial_diff ADD COLUMN marker INTEGER",
        "UPDATE evolved_partial_diff SET marker = 1 WHERE id = 1",
        "CREATE INDEX evolved_partial_diff_key ON evolved_partial_diff(key_value)
         WHERE marker IS NOT NULL",
        "SELECT id FROM evolved_partial_diff WHERE key_value = 7 AND marker IS NOT NULL",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}
