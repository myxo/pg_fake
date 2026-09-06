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

#[test]
fn invalidates_inactive_catalogs_after_temporary_ddl_and_failed_changes() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TEMP TABLE local_rows (id INTEGER PRIMARY KEY); INSERT INTO local_rows VALUES (1)").unwrap();
    second.execute("CREATE TEMP TABLE local_rows (id INTEGER PRIMARY KEY); INSERT INTO local_rows VALUES (2)").unwrap();
    for _ in 0..3 {
        assert_eq!(
            first.query("SELECT * FROM local_rows", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            second.query("SELECT * FROM local_rows", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
    }
    first
        .execute("ALTER TABLE local_rows ADD COLUMN marker INTEGER DEFAULT 9")
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            first.query("SELECT * FROM local_rows", &[]).unwrap().rows,
            vec![vec![Value::Int4(1), Value::Int4(9)]]
        );
        assert_eq!(
            second.query("SELECT * FROM local_rows", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
    }
    assert!(
        first
            .execute(
                "ALTER TABLE local_rows ADD COLUMN failed INTEGER DEFAULT 7, ADD COLUMN id INTEGER"
            )
            .is_err()
    );
    second.query("SELECT * FROM local_rows", &[]).unwrap();
    assert_eq!(
        first.query("SELECT * FROM local_rows", &[]).unwrap().rows,
        vec![vec![Value::Int4(1), Value::Int4(9)]]
    );
    first
        .execute("BEGIN; ALTER TABLE local_rows ADD COLUMN rolled_back INTEGER DEFAULT 8")
        .unwrap();
    assert_eq!(
        second.query("SELECT * FROM local_rows", &[]).unwrap().rows,
        vec![vec![Value::Int4(2)]]
    );
    assert_eq!(
        first
            .query("SELECT * FROM local_rows", &[])
            .unwrap()
            .columns
            .len(),
        3
    );
    first.execute("ROLLBACK").unwrap();
    second.query("SELECT * FROM local_rows", &[]).unwrap();
    assert_eq!(
        first.query("SELECT * FROM local_rows", &[]).unwrap().rows,
        vec![vec![Value::Int4(1), Value::Int4(9)]]
    );
    drop(second);
    let mut replacement = db.create_session();
    replacement
        .execute("CREATE TEMP TABLE local_rows (id INTEGER); INSERT INTO local_rows VALUES (3)")
        .unwrap();
    assert_eq!(
        replacement
            .query("SELECT * FROM local_rows", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(3)]]
    );
    assert_eq!(
        first.query("SELECT * FROM local_rows", &[]).unwrap().rows,
        vec![vec![Value::Int4(1), Value::Int4(9)]]
    );
}

#[test]
fn clears_reused_session_catalogs_when_loading_an_old_snapshot() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    let mut old = db.create_session();
    first
        .execute("CREATE TABLE anchor_rows (id INTEGER); INSERT INTO anchor_rows VALUES (1)")
        .unwrap();
    old.execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    old.query("SELECT * FROM anchor_rows", &[]).unwrap();
    first
        .execute("CREATE TABLE later_rows (id INTEGER); INSERT INTO later_rows VALUES (2)")
        .unwrap();
    for _ in 0..3 {
        first.query("SELECT * FROM later_rows", &[]).unwrap();
        second.query("SELECT * FROM later_rows", &[]).unwrap();
    }
    assert_eq!(
        old.query("SELECT * FROM later_rows", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    old.execute("ROLLBACK").unwrap();
    for session in [&mut first, &mut second, &mut old] {
        assert_eq!(
            session.query("SELECT * FROM later_rows", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
    }
}

#[test]
fn retains_other_catalog_commits_after_committing_ddl_from_an_old_snapshot() {
    let db = Db::create();
    let mut old = db.create_session();
    let mut writer = db.create_session();
    old.execute("CREATE TABLE commit_anchor (id INTEGER)")
        .unwrap();
    old.execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    old.query("SELECT * FROM commit_anchor", &[]).unwrap();
    writer
        .execute("CREATE TABLE newer_catalog (id INTEGER); INSERT INTO newer_catalog VALUES (7)")
        .unwrap();
    old.execute("CREATE TABLE own_catalog (id INTEGER); COMMIT")
        .unwrap();
    for session in [&mut old, &mut writer] {
        assert_eq!(
            session
                .query("SELECT * FROM newer_catalog", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(7)]]
        );
        assert!(
            session
                .query("SELECT * FROM own_catalog", &[])
                .unwrap()
                .rows
                .is_empty()
        );
    }
}

#[test]
fn preserves_catalog_commit_visibility_with_pending_ddl_and_commit_failures() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("BEGIN; CREATE TABLE pending_catalog_commit (id INTEGER)")
        .unwrap();
    second
        .execute("CREATE TABLE successful_catalog_commit (id INTEGER)")
        .unwrap();
    assert_eq!(
        second
            .query("SELECT * FROM pending_catalog_commit", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    first.execute("ROLLBACK").unwrap();
    first
        .query("SELECT * FROM successful_catalog_commit", &[])
        .unwrap();
    second
        .query("SELECT * FROM successful_catalog_commit", &[])
        .unwrap();

    first.execute("BEGIN; CREATE TEMP TABLE dropped_at_commit (id SERIAL) ON COMMIT DROP; INSERT INTO dropped_at_commit DEFAULT VALUES; COMMIT").unwrap();
    assert_eq!(
        first
            .query("SELECT * FROM dropped_at_commit", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert!(
        first
            .query("SELECT nextval('dropped_at_commit_id_seq')", &[])
            .is_err()
    );

    first.execute("CREATE TABLE commit_parent (id INTEGER PRIMARY KEY); CREATE TABLE commit_child (id INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED)").unwrap();
    first.execute("BEGIN; CREATE TABLE failed_catalog_commit (id INTEGER); INSERT INTO commit_child VALUES (9)").unwrap();
    assert_eq!(
        first.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::ForeignKeyViolation
    );
    for session in [&mut first, &mut second] {
        assert_eq!(
            session
                .query("SELECT * FROM failed_catalog_commit", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert!(
            session
                .query("SELECT * FROM commit_child", &[])
                .unwrap()
                .rows
                .is_empty()
        );
    }
}
