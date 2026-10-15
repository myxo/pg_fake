use pg_fake::{Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn selects_complete_composite_keys_and_rechecks_other_predicates() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE composite_keyed (group_id INTEGER, code TEXT, payload TEXT); \
             CREATE UNIQUE INDEX composite_keyed_pair ON composite_keyed(group_id, code); \
             CREATE TABLE composite_reverse (group_id INTEGER, code TEXT, payload TEXT); \
             CREATE UNIQUE INDEX composite_reverse_pair ON composite_reverse(code, group_id); \
             CREATE TABLE composite_many (group_id INTEGER, code TEXT, payload TEXT); \
             CREATE INDEX composite_many_pair ON composite_many(group_id, code); \
             CREATE TABLE composite_parent (id INTEGER PRIMARY KEY, group_id INTEGER, code TEXT); \
             CREATE INDEX composite_parent_group ON composite_parent(group_id); \
             INSERT INTO composite_keyed VALUES (1, 'a', 'first'), (1, 'b', 'second'), \
             (2, 'a', 'third'), (NULL, 'a', 'null group'), (3, NULL, 'null code'); \
             INSERT INTO composite_reverse VALUES (1, 'a', 'reverse first'), \
             (1, 'b', 'reverse second'), (2, 'a', 'reverse third'); \
             INSERT INTO composite_many VALUES (1, 'a', 'first'), (1, 'a', 'second'), \
             (1, 'b', 'third'); \
             INSERT INTO composite_parent VALUES (10, 1, 'a'), (11, 1, 'b'), \
             (12, 2, 'a'), (13, 3, NULL), (14, 1, 'a')",
        )
        .unwrap();
    for sql in [
        "SELECT payload FROM composite_keyed WHERE group_id = $1 AND code = $2",
        "SELECT payload FROM composite_keyed WHERE code = $2 AND group_id = $1",
        "SELECT payload FROM composite_keyed WHERE (code = $2 AND (group_id = $1))",
        "SELECT payload FROM composite_keyed WHERE group_id = $1 AND code = $2 AND payload = 'first'",
    ] {
        let prepared = session.prepare(sql).unwrap();
        assert_eq!(
            session
                .query_prepared(&prepared, &[Value::Int4(1), Value::Text("a".into())])
                .unwrap()
                .rows,
            vec![vec![Value::Text("first".into())]]
        );
        assert!(
            session
                .query_prepared(&prepared, &[Value::Int4(1), Value::Text("missing".into())])
                .unwrap()
                .rows
                .is_empty()
        );
    }
    let reverse = session
        .prepare("SELECT payload FROM composite_reverse WHERE group_id = $1 AND code = $2")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&reverse, &[Value::Int4(1), Value::Text("a".into())])
            .unwrap()
            .rows,
        vec![vec![Value::Text("reverse first".into())]]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT payload FROM composite_keyed WHERE code = 'a' AND group_id = 2",
        ),
        vec![vec![Value::Text("third".into())]]
    );
    assert!(
        query_rows(
            &mut session,
            "SELECT payload FROM composite_keyed WHERE group_id = NULL AND code = 'a'",
        )
        .is_empty()
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT payload FROM composite_keyed WHERE group_id = 1 ORDER BY code",
        ),
        vec![
            vec![Value::Text("first".into())],
            vec![Value::Text("second".into())],
        ]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT payload FROM composite_many WHERE code = 'a' AND group_id = 1 ORDER BY payload",
        ),
        vec![
            vec![Value::Text("first".into())],
            vec![Value::Text("second".into())],
        ]
    );
    let join = session
        .prepare(
            "SELECT p.id, k.payload FROM composite_parent p JOIN composite_keyed k \
             ON k.code = p.code AND k.group_id = p.group_id WHERE p.id = $1",
        )
        .unwrap();
    for (id, expected) in [
        (10, Some("first")),
        (11, Some("second")),
        (12, Some("third")),
        (13, None),
        (14, Some("first")),
    ] {
        let rows = session
            .query_prepared(&join, &[Value::Int4(id)])
            .unwrap()
            .rows;
        assert_eq!(
            rows,
            expected
                .map(|payload| vec![vec![Value::Int4(id), Value::Text(payload.into())]])
                .unwrap_or_default()
        );
    }
    let repeated_join = session
        .prepare(
            "SELECT p.id, r.payload FROM composite_parent p JOIN composite_reverse r \
             ON r.group_id = p.group_id AND r.code = p.code \
             WHERE p.group_id = $1 AND p.code = $2",
        )
        .unwrap();
    let mut repeated_rows = session
        .query_prepared(&repeated_join, &[Value::Int4(1), Value::Text("a".into())])
        .unwrap()
        .rows;
    repeated_rows.sort_by_key(|row| match row[0] {
        Value::Int4(id) => id,
        _ => unreachable!(),
    });
    assert_eq!(
        repeated_rows,
        vec![
            vec![Value::Int4(10), Value::Text("reverse first".into())],
            vec![Value::Int4(14), Value::Text("reverse first".into())],
        ]
    );
}

#[test]
fn preserves_composite_index_history_and_mutation_rollback() {
    let db = Db::create();
    let mut writer = db.create_session();
    writer
        .execute(
            "CREATE TABLE composite_history (group_id INTEGER, code TEXT, payload TEXT, \
             UNIQUE(group_id, code)); \
             INSERT INTO composite_history VALUES (1, 'a', 'original')",
        )
        .unwrap();
    let mut reader = db.create_session();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    let old_key = "SELECT payload FROM composite_history WHERE group_id = 1 AND code = 'a'";
    let new_key = "SELECT payload FROM composite_history WHERE group_id = 2 AND code = 'b'";
    assert_eq!(
        query_rows(&mut reader, old_key),
        vec![vec![Value::Text("original".into())]]
    );
    writer
        .execute("UPDATE composite_history SET group_id = 2, code = 'b' WHERE group_id = 1 AND code = 'a'")
        .unwrap();
    assert_eq!(
        query_rows(&mut reader, old_key),
        vec![vec![Value::Text("original".into())]]
    );
    assert!(query_rows(&mut reader, new_key).is_empty());
    reader.execute("ROLLBACK").unwrap();
    assert!(query_rows(&mut reader, old_key).is_empty());
    assert_eq!(
        query_rows(&mut reader, new_key),
        vec![vec![Value::Text("original".into())]]
    );
    writer.execute("BEGIN").unwrap();
    writer.execute("SAVEPOINT before_change").unwrap();
    writer
        .execute(
            "UPDATE composite_history SET payload = 'changed' WHERE code = 'b' AND group_id = 2",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut writer, new_key),
        vec![vec![Value::Text("changed".into())]]
    );
    writer
        .execute("ROLLBACK TO SAVEPOINT before_change")
        .unwrap();
    assert_eq!(
        query_rows(&mut writer, new_key),
        vec![vec![Value::Text("original".into())]]
    );
    writer.execute("ROLLBACK").unwrap();
    writer.execute("BEGIN").unwrap();
    writer
        .execute("DELETE FROM composite_history WHERE group_id = 2 AND code = 'b'")
        .unwrap();
    assert!(query_rows(&mut writer, new_key).is_empty());
    writer.execute("ROLLBACK").unwrap();
    assert_eq!(
        query_rows(&mut writer, new_key),
        vec![vec![Value::Text("original".into())]]
    );
}

#[test]
fn rechecks_nonunique_composite_keys_in_old_snapshots() {
    let db = Db::create();
    let mut writer = db.create_session();
    writer
        .execute(
            "CREATE TABLE composite_nonunique_history (id INTEGER PRIMARY KEY, group_id INTEGER, code TEXT); \
             CREATE INDEX composite_nonunique_history_key ON composite_nonunique_history(group_id, code); \
             INSERT INTO composite_nonunique_history VALUES (1, 1, 'a'), (2, 1, 'a')",
        )
        .unwrap();
    let mut reader = db.create_session();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    let old_key =
        "SELECT id FROM composite_nonunique_history WHERE group_id = 1 AND code = 'a' ORDER BY id";
    let new_key =
        "SELECT id FROM composite_nonunique_history WHERE group_id = 1 AND code = 'b' ORDER BY id";
    assert_eq!(
        query_rows(&mut reader, old_key),
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
    writer
        .execute("UPDATE composite_nonunique_history SET code = 'b' WHERE id = 1")
        .unwrap();
    assert_eq!(
        query_rows(&mut reader, old_key),
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
    assert!(query_rows(&mut reader, new_key).is_empty());
    reader.execute("ROLLBACK").unwrap();
    assert_eq!(query_rows(&mut reader, old_key), vec![vec![Value::Int4(2)]]);
    assert_eq!(query_rows(&mut reader, new_key), vec![vec![Value::Int4(1)]]);
}

#[test]
fn serializable_composite_absent_key_tracks_insert() {
    let db = Db::create();
    let mut first = db.create_session();
    first
        .execute(
            "CREATE TABLE composite_ssi (group_id INTEGER, account_id INTEGER, \
             UNIQUE(group_id, account_id)); \
             CREATE TABLE composite_ssi_flag (id INTEGER PRIMARY KEY, value INTEGER); \
             INSERT INTO composite_ssi_flag VALUES (1, 0)",
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
            "SELECT group_id FROM composite_ssi WHERE group_id = 1 AND account_id = 2",
        )
        .is_empty()
    );
    assert_eq!(
        query_rows(
            &mut second,
            "SELECT value FROM composite_ssi_flag WHERE id = 1"
        ),
        vec![vec![Value::Int4(0)]]
    );
    first
        .execute("UPDATE composite_ssi_flag SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("INSERT INTO composite_ssi VALUES (1, 2)")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
}

#[test]
fn serializable_nonunique_composite_probe_tracks_insert() {
    let db = Db::create();
    let mut first = db.create_session();
    first
        .execute(
            "CREATE TABLE composite_multi_ssi (group_id INTEGER, account_id INTEGER); \
             CREATE INDEX composite_multi_ssi_key ON composite_multi_ssi(group_id, account_id); \
             CREATE TABLE composite_multi_flag (id INTEGER PRIMARY KEY, value INTEGER); \
             INSERT INTO composite_multi_flag VALUES (1, 0)",
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
            "SELECT group_id FROM composite_multi_ssi WHERE group_id = 1 AND account_id = 2",
        )
        .is_empty()
    );
    assert_eq!(
        query_rows(
            &mut second,
            "SELECT value FROM composite_multi_flag WHERE id = 1"
        ),
        vec![vec![Value::Int4(0)]]
    );
    first
        .execute("UPDATE composite_multi_flag SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("INSERT INTO composite_multi_ssi VALUES (1, 2)")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
}
