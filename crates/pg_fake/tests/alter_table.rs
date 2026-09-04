use pg_fake::{api::Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::api::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn evolves_columns_and_rows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER); \
             INSERT INTO items VALUES (1, 4), (2, 5); \
             ALTER TABLE items \
               ADD COLUMN doubled INTEGER DEFAULT 8 NOT NULL, \
               RENAME COLUMN value TO amount, \
               ALTER COLUMN amount TYPE BIGINT USING amount * 10, \
               ALTER COLUMN doubled SET DEFAULT 9",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, amount, doubled FROM items ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int8(40), Value::Int4(8)],
            vec![Value::Int4(2), Value::Int8(50), Value::Int4(8)],
        ]
    );
    session
        .execute("INSERT INTO items (id, amount) VALUES (3, 60)")
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT doubled FROM items WHERE id = 3"),
        vec![vec![Value::Int4(9)]]
    );

    session
        .execute("ALTER TABLE items DROP COLUMN amount, ALTER COLUMN doubled DROP DEFAULT")
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT * FROM items ORDER BY id"),
        vec![
            vec![Value::Int4(1), Value::Int4(8)],
            vec![Value::Int4(2), Value::Int4(8)],
            vec![Value::Int4(3), Value::Int4(9)],
        ]
    );
}

#[test]
fn validates_added_constraints_and_not_valid_lifecycle() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY); \
             INSERT INTO parents VALUES (1); \
             CREATE TABLE children (id INTEGER, parent_id INTEGER); \
             INSERT INTO children VALUES (1, 1), (2, 99); \
             ALTER TABLE children ADD CONSTRAINT positive CHECK (id > 0) NOT VALID; \
             ALTER TABLE children ADD CONSTRAINT parent_fk \
               FOREIGN KEY (parent_id) REFERENCES parents(id) NOT VALID",
        )
        .unwrap();
    session
        .execute(
            "ALTER TABLE children ADD COLUMN second_parent INTEGER REFERENCES parents(id); \
             INSERT INTO children VALUES (3, 1, 1)",
        )
        .unwrap();

    assert_eq!(
        session
            .execute("INSERT INTO children (id, parent_id) VALUES (-1, 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::CheckViolation
    );
    assert_eq!(
        session
            .execute("ALTER TABLE children VALIDATE CONSTRAINT parent_fk")
            .unwrap_err()
            .sqlstate,
        SqlState::ForeignKeyViolation
    );
    session
        .execute(
            "UPDATE children SET parent_id = 1 WHERE parent_id = 99; \
             ALTER TABLE children VALIDATE CONSTRAINT parent_fk; \
             ALTER TABLE children VALIDATE CONSTRAINT positive; \
             ALTER TABLE children ADD CONSTRAINT child_id_key UNIQUE (id)",
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO children (id, parent_id) VALUES (1, 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
}

#[test]
fn rolls_back_schema_changes_and_cascades_foreign_keys() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE parent (id INTEGER PRIMARY KEY, value INTEGER); \
             CREATE TABLE child (parent_id INTEGER REFERENCES parent(id)); \
             INSERT INTO parent VALUES (1, 10); \
             INSERT INTO child VALUES (1)",
        )
        .unwrap();
    session
        .execute(
            "BEGIN; \
             ALTER TABLE parent RENAME TO renamed; \
             ALTER TABLE renamed RENAME COLUMN value TO amount; \
             ALTER TABLE renamed ADD COLUMN extra INTEGER DEFAULT 7; \
             ROLLBACK",
        )
        .unwrap();

    assert_eq!(
        query_rows(&mut session, "SELECT id, value FROM parent"),
        vec![vec![Value::Int4(1), Value::Int4(10)]]
    );
    session
        .execute("ALTER TABLE parent DROP COLUMN id CASCADE")
        .unwrap();
    session.execute("INSERT INTO child VALUES (999)").unwrap();
    assert!(matches!(
        session.execute("SELECT * FROM renamed"),
        Err(error) if error.sqlstate == SqlState::UndefinedTable
    ));
}

#[test]
fn keeps_multi_action_failure_atomic() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE values_table (value INTEGER); INSERT INTO values_table VALUES (NULL)",
        )
        .unwrap();

    assert_eq!(
        session
            .execute(
                "ALTER TABLE values_table ADD COLUMN added INTEGER DEFAULT 1, \
                 ALTER COLUMN value SET NOT NULL",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::NotNullViolation
    );
    assert_eq!(
        query_rows(&mut session, "SELECT * FROM values_table"),
        vec![vec![Value::Null]]
    );
}

#[test]
fn applies_existence_guards_and_constraint_drops() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "ALTER TABLE IF EXISTS missing ADD COLUMN value INTEGER; \
             CREATE TABLE guarded (id INTEGER, value INTEGER); \
             ALTER TABLE guarded ADD COLUMN IF NOT EXISTS value INTEGER; \
             ALTER TABLE guarded DROP COLUMN IF EXISTS absent; \
             ALTER TABLE guarded ADD CONSTRAINT guarded_key UNIQUE (id); \
             ALTER TABLE guarded DROP CONSTRAINT IF EXISTS absent; \
             ALTER TABLE guarded DROP CONSTRAINT guarded_key; \
             INSERT INTO guarded VALUES (1, 1), (1, 2)",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT * FROM guarded ORDER BY value"),
        vec![
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(1), Value::Int4(2)],
        ]
    );
}

#[test]
fn rejects_constraint_dependencies_without_cascade() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE dependency_parent (id INTEGER PRIMARY KEY, code INTEGER UNIQUE); \
             CREATE TABLE dependency_child \
               (parent_id INTEGER REFERENCES dependency_parent(id), \
                parent_code INTEGER REFERENCES dependency_parent(code))",
        )
        .unwrap();

    assert_eq!(
        session
            .execute("ALTER TABLE dependency_parent DROP CONSTRAINT dependency_parent_pkey")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    session
        .execute(
            "ALTER TABLE dependency_parent DROP CONSTRAINT dependency_parent_pkey CASCADE; \
             INSERT INTO dependency_child VALUES (99, NULL); \
             INSERT INTO dependency_parent VALUES (1, 1)",
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO dependency_child VALUES (NULL, 99)")
            .unwrap_err()
            .sqlstate,
        SqlState::ForeignKeyViolation
    );
}

#[test]
fn adds_and_drops_owned_sequences_transactionally() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE serial_items (value INTEGER); \
             INSERT INTO serial_items VALUES (10), (20); \
             ALTER TABLE serial_items ADD COLUMN id SERIAL",
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT value, id FROM serial_items ORDER BY value"
        ),
        vec![
            vec![Value::Int4(10), Value::Int4(1)],
            vec![Value::Int4(20), Value::Int4(2)],
        ]
    );
    session
        .execute("INSERT INTO serial_items (value) VALUES (30)")
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT id FROM serial_items WHERE value = 30"),
        vec![vec![Value::Int4(3)]]
    );

    session.execute("BEGIN").unwrap();
    session
        .execute("ALTER TABLE serial_items DROP COLUMN id")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT nextval('serial_items_id_seq')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('serial_items_id_seq')"),
        vec![vec![Value::Int8(4)]]
    );
}
