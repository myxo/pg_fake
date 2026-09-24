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
fn matches_write_skew_and_unique_gap_outcomes() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres_first = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut postgres_second = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake_first = PgFakeConnection::new(db.clone());
    let mut fake_second = PgFakeConnection::new(db);
    for (use_first, sql) in [
        (true, "CREATE TABLE ssi_rows(id INT PRIMARY KEY, value INT)"),
        (true, "INSERT INTO ssi_rows VALUES (1, 0), (2, 0)"),
        (true, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (false, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (true, "SELECT value FROM ssi_rows WHERE id = 2"),
        (false, "SELECT value FROM ssi_rows WHERE id = 1"),
        (true, "UPDATE ssi_rows SET value = 1 WHERE id = 1"),
        (false, "UPDATE ssi_rows SET value = 1 WHERE id = 2"),
        (true, "COMMIT"),
        (false, "COMMIT"),
        (true, "SELECT value FROM ssi_rows ORDER BY id"),
        (true, "CREATE TABLE ssi_keys(id INT PRIMARY KEY)"),
        (true, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (false, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (true, "SELECT id FROM ssi_keys WHERE id = 2"),
        (false, "SELECT id FROM ssi_keys WHERE id = 1"),
        (true, "INSERT INTO ssi_keys VALUES (1)"),
        (false, "INSERT INTO ssi_keys VALUES (2)"),
        (true, "COMMIT"),
        (false, "COMMIT"),
        (true, "SELECT id FROM ssi_keys ORDER BY id"),
    ] {
        let (postgres, fake) = if use_first {
            (&mut postgres_first, &mut fake_first)
        } else {
            (&mut postgres_second, &mut fake_second)
        };
        let order = if sql.starts_with("SELECT id FROM ssi_keys WHERE") {
            RowOrder::Unordered
        } else {
            RowOrder::Ordered
        };
        if sql == "CREATE TABLE ssi_rows(id INT PRIMARY KEY, value INT)" {
            assert_statement(&runtime, postgres, fake, sql, order);
        } else {
            assert_statement_allow_error(&runtime, postgres, fake, sql, order);
        }
    }
}

#[test]
fn matches_read_only_anomaly_outcomes_for_both_writer_isolations() {
    for serializable_writer in [false, true] {
        let server = start_isolated_postgres_server();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut postgres = [
            runtime
                .block_on(PgConnection::connect(&server.url))
                .unwrap(),
            runtime
                .block_on(PgConnection::connect(&server.url))
                .unwrap(),
            runtime
                .block_on(PgConnection::connect(&server.url))
                .unwrap(),
        ];
        let db = Db::create();
        let mut fake = [
            PgFakeConnection::new(db.clone()),
            PgFakeConnection::new(db.clone()),
            PgFakeConnection::new(db),
        ];
        for (session, sql) in [
            (
                0,
                "CREATE TABLE readonly_anomaly(id INT PRIMARY KEY, value INT)",
            ),
            (0, "INSERT INTO readonly_anomaly VALUES (1, 0), (2, 0)"),
            (1, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
            (1, "SELECT value FROM readonly_anomaly WHERE id = 2"),
            (2, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
            (2, "UPDATE readonly_anomaly SET value = 1 WHERE id = 2"),
            (2, "COMMIT"),
            (0, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
            (0, "SELECT value FROM readonly_anomaly WHERE id = 2"),
            (0, "SAVEPOINT s"),
            (1, "UPDATE readonly_anomaly SET value = 1 WHERE id = 1"),
            (1, "COMMIT"),
            (0, "SELECT value FROM readonly_anomaly WHERE id = 1"),
            (0, "ROLLBACK TO SAVEPOINT s"),
            (0, "SELECT 1"),
            (0, "COMMIT"),
        ] {
            if !serializable_writer && session == 2 && (sql.starts_with("BEGIN") || sql == "COMMIT")
            {
                continue;
            }
            assert_statement_allow_error(
                &runtime,
                &mut postgres[session],
                &mut fake[session],
                sql,
                RowOrder::Ordered,
            );
        }
    }
}

#[test]
fn matches_on_conflict_and_deferred_foreign_key_outcomes() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = [
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    ];
    let db = Db::create();
    let mut fake = [PgFakeConnection::new(db.clone()), PgFakeConnection::new(db)];
    for (session, sql) in [
        (
            0,
            "CREATE TABLE ssi_conflict(id INT PRIMARY KEY, value INT)",
        ),
        (0, "INSERT INTO ssi_conflict VALUES (1, 0)"),
        (0, "CREATE TABLE ssi_guard(id INT PRIMARY KEY, value INT)"),
        (0, "INSERT INTO ssi_guard VALUES (1, 0)"),
        (0, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (1, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (
            0,
            "INSERT INTO ssi_conflict VALUES (1, 9) ON CONFLICT (id) DO NOTHING",
        ),
        (1, "SELECT value FROM ssi_guard WHERE id = 1"),
        (0, "UPDATE ssi_guard SET value = 1 WHERE id = 1"),
        (0, "COMMIT"),
        (1, "UPDATE ssi_conflict SET value = 2 WHERE id = 1"),
        (1, "ROLLBACK"),
        (0, "CREATE TABLE ssi_parent(id INT PRIMARY KEY)"),
        (
            0,
            "CREATE TABLE ssi_child(id INT PRIMARY KEY, parent_id INT REFERENCES ssi_parent DEFERRABLE INITIALLY DEFERRED)",
        ),
        (0, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (0, "SELECT id FROM ssi_parent"),
        (0, "INSERT INTO ssi_child VALUES (1, 7)"),
        (1, "INSERT INTO ssi_parent VALUES (7)"),
        (0, "COMMIT"),
    ] {
        assert_statement_allow_error(
            &runtime,
            &mut postgres[session],
            &mut fake[session],
            sql,
            RowOrder::Ordered,
        );
    }
}

#[test]
fn matches_savepoint_retry_after_write_skew_failure() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = [
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    ];
    let db = Db::create();
    let mut fake = [PgFakeConnection::new(db.clone()), PgFakeConnection::new(db)];
    for (session, sql) in [
        (0, "CREATE TABLE retry_skew(id INT PRIMARY KEY, value INT)"),
        (0, "INSERT INTO retry_skew VALUES (1, 0), (2, 0)"),
        (0, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (1, "BEGIN ISOLATION LEVEL SERIALIZABLE"),
        (0, "SELECT value FROM retry_skew WHERE id = 2"),
        (1, "SELECT value FROM retry_skew WHERE id = 1"),
        (0, "UPDATE retry_skew SET value = 1 WHERE id = 1"),
        (0, "COMMIT"),
        (1, "SAVEPOINT s"),
        (1, "UPDATE retry_skew SET value = 1 WHERE id = 2"),
        (1, "ROLLBACK TO SAVEPOINT s"),
        (1, "UPDATE retry_skew SET value = 1 WHERE id = 2"),
        (1, "ROLLBACK"),
    ] {
        assert_statement_allow_error(
            &runtime,
            &mut postgres[session],
            &mut fake[session],
            sql,
            RowOrder::Ordered,
        );
    }
}
