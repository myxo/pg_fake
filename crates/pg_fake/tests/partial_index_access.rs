use pg_fake::{Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn selects_only_visible_rows_satisfying_the_proven_partial_predicate() {
    let db = Db::create();
    let mut writer = db.create_session();
    writer
        .execute(
            "CREATE TABLE memberships (
               id INTEGER PRIMARY KEY, account_id INTEGER, group_id INTEGER,
               deleted_at INTEGER, role TEXT
             ); \
             CREATE UNIQUE INDEX active_pair ON memberships (group_id, account_id)
               WHERE deleted_at IS NULL; \
             CREATE INDEX active_account ON memberships (account_id, group_id)
               WHERE deleted_at IS NULL; \
             INSERT INTO memberships VALUES
               (1, 10, 20, NULL, 'active'),
               (2, 10, 20, 1, 'deleted'),
               (3, 10, 21, NULL, 'second'),
               (4, 11, 22, 1, 'deleted only')",
        )
        .unwrap();
    let mut reader = db.create_session();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    let active =
        "SELECT id, role FROM memberships WHERE account_id = $1 AND deleted_at IS NULL ORDER BY id";
    let prepared = reader.prepare(active).unwrap();
    let expected = vec![
        vec![Value::Int4(1), Value::Text("active".into())],
        vec![Value::Int4(3), Value::Text("second".into())],
    ];
    assert_eq!(
        reader
            .query_prepared(&prepared, &[Value::Int4(10)])
            .unwrap()
            .rows,
        expected
    );
    assert!(
        reader
            .query_prepared(&prepared, &[Value::Int4(11)])
            .unwrap()
            .rows
            .is_empty()
    );
    for sql in [
        "SELECT id, role FROM memberships WHERE deleted_at IS NULL AND account_id = 10 ORDER BY id",
        "SELECT id, role FROM memberships WHERE ((account_id = 10) AND (deleted_at IS NULL)) ORDER BY id",
    ] {
        assert_eq!(query_rows(&mut reader, sql), expected);
    }
    assert_eq!(
        query_rows(
            &mut reader,
            "SELECT id FROM memberships WHERE account_id = 10 ORDER BY id"
        ),
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );
    let nullable_comparison = reader
        .prepare("SELECT id FROM memberships WHERE account_id = $1 AND deleted_at = $2 ORDER BY id")
        .unwrap();
    for (deleted_at, expected) in [
        (Value::Int4(1), vec![vec![Value::Int4(2)]]),
        (Value::Null, Vec::new()),
        (Value::Int4(2), Vec::new()),
        (Value::Int4(1), vec![vec![Value::Int4(2)]]),
    ] {
        assert_eq!(
            reader
                .query_prepared(&nullable_comparison, &[Value::Int4(10), deleted_at])
                .unwrap()
                .rows,
            expected
        );
    }
    assert_eq!(
        query_rows(
            &mut reader,
            "SELECT id FROM memberships WHERE account_id = 10 AND (deleted_at IS NULL OR role = 'deleted') ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );
    let complete = reader
        .prepare(
            "SELECT id FROM memberships WHERE group_id = $1 AND account_id = $2 AND deleted_at IS NULL",
        )
        .unwrap();
    assert_eq!(
        reader
            .query_prepared(&complete, &[Value::Int4(20), Value::Int4(10)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    writer
        .execute("UPDATE memberships SET deleted_at = 2 WHERE id = 1")
        .unwrap();
    writer
        .execute("UPDATE memberships SET deleted_at = NULL WHERE id = 2")
        .unwrap();
    assert_eq!(
        reader
            .query_prepared(&complete, &[Value::Int4(20), Value::Int4(10)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        writer
            .query_prepared(&complete, &[Value::Int4(20), Value::Int4(10)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
    reader.execute("ROLLBACK").unwrap();
    writer.execute("BEGIN; SAVEPOINT before_change; UPDATE memberships SET deleted_at = 3 WHERE id = 2; ROLLBACK TO SAVEPOINT before_change; COMMIT").unwrap();
    assert_eq!(
        writer
            .query_prepared(&complete, &[Value::Int4(20), Value::Int4(10)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
    writer
        .execute("DROP INDEX active_pair; DROP INDEX active_account")
        .unwrap();
    assert_eq!(
        writer
            .query_prepared(&complete, &[Value::Int4(20), Value::Int4(10)])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    let complete = writer
        .prepare(
            "SELECT id FROM memberships WHERE group_id = $1 AND account_id = $2 AND deleted_at IS NULL",
        )
        .unwrap();
    assert_eq!(
        writer
            .query_prepared(&complete, &[Value::Int4(20), Value::Int4(10)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
}

#[test]
fn proves_conjunctive_null_tests_and_preserves_unknown_fallbacks() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE partial_flags (id INTEGER PRIMARY KEY, key_value INTEGER,
               deleted_at INTEGER, label TEXT); \
             CREATE INDEX partial_flags_key ON partial_flags (key_value)
               WHERE deleted_at IS NULL AND label IS NOT NULL; \
             INSERT INTO partial_flags VALUES
               (1, 7, NULL, 'active'), (2, 7, NULL, NULL),
               (3, 7, 1, 'deleted'), (4, 8, NULL, 'other')",
        )
        .unwrap();
    for key in [7, 8, 9] {
        let prepared = session
            .prepare(
                "SELECT id FROM partial_flags WHERE key_value = $1
             AND label IS NOT NULL AND deleted_at IS NULL ORDER BY id",
            )
            .unwrap();
        let got = session
            .query_prepared(&prepared, &[Value::Int4(key)])
            .unwrap()
            .rows;
        let expected = match key {
            7 => vec![vec![Value::Int4(1)]],
            8 => vec![vec![Value::Int4(4)]],
            _ => Vec::new(),
        };
        assert_eq!(got, expected);
    }
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id FROM partial_flags WHERE key_value = 7 AND deleted_at IS NULL ORDER BY id"
        ),
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id FROM partial_flags WHERE key_value = 7 ORDER BY id"
        ),
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );
}

#[test]
fn serializable_partial_probe_tracks_inserted_matches() {
    let db = Db::create();
    let mut first = db.create_session();
    first
        .execute(
            "CREATE TABLE partial_ssi (account_id INTEGER, group_id INTEGER, deleted_at INTEGER); \
             CREATE INDEX partial_ssi_key ON partial_ssi(account_id, group_id)
               WHERE deleted_at IS NULL; \
             CREATE TABLE partial_ssi_flag (id INTEGER PRIMARY KEY, value INTEGER); \
             INSERT INTO partial_ssi_flag VALUES (1, 0)",
        )
        .unwrap();
    let mut second = db.create_session();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    assert!(
        query_rows(
            &mut first,
            "SELECT group_id FROM partial_ssi WHERE account_id = 1 AND deleted_at IS NULL"
        )
        .is_empty()
    );
    assert_eq!(
        query_rows(
            &mut second,
            "SELECT value FROM partial_ssi_flag WHERE id = 1"
        ),
        vec![vec![Value::Int4(0)]]
    );
    first
        .execute("UPDATE partial_ssi_flag SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("INSERT INTO partial_ssi VALUES (1, 10, NULL)")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
}

#[test]
fn maintains_partial_entries_through_rollback_and_reclamation() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE partial_lifecycle (id INTEGER PRIMARY KEY, key_value INTEGER,
               deleted_at INTEGER); \
             CREATE INDEX partial_lifecycle_key ON partial_lifecycle(key_value)
               WHERE deleted_at IS NULL",
        )
        .unwrap();
    session
        .execute("BEGIN; INSERT INTO partial_lifecycle VALUES (1, 1, 1); ROLLBACK")
        .unwrap();
    session
        .execute("INSERT INTO partial_lifecycle VALUES (1, 1, 1)")
        .unwrap();
    session
        .execute("UPDATE partial_lifecycle SET key_value = 2, deleted_at = NULL WHERE id = 1")
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id FROM partial_lifecycle WHERE key_value = 2 AND deleted_at IS NULL",
        ),
        vec![vec![Value::Int4(1)]]
    );
    session
        .execute("UPDATE partial_lifecycle SET deleted_at = 2 WHERE id = 1")
        .unwrap();
    assert!(
        query_rows(
            &mut session,
            "SELECT id FROM partial_lifecycle WHERE key_value = 2 AND deleted_at IS NULL",
        )
        .is_empty()
    );
    session
        .execute("BEGIN; SAVEPOINT before_activation; UPDATE partial_lifecycle SET deleted_at = NULL WHERE id = 1; ROLLBACK TO SAVEPOINT before_activation; COMMIT")
        .unwrap();
    assert!(
        query_rows(
            &mut session,
            "SELECT id FROM partial_lifecycle WHERE key_value = 2 AND deleted_at IS NULL",
        )
        .is_empty()
    );
}

#[test]
fn builds_partial_index_after_adding_predicate_column() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE evolved_partial (id INTEGER PRIMARY KEY, key_value INTEGER); \
             INSERT INTO evolved_partial VALUES (1, 7); \
             ALTER TABLE evolved_partial ADD COLUMN marker INTEGER; \
             UPDATE evolved_partial SET marker = 1 WHERE id = 1; \
             CREATE INDEX evolved_partial_key ON evolved_partial(key_value)
               WHERE marker IS NOT NULL",
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id FROM evolved_partial WHERE key_value = 7 AND marker IS NOT NULL",
        ),
        vec![vec![Value::Int4(1)]]
    );
}
