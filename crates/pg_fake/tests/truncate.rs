use pg_fake::{Db, error::SqlState, value::Value};

#[test]
fn truncates_transactionally_and_handles_foreign_keys() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY); \
             CREATE TABLE children (id INTEGER REFERENCES parents(id)); \
             INSERT INTO parents VALUES (1); \
             INSERT INTO children VALUES (1)",
        )
        .unwrap();

    assert_eq!(
        session.execute("TRUNCATE parents").unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    session
        .execute("BEGIN; TRUNCATE parents CASCADE; ROLLBACK")
        .unwrap();
    assert_eq!(
        session.query("SELECT id FROM parents", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        session.query("SELECT id FROM children", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );

    session
        .execute("TRUNCATE parents, children RESTRICT")
        .unwrap();
    assert!(
        session
            .query("SELECT id FROM parents", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        session
            .query("SELECT id FROM children", &[])
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn restarts_owned_sequences_transactionally() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE numbered (id SERIAL PRIMARY KEY); INSERT INTO numbered DEFAULT VALUES",
        )
        .unwrap();
    session
        .execute("BEGIN; TRUNCATE numbered RESTART IDENTITY; INSERT INTO numbered DEFAULT VALUES; ROLLBACK")
        .unwrap();
    assert_eq!(
        session
            .query("INSERT INTO numbered DEFAULT VALUES RETURNING id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
    session
        .execute("TRUNCATE numbered RESTART IDENTITY")
        .unwrap();
    assert_eq!(
        session
            .query("INSERT INTO numbered DEFAULT VALUES RETURNING id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
fn holds_an_access_exclusive_lock_until_transaction_end() {
    let db = Db::create();
    let mut holder = db.create_session();
    let mut contender = db.create_session();
    holder
        .execute(
            "CREATE TABLE truncate_locked (id INTEGER); INSERT INTO truncate_locked VALUES (1)",
        )
        .unwrap();
    holder.execute("BEGIN; TRUNCATE truncate_locked").unwrap();
    contender.execute("SET lock_timeout = '10ms'").unwrap();
    assert_eq!(
        contender
            .query("SELECT id FROM truncate_locked", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::LockNotAvailable
    );
    holder.execute("ROLLBACK").unwrap();
    assert_eq!(
        contender
            .query("SELECT id FROM truncate_locked", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
fn truncation_is_relation_wide_and_not_mvcc_safe() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    writer
        .execute("CREATE TABLE truncate_epoch (id INTEGER); INSERT INTO truncate_epoch VALUES (1)")
        .unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT 1")
        .unwrap();
    writer.execute("TRUNCATE truncate_epoch").unwrap();
    assert!(
        reader
            .query("SELECT id FROM truncate_epoch", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    reader.execute("ROLLBACK").unwrap();

    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT 1")
        .unwrap();
    writer
        .execute("INSERT INTO truncate_epoch VALUES (2)")
        .unwrap();
    reader.execute("TRUNCATE truncate_epoch").unwrap();
    reader.execute("COMMIT").unwrap();
    assert!(
        writer
            .query("SELECT id FROM truncate_epoch", &[])
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn restores_pre_truncate_storage_after_repeated_rollback() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE truncate_rollback (id INTEGER PRIMARY KEY, value INTEGER); \
             INSERT INTO truncate_rollback VALUES (1, 10), (2, 20)",
        )
        .unwrap();
    session
        .execute(
            "BEGIN; \
             UPDATE truncate_rollback SET value = 30 WHERE id = 1; \
             DELETE FROM truncate_rollback WHERE id = 2; \
             TRUNCATE truncate_rollback; \
             INSERT INTO truncate_rollback VALUES (3, 40); \
             TRUNCATE truncate_rollback; \
             INSERT INTO truncate_rollback VALUES (4, 50); \
             ROLLBACK",
        )
        .unwrap();
    assert_eq!(
        session
            .query("SELECT id, value FROM truncate_rollback ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(1), Value::Int4(10)],
            vec![Value::Int4(2), Value::Int4(20)],
        ]
    );
    session
        .execute("INSERT INTO truncate_rollback VALUES (3, 30)")
        .unwrap();
}
