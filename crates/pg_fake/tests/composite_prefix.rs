use pg_fake::{Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn reads_bounded_unique_and_nonunique_prefixes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE prefix_unique (tenant INTEGER, code TEXT, payload TEXT); \
             CREATE UNIQUE INDEX prefix_unique_key ON prefix_unique(tenant, code); \
             CREATE TABLE prefix_nonunique (tenant INTEGER, code TEXT, payload TEXT); \
             CREATE INDEX prefix_nonunique_key ON prefix_nonunique(tenant, code); \
             CREATE TABLE prefix_triple (tenant INTEGER, region INTEGER, code TEXT); \
             CREATE UNIQUE INDEX prefix_triple_key ON prefix_triple(tenant, region, code); \
             CREATE TABLE prefix_late (tenant INTEGER, code INTEGER); \
             CREATE TABLE prefix_partial (tenant INTEGER, code INTEGER, deleted_at INTEGER); \
             CREATE INDEX prefix_partial_key ON prefix_partial(tenant, code) \
             WHERE deleted_at IS NULL; \
             INSERT INTO prefix_unique VALUES (1, 'a', 'first'), (1, 'b', 'second'), \
             (2, 'a', 'third'), (NULL, 'z', 'null tenant'), \
             (1, NULL, 'null code'), (1, NULL, 'second null'); \
             INSERT INTO prefix_nonunique VALUES (1, 'a', 'first'), (1, 'a', 'second'), \
             (1, 'b', 'third'), (2, 'a', 'fourth'); \
             INSERT INTO prefix_triple VALUES (1, 2, 'a'), (1, 2, 'b'), (1, 2, NULL), \
             (1, 3, 'c'), (2, 2, 'd'); \
             INSERT INTO prefix_late VALUES (1, NULL), (1, 5), (2, 7); \
             CREATE UNIQUE INDEX prefix_late_key ON prefix_late(tenant, code); \
             INSERT INTO prefix_partial VALUES (1, 10, NULL), (1, 20, 1)",
        )
        .unwrap();
    let unique = session
        .prepare("SELECT code FROM prefix_unique WHERE tenant = $1 ORDER BY code")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&unique, &[Value::Int4(1)])
            .unwrap()
            .rows,
        vec![
            vec![Value::Text("a".into())],
            vec![Value::Text("b".into())],
            vec![Value::Null],
            vec![Value::Null],
        ]
    );
    assert!(
        session
            .query_prepared(&unique, &[Value::Int4(99)])
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        session
            .query_prepared(&unique, &[Value::Null])
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT code FROM prefix_unique WHERE tenant IS NULL",
        ),
        vec![vec![Value::Text("z".into())]]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT payload FROM prefix_unique WHERE tenant = 1 AND code IS NULL ORDER BY payload",
        ),
        vec![
            vec![Value::Text("null code".into())],
            vec![Value::Text("second null".into())],
        ]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT payload FROM prefix_nonunique WHERE tenant = 1 ORDER BY payload",
        ),
        vec![
            vec![Value::Text("first".into())],
            vec![Value::Text("second".into())],
            vec![Value::Text("third".into())],
        ]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT payload FROM prefix_unique WHERE code = 'a' ORDER BY payload",
        ),
        vec![
            vec![Value::Text("first".into())],
            vec![Value::Text("third".into())],
        ]
    );
    let triple = session
        .prepare("SELECT code FROM prefix_triple WHERE region = $2 AND tenant = $1 ORDER BY code")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&triple, &[Value::Int4(1), Value::Int4(2)])
            .unwrap()
            .rows,
        vec![
            vec![Value::Text("a".into())],
            vec![Value::Text("b".into())],
            vec![Value::Null],
        ]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT code FROM prefix_late WHERE tenant = 1 ORDER BY code",
        ),
        vec![vec![Value::Int4(5)], vec![Value::Null]]
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT code FROM prefix_partial WHERE tenant = 1 ORDER BY code",
        ),
        vec![vec![Value::Int4(10)], vec![Value::Int4(20)]]
    );
}

#[test]
fn rechecks_changed_prefix_keys_and_deduplicates_row_versions() {
    let db = Db::create();
    let mut writer = db.create_session();
    writer
        .execute(
            "CREATE TABLE prefix_history (id INTEGER PRIMARY KEY, tenant INTEGER, code INTEGER); \
             CREATE UNIQUE INDEX prefix_history_key ON prefix_history(tenant, code); \
             INSERT INTO prefix_history VALUES (1, 1, 10), (2, 1, 20), (3, 2, 30)",
        )
        .unwrap();
    let mut reader = db.create_session();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    let old = "SELECT id, code FROM prefix_history WHERE tenant = 1 ORDER BY id";
    assert_eq!(
        query_rows(&mut reader, old),
        vec![
            vec![Value::Int4(1), Value::Int4(10)],
            vec![Value::Int4(2), Value::Int4(20)],
        ]
    );
    writer
        .execute("UPDATE prefix_history SET code = 11 WHERE id = 1")
        .unwrap();
    assert_eq!(
        query_rows(&mut reader, old),
        vec![
            vec![Value::Int4(1), Value::Int4(10)],
            vec![Value::Int4(2), Value::Int4(20)],
        ]
    );
    reader.execute("ROLLBACK").unwrap();
    assert_eq!(
        query_rows(&mut reader, old),
        vec![
            vec![Value::Int4(1), Value::Int4(11)],
            vec![Value::Int4(2), Value::Int4(20)],
        ]
    );
    writer.execute("BEGIN").unwrap();
    writer
        .execute("UPDATE prefix_history SET tenant = 2, code = 31 WHERE id = 1")
        .unwrap();
    assert_eq!(
        query_rows(&mut writer, old),
        vec![vec![Value::Int4(2), Value::Int4(20)]]
    );
    writer.execute("ROLLBACK").unwrap();
    assert_eq!(
        query_rows(&mut writer, old),
        vec![
            vec![Value::Int4(1), Value::Int4(11)],
            vec![Value::Int4(2), Value::Int4(20)],
        ]
    );
}

#[test]
fn retains_null_suffix_entries_across_updates_and_rollback() {
    let db = Db::create();
    let mut writer = db.create_session();
    writer
        .execute(
            "CREATE TABLE prefix_null_history (id INTEGER PRIMARY KEY, tenant INTEGER, code INTEGER); \
             CREATE UNIQUE INDEX prefix_null_history_key ON prefix_null_history(tenant, code); \
             INSERT INTO prefix_null_history VALUES (1, 1, NULL)",
        )
        .unwrap();
    let mut reader = db.create_session();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    let query = "SELECT code FROM prefix_null_history WHERE tenant = 1";
    assert_eq!(query_rows(&mut reader, query), vec![vec![Value::Null]]);
    writer
        .execute("UPDATE prefix_null_history SET code = 10 WHERE id = 1")
        .unwrap();
    assert_eq!(query_rows(&mut reader, query), vec![vec![Value::Null]]);
    reader.execute("ROLLBACK").unwrap();
    assert_eq!(query_rows(&mut reader, query), vec![vec![Value::Int4(10)]]);
    writer.execute("BEGIN").unwrap();
    writer
        .execute("UPDATE prefix_null_history SET code = NULL WHERE id = 1")
        .unwrap();
    assert_eq!(query_rows(&mut writer, query), vec![vec![Value::Null]]);
    writer.execute("ROLLBACK").unwrap();
    assert_eq!(query_rows(&mut writer, query), vec![vec![Value::Int4(10)]]);
}

#[test]
fn serializable_prefix_probe_tracks_inserted_matches() {
    let db = Db::create();
    let mut first = db.create_session();
    first
        .execute(
            "CREATE TABLE prefix_ssi (tenant INTEGER, code INTEGER); \
             CREATE UNIQUE INDEX prefix_ssi_key ON prefix_ssi(tenant, code); \
             CREATE TABLE prefix_flag (id INTEGER PRIMARY KEY, value INTEGER); \
             INSERT INTO prefix_flag VALUES (1, 0)",
        )
        .unwrap();
    let mut second = db.create_session();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    assert!(query_rows(&mut first, "SELECT code FROM prefix_ssi WHERE tenant = 1").is_empty());
    assert_eq!(
        query_rows(&mut second, "SELECT value FROM prefix_flag WHERE id = 1"),
        vec![vec![Value::Int4(0)]]
    );
    first
        .execute("UPDATE prefix_flag SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("INSERT INTO prefix_ssi VALUES (1, 10)")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
}
