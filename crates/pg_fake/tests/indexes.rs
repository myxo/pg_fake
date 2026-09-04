use pg_fake::{api::Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::api::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn creates_renames_and_drops_btree_indexes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE indexed (a INTEGER, b INTEGER, c INTEGER, d INTEGER, payload TEXT); \
             CREATE INDEX indexed_covering ON indexed \
               (a, b ASC, c DESC, d) INCLUDE (payload); \
             CREATE INDEX IF NOT EXISTS indexed_covering ON indexed (a); \
             ALTER INDEX indexed_covering RENAME TO indexed_renamed; \
             ALTER INDEX IF EXISTS missing_index RENAME TO ignored; \
             DROP INDEX indexed_renamed; \
             DROP INDEX IF EXISTS indexed_renamed; \
             CREATE TABLE public.qualified_index_table (id INTEGER); \
             CREATE INDEX public.qualified_index_name \
               ON public.qualified_index_table USING btree (id); \
             ALTER INDEX public.qualified_index_name RENAME TO qualified_index_renamed; \
             DROP INDEX public.qualified_index_renamed; \
             CREATE TEMP TABLE temp_index_table (id INTEGER); \
             CREATE INDEX pg_temp.temp_index_name ON pg_temp.temp_index_table (id); \
             DROP INDEX pg_temp.temp_index_name",
        )
        .unwrap();

    session
        .execute("CREATE INDEX indexed_covering ON indexed (a)")
        .unwrap();
    assert_eq!(
        session
            .execute("CREATE INDEX indexed_covering ON indexed (b)")
            .unwrap_err()
            .sqlstate,
        SqlState::DuplicateTable
    );
}

#[test]
fn enforces_unique_indexes_and_validates_existing_rows_atomically() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE accounts (tenant INTEGER, email TEXT, marker INTEGER); \
             INSERT INTO accounts VALUES (1, 'same', 1), (1, 'same', 2)",
        )
        .unwrap();

    assert_eq!(
        session
            .execute("CREATE UNIQUE INDEX account_key ON accounts (tenant, email)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    session
        .execute(
            "CREATE INDEX account_key ON accounts (marker); \
             DROP INDEX account_key; \
             UPDATE accounts SET email = 'other' WHERE marker = 2; \
             CREATE UNIQUE INDEX account_key ON accounts (tenant, email)",
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO accounts VALUES (1, 'same', 3)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    session
        .execute("INSERT INTO accounts VALUES (NULL, 'same', 3), (NULL, 'same', 4)")
        .unwrap();
    assert_eq!(
        session
            .execute("UPDATE accounts SET email = 'same' WHERE marker = 2")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
}

#[test]
fn applies_supported_partial_predicates_and_membership_transitions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE jobs (
               id INTEGER,
               active BOOLEAN,
               deleted_at INTEGER,
               state TEXT,
               priority INTEGER
             ); \
             CREATE UNIQUE INDEX jobs_active_key ON jobs (id) WHERE active; \
             CREATE INDEX jobs_deleted ON jobs (deleted_at) WHERE deleted_at IS NULL; \
             CREATE INDEX jobs_present ON jobs (deleted_at) WHERE deleted_at IS NOT NULL; \
             CREATE INDEX jobs_state ON jobs (state) WHERE state IN ('ready', 'running'); \
             CREATE INDEX jobs_filter ON jobs (priority) \
               WHERE (priority >= 10 AND state != 'done') OR priority < 0; \
             INSERT INTO jobs VALUES
               (1, true, NULL, 'ready', 10),
               (1, false, 1, 'done', 0),
               (2, NULL, NULL, 'running', -1)",
        )
        .unwrap();

    assert_eq!(
        session
            .execute("UPDATE jobs SET active = true WHERE id = 1 AND active = false")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    session
        .execute(
            "UPDATE jobs SET active = false WHERE id = 1 AND active = true; \
             UPDATE jobs SET active = true WHERE id = 1 AND deleted_at = 1",
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, active FROM jobs WHERE id = 1 ORDER BY deleted_at NULLS FIRST",
        ),
        vec![
            vec![Value::Int4(1), Value::Bool(false)],
            vec![Value::Int4(1), Value::Bool(true)],
        ]
    );
}

#[test]
fn infers_partial_unique_indexes_for_on_conflict() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE subscriptions (account_id INTEGER, active BOOLEAN, value INTEGER); \
             CREATE UNIQUE INDEX subscriptions_active_key \
               ON subscriptions (account_id) WHERE active; \
             INSERT INTO subscriptions VALUES (1, true, 10), (1, false, 20); \
             INSERT INTO subscriptions VALUES (1, true, 30) \
               ON CONFLICT (account_id) WHERE active \
               DO UPDATE SET value = excluded.value; \
             INSERT INTO subscriptions VALUES (2, false, 40) \
               ON CONFLICT (account_id) WHERE active DO NOTHING",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT account_id, active, value FROM subscriptions ORDER BY value",
        ),
        vec![
            vec![Value::Int4(1), Value::Bool(false), Value::Int4(20)],
            vec![Value::Int4(1), Value::Bool(true), Value::Int4(30)],
            vec![Value::Int4(2), Value::Bool(false), Value::Int4(40)],
        ]
    );
    assert_eq!(
        session
            .execute(
                "INSERT INTO subscriptions VALUES (1, true, 50) \
                 ON CONFLICT (account_id) DO NOTHING",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidColumnReference
    );
}

#[test]
fn follows_table_and_column_changes_and_transaction_rollback() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE mutable (key INTEGER, active BOOLEAN, payload TEXT); \
             CREATE UNIQUE INDEX mutable_key ON mutable (key) INCLUDE (payload) WHERE active; \
             ALTER TABLE mutable RENAME COLUMN key TO renamed_key; \
             ALTER TABLE mutable RENAME COLUMN active TO enabled; \
             ALTER TABLE mutable RENAME TO renamed_table; \
             INSERT INTO renamed_table VALUES (1, true, 'first')",
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO renamed_table VALUES (1, true, 'duplicate')")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );

    session
        .execute("BEGIN; DROP INDEX mutable_key; ROLLBACK")
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO renamed_table VALUES (1, true, 'duplicate')")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    session
        .execute(
            "BEGIN; CREATE INDEX rolled_back_creation ON renamed_table (enabled); ROLLBACK; \
             CREATE INDEX rolled_back_creation ON renamed_table (enabled); \
             DROP INDEX rolled_back_creation; \
             BEGIN; ALTER INDEX mutable_key RENAME TO rolled_back_name; ROLLBACK; \
             DROP INDEX mutable_key; \
             INSERT INTO renamed_table VALUES (1, true, 'allowed')",
        )
        .unwrap();

    session
        .execute(
            "CREATE INDEX renamed_dependency ON renamed_table (renamed_key) INCLUDE (payload); \
             ALTER TABLE renamed_table DROP COLUMN payload; \
             CREATE INDEX renamed_dependency ON renamed_table (enabled)",
        )
        .unwrap();
}

#[test]
fn reports_index_definition_and_namespace_errors() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE validation \
             (a INTEGER, b INTEGER, c INTEGER, d INTEGER, e INTEGER, flag BOOLEAN)",
        )
        .unwrap();

    for (sql, state) in [
        (
            "CREATE INDEX too_many ON validation (a, b, c, d, e)",
            SqlState::FeatureNotSupported,
        ),
        (
            "CREATE INDEX repeated ON validation (a) INCLUDE (a)",
            SqlState::DuplicateColumn,
        ),
        (
            "CREATE INDEX unknown_column ON validation (missing)",
            SqlState::UndefinedColumn,
        ),
        (
            "CREATE INDEX wrong_predicate ON validation (a) WHERE a",
            SqlState::DatatypeMismatch,
        ),
        (
            "CREATE INDEX expression_key ON validation ((a + b))",
            SqlState::FeatureNotSupported,
        ),
    ] {
        assert_eq!(session.execute(sql).unwrap_err().sqlstate, state, "{sql}");
    }

    session
        .execute("CREATE INDEX validation_index ON validation (a)")
        .unwrap();
    assert_eq!(
        session
            .execute("CREATE TABLE validation_index (id INTEGER)")
            .unwrap_err()
            .sqlstate,
        SqlState::DuplicateTable
    );
    assert_eq!(
        session
            .execute("DROP INDEX missing_index")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedObject
    );
}
