use pg_fake::{api::Db, error::SqlState, value::Value};

#[test]
fn checks_deferrable_foreign_keys_at_commit() {
    let db = Db::create();
    let mut session = db.create_session();
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

#[test]
fn checks_initially_deferred_foreign_keys_in_prepared_autocommit() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    session
        .execute("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents DEFERRABLE INITIALLY DEFERRED)")
        .unwrap();
    let statement = session
        .prepare("INSERT INTO children VALUES ($1, $2)")
        .unwrap();

    assert_eq!(
        session
            .execute_prepared(&statement, &[Value::Int4(1), Value::Int4(2)])
            .unwrap_err()
            .sqlstate,
        SqlState::ForeignKeyViolation
    );
}
