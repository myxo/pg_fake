use super::*;
use crate::{error::SqlState, value::Value};
use chaos_theory::{check, make::int_in};

#[test]
fn snapshot_committed_rows_catalog_sequences_and_settings() {
    check(|src| {
        let count = src.any_of("rows", int_in(1..=10));
        let allocated = src.any_of("allocated", int_in(1..=10));
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_millis(37))
            .build();
        let mut source = db.create_session();
        source
            .execute("CREATE TABLE items(id SERIAL PRIMARY KEY, value INT)")
            .unwrap();
        for value in 0..count {
            source
                .execute(&format!("INSERT INTO items(value) VALUES({value})"))
                .unwrap();
        }
        let expected = source
            .query("SELECT * FROM items ORDER BY id", &[])
            .unwrap();
        source.execute("CREATE TEMP TABLE scratch(id SERIAL); SET application_name='source'; SET search_path=pg_temp, public; SET lock_timeout='2s'").unwrap();
        source.execute("BEGIN").unwrap();
        for _ in 0..allocated {
            source.query("SELECT nextval('items_id_seq')", &[]).unwrap();
        }
        source.execute("UPDATE items SET value=-1; DELETE FROM items WHERE id=1; ALTER TABLE items ADD COLUMN extra TEXT; CREATE TABLE pending(id INT); SAVEPOINT s; INSERT INTO items(value) VALUES(99)").unwrap();
        let fork = db.snapshot();
        let mut session = fork.create_session();
        assert_eq!(
            session
                .query("SELECT * FROM items ORDER BY id", &[])
                .unwrap(),
            expected
        );
        assert_eq!(
            session
                .query("SELECT * FROM pending", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session
                .query("SELECT * FROM scratch", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session.query("SHOW lock_timeout", &[]).unwrap().rows,
            vec![vec![Value::Text("37ms".into())]]
        );
        assert_eq!(
            session.query("SHOW application_name", &[]).unwrap().rows,
            vec![vec![Value::Text("".into())]]
        );
        let next = source.query("SELECT nextval('items_id_seq')", &[]).unwrap();
        assert_eq!(
            session
                .query("SELECT nextval('items_id_seq')", &[])
                .unwrap(),
            next
        );
        session
            .query("SELECT nextval('items_id_seq')", &[])
            .unwrap();
        source.execute("COMMIT; DROP TABLE items").unwrap();
        assert_eq!(
            session
                .query("SELECT * FROM items ORDER BY id", &[])
                .unwrap(),
            expected
        );
        session
            .execute("UPDATE items SET value=42 WHERE id=1; INSERT INTO items(value) VALUES(123)")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT count(*) FROM items", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(i64::from(count + 1))]]
        );
        assert_eq!(
            db.create_session()
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    });
}

#[test]
fn snapshot_excludes_uncommitted_truncation_and_dropped_catalog_objects() {
    let db = Db::create();
    let mut source = db.create_session();
    source.execute("CREATE TABLE items(id SERIAL PRIMARY KEY); INSERT INTO items DEFAULT VALUES; CREATE TABLE kept(id INT)").unwrap();
    source.execute("BEGIN; TRUNCATE items RESTART IDENTITY; INSERT INTO items DEFAULT VALUES; DROP TABLE kept").unwrap();
    let snapshot = db.snapshot();
    let mut fork = snapshot.create_session();
    assert_eq!(
        fork.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        fork.query("SELECT nextval('items_id_seq')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    fork.query("SELECT * FROM kept", &[]).unwrap();
    source.execute("COMMIT").unwrap();
    assert_eq!(
        source
            .query("SELECT nextval('items_id_seq')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    fork.execute("INSERT INTO kept VALUES(9)").unwrap();
    assert_eq!(
        source
            .query("SELECT * FROM kept", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
fn snapshot_detaches_locks_clock_rng_and_strict_policy() {
    let db = Db::create_builder()
        .set_mock_time_enabled(true)
        .set_random_seed(42)
        .set_strict_mode_enabled(true)
        .build();
    let mut source = db.create_session();
    source
        .execute("CREATE TABLE items(id INT); INSERT INTO items VALUES(1)")
        .unwrap();
    source
        .execute("BEGIN; LOCK TABLE items IN ACCESS EXCLUSIVE MODE; UPDATE items SET id=2")
        .unwrap();
    source.query("SELECT pg_advisory_lock(123)", &[]).unwrap();
    let fork = db.snapshot();
    let mut session = fork.create_session();
    session.execute("UPDATE items SET id=3").unwrap();
    assert_eq!(
        session
            .query("SELECT pg_try_advisory_xact_lock(123)", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        db.create_session()
            .query("SELECT pg_try_advisory_xact_lock(123)", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Bool(false)]]
    );
    assert_eq!(
        session
            .execute("SET enable_seqscan=off")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    let frozen = session.query("SELECT clock_timestamp()", &[]).unwrap();
    db.advance_time(chrono::Duration::hours(1)).unwrap();
    assert_eq!(
        session.query("SELECT clock_timestamp()", &[]).unwrap(),
        frozen
    );
    assert_ne!(
        source.query("SELECT clock_timestamp()", &[]).unwrap(),
        frozen
    );
    assert_eq!(
        session.query("SELECT gen_random_uuid()", &[]).unwrap(),
        source.query("SELECT gen_random_uuid()", &[]).unwrap()
    );
    source.execute("ROLLBACK").unwrap();
    assert_eq!(
        source.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        session.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(3)]]
    );
}
