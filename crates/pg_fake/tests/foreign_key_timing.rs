use pg_fake::{api::Db, error::SqlState};

#[test]
fn deferrable_foreign_keys_are_checked_at_commit() {
    let db = Db::new();
    let mut session = db.session();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    session
        .execute("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER CONSTRAINT children_parent_fkey REFERENCES parents DEFERRABLE INITIALLY DEFERRED)")
        .unwrap();
    session.execute("BEGIN").unwrap();
    session
        .execute("INSERT INTO children VALUES (1, 2)")
        .unwrap();
    session.execute("INSERT INTO parents VALUES (2)").unwrap();
    session.execute("SET CONSTRAINTS ALL IMMEDIATE").unwrap();
    session.execute("COMMIT").unwrap();

    session.execute("BEGIN").unwrap();
    session
        .execute("INSERT INTO children VALUES (2, 3)")
        .unwrap();
    assert_eq!(
        session.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::ForeignKeyViolation
    );
}
