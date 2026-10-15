use pg_fake::{Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn preserves_left_join_rows_while_selecting_indexed_sources() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE outer_left (id INTEGER PRIMARY KEY, key_value INTEGER); \
             CREATE TABLE outer_right (id INTEGER PRIMARY KEY, key_value INTEGER, active BOOLEAN); \
             CREATE INDEX outer_right_key ON outer_right(key_value); \
             INSERT INTO outer_left VALUES (1, 10), (2, 20), (3, 30), (4, NULL); \
             INSERT INTO outer_right VALUES (11, 10, TRUE), (12, 10, FALSE), (21, 20, FALSE)",
        )
        .unwrap();
    let prepared = session
        .prepare(
            "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
             ON r.key_value = l.key_value WHERE l.id = $1 ORDER BY r.id",
        )
        .unwrap();
    for (id, expected) in [
        (
            1,
            vec![
                vec![Value::Int4(1), Value::Int4(11)],
                vec![Value::Int4(1), Value::Int4(12)],
            ],
        ),
        (2, vec![vec![Value::Int4(2), Value::Int4(21)]]),
        (3, vec![vec![Value::Int4(3), Value::Null]]),
        (4, vec![vec![Value::Int4(4), Value::Null]]),
        (99, vec![]),
    ] {
        assert_eq!(
            session
                .query_prepared(&prepared, &[Value::Int4(id)])
                .unwrap()
                .rows,
            expected
        );
    }
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT l.id FROM outer_left l LEFT JOIN outer_right r \
             ON r.key_value = l.key_value WHERE l.id = 3 AND r.id IS NULL",
        ),
        vec![vec![Value::Int4(3)]]
    );
    assert!(
        query_rows(
            &mut session,
            "SELECT l.id FROM outer_left l LEFT JOIN outer_right r \
             ON r.key_value = l.key_value WHERE l.id = 2 AND r.id IS NULL",
        )
        .is_empty()
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
             ON r.key_value = l.key_value AND r.active = TRUE WHERE l.id = 2",
        ),
        vec![vec![Value::Int4(2), Value::Null]]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT l.id, r.id FROM outer_left l LEFT JOIN outer_right r \
             ON r.key_value = l.key_value WHERE l.id = 1 OR l.id = 3 ORDER BY l.id, r.id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int4(11)],
            vec![Value::Int4(1), Value::Int4(12)],
            vec![Value::Int4(3), Value::Null],
        ]
    );
}

#[test]
fn probes_outer_join_rows_from_an_old_snapshot() {
    let db = Db::create();
    let mut writer = db.create_session();
    writer
        .execute(
            "CREATE TABLE outer_snapshot_left (id INTEGER PRIMARY KEY); \
             CREATE TABLE outer_snapshot_right (id INTEGER PRIMARY KEY, left_id INTEGER); \
             CREATE INDEX outer_snapshot_right_left ON outer_snapshot_right(left_id); \
             INSERT INTO outer_snapshot_left VALUES (1); \
             INSERT INTO outer_snapshot_right VALUES (10, 1)",
        )
        .unwrap();
    let mut reader = db.create_session();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    let sql = "SELECT r.id FROM outer_snapshot_left l LEFT JOIN outer_snapshot_right r \
               ON r.left_id = l.id WHERE l.id = 1";
    assert_eq!(query_rows(&mut reader, sql), vec![vec![Value::Int4(10)]]);
    writer
        .execute("UPDATE outer_snapshot_right SET left_id = 2 WHERE id = 10")
        .unwrap();
    assert_eq!(query_rows(&mut reader, sql), vec![vec![Value::Int4(10)]]);
    reader.execute("ROLLBACK").unwrap();
    assert_eq!(query_rows(&mut reader, sql), vec![vec![Value::Null]]);
}

#[test]
fn serializable_nonunique_probe_tracks_inserted_matches() {
    let db = Db::create();
    let mut first = db.create_session();
    first
        .execute(
            "CREATE TABLE outer_ssi_left (id INTEGER PRIMARY KEY, value INTEGER); \
             CREATE TABLE outer_ssi_right (id INTEGER PRIMARY KEY, left_id INTEGER); \
             CREATE INDEX outer_ssi_right_left ON outer_ssi_right(left_id); \
             INSERT INTO outer_ssi_left VALUES (1, 0)",
        )
        .unwrap();
    let mut second = db.create_session();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    assert_eq!(
        query_rows(
            &mut first,
            "SELECT l.id, r.id FROM outer_ssi_left l LEFT JOIN outer_ssi_right r \
             ON r.left_id = l.id WHERE l.id = 1 AND r.id IS NULL",
        ),
        vec![vec![Value::Int4(1), Value::Null]]
    );
    assert_eq!(
        query_rows(&mut second, "SELECT value FROM outer_ssi_left WHERE id = 1"),
        vec![vec![Value::Int4(0)]]
    );
    first
        .execute("UPDATE outer_ssi_left SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("INSERT INTO outer_ssi_right VALUES (10, 1)")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
}
