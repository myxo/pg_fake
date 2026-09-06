use pg_fake::{api::Db, error::SqlState, value::Value};

#[test]
fn preserves_catalog_visibility_across_commit_rollback_and_old_snapshots() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    reader
        .execute("CREATE TABLE anchor (id INTEGER); INSERT INTO anchor VALUES (1)")
        .unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    reader.query("SELECT * FROM anchor", &[]).unwrap();
    writer
        .execute("CREATE TABLE committed_later (id INTEGER)")
        .unwrap();
    assert_eq!(
        reader
            .query("SELECT * FROM committed_later", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    reader.execute("ROLLBACK").unwrap();
    reader.query("SELECT * FROM committed_later", &[]).unwrap();
    for finish in ["ROLLBACK", "COMMIT"] {
        writer
            .execute("BEGIN; CREATE TABLE pending_catalog (id INTEGER)")
            .unwrap();
        assert_eq!(
            reader.query("SELECT * FROM anchor", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        writer.execute(finish).unwrap();
        let result = reader.query("SELECT * FROM pending_catalog", &[]);
        if finish == "ROLLBACK" {
            assert_eq!(result.unwrap_err().sqlstate, SqlState::UndefinedTable);
        } else {
            assert!(result.unwrap().rows.is_empty());
        }
    }
}

#[test]
fn restores_catalog_after_partially_applied_ddl_fails() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE ddl_failure (id INTEGER); INSERT INTO ddl_failure VALUES (1)")
        .unwrap();
    session.query("SELECT * FROM ddl_failure", &[]).unwrap();
    assert!(
        session
            .execute(
                "ALTER TABLE ddl_failure ADD COLUMN extra INTEGER DEFAULT 7, ADD COLUMN id INTEGER"
            )
            .is_err()
    );
    let result = session.query("SELECT * FROM ddl_failure", &[]).unwrap();
    assert_eq!(result.columns.len(), 1);
    assert_eq!(result.rows, vec![vec![Value::Int4(1)]]);
    session
        .execute("ALTER TABLE ddl_failure ADD COLUMN extra INTEGER DEFAULT 9")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT * FROM ddl_failure", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Int4(9)]]
    );
}

#[test]
fn switches_cached_catalogs_between_temporary_schemas() {
    let db = Db::create();
    let mut first = db.create_session();
    first.execute("CREATE TABLE shadow (id INTEGER); INSERT INTO shadow VALUES (1); CREATE TEMP TABLE shadow (id INTEGER); INSERT INTO shadow VALUES (2)").unwrap();
    let mut second = db.create_session();
    second
        .execute("CREATE TEMP TABLE shadow (id INTEGER); INSERT INTO shadow VALUES (3)")
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            first.query("SELECT * FROM shadow", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
        assert_eq!(
            second.query("SELECT * FROM shadow", &[]).unwrap().rows,
            vec![vec![Value::Int4(3)]]
        );
    }
    drop(second);
    assert_eq!(
        first.query("SELECT * FROM shadow", &[]).unwrap().rows,
        vec![vec![Value::Int4(2)]]
    );
}
