use std::{sync::mpsc, thread};

use crate::storage::Table;

use crate::{
    txn::{Snapshot, find_visible_version},
    value::BaseType,
};

use crate::executor::DatabaseState;

use super::{catalog_dependencies::CatalogDependency, prepared::can_cache_read_locks, *};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn wait_until_blocked(db: &Db) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if db.state.lock().unwrap().row_locks.has_waiters() {
            return;
        }
        assert!(Instant::now() < deadline, "transaction did not block");
        thread::yield_now();
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn wait_until_relation_blocked(db: &Db) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if db.state.lock().unwrap().relation_locks.has_waiters() {
            return;
        }
        assert!(Instant::now() < deadline, "transaction did not block");
        thread::yield_now();
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_affected_results(rows: u64) -> Vec<StatementResult> {
    vec![StatementResult::Affected(rows)]
}

#[test]
fn revalidates_prepared_dependencies_after_waiting_for_ddl() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    writer
        .execute("CREATE TABLE waited_schema (id INTEGER); INSERT INTO waited_schema VALUES (1)")
        .unwrap();
    let prepared = reader.prepare("SELECT id FROM waited_schema").unwrap();
    reader.query_prepared(&prepared, &[]).unwrap();
    writer
        .execute("BEGIN; ALTER TABLE waited_schema ADD COLUMN extra INTEGER")
        .unwrap();
    let waiting = thread::spawn(move || reader.query_prepared(&prepared, &[]));
    wait_until_relation_blocked(&db);
    writer.execute("COMMIT").unwrap();
    assert_eq!(
        waiting.join().unwrap().unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
fn reuses_prepared_queries_after_failed_catalog_changes_are_restored() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE restored_schema (id INTEGER); INSERT INTO restored_schema VALUES (1)",
        )
        .unwrap();
    let prepared = session.prepare("SELECT id FROM restored_schema").unwrap();
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    assert!(session.execute("ALTER TABLE restored_schema ADD COLUMN failed INTEGER DEFAULT 7, ADD COLUMN id INTEGER").is_err());
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    session
        .execute("ALTER TABLE restored_schema ADD COLUMN valid INTEGER")
        .unwrap();
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
fn excludes_nested_writes_and_sequence_calls_from_cached_read_locks() {
    for sql in [
        "WITH changed AS (INSERT INTO items VALUES (1) RETURNING id) SELECT * FROM changed",
        "SELECT * FROM (WITH changed AS (DELETE FROM items RETURNING id) SELECT * FROM changed) nested",
        "SELECT 1 UNION ALL (WITH changed AS (UPDATE items SET id = 2 RETURNING id) SELECT * FROM changed)",
        "SELECT (SELECT nextval($1))",
    ] {
        let statement = parser::parse(sql).unwrap().pop().unwrap();
        assert!(!can_cache_read_locks(&statement), "{sql}");
    }
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE SEQUENCE ids; CREATE VIEW generated_ids AS SELECT nextval('ids') AS id; CREATE VIEW indirect_ids AS SELECT * FROM generated_ids").unwrap();
    let prepared = session.prepare("SELECT * FROM indirect_ids").unwrap();
    assert!(prepared.relation_locks.is_none());
    let dynamic = session.prepare("SELECT nextval($1::text)").unwrap();
    assert!(dynamic.relation_locks.is_none());
    assert_eq!(
        session
            .query_prepared(&dynamic, &[Value::Text("ids".into())])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
    session
        .execute("CREATE SEQUENCE other_ids START 20")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&dynamic, &[Value::Text("other_ids".into())])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(20)]]
    );
}

#[test]
fn applies_and_restores_migration_local_timeouts() {
    let db = Db::create();
    let mut session = db.create_session();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(1));
    assert_eq!(session.settings.statement_timeout, Duration::ZERO);

    session.execute("BEGIN").unwrap();
    session.execute("SET LOCAL lock_timeout = '5s'").unwrap();
    session
        .execute("SET LOCAL statement_timeout = '30min'")
        .unwrap();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(5));
    assert_eq!(
        session.settings.statement_timeout,
        Duration::from_secs(30 * 60)
    );
    session.execute("COMMIT").unwrap();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(1));
    assert_eq!(session.settings.statement_timeout, Duration::ZERO);

    session.execute("BEGIN").unwrap();
    session.execute("SET lock_timeout = '2s'").unwrap();
    session.execute("SET LOCAL lock_timeout = '20ms'").unwrap();
    session.execute("COMMIT").unwrap();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(2));

    session.execute("BEGIN").unwrap();
    session.execute("SET lock_timeout = '3s'").unwrap();
    session
        .execute("SET LOCAL statement_timeout = '10ms'")
        .unwrap();
    session.execute("ROLLBACK").unwrap();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(2));
    assert_eq!(session.settings.statement_timeout, Duration::ZERO);

    session.execute("BEGIN").unwrap();
    session
        .execute("SET LOCAL statement_timeout = '1us'")
        .unwrap();
    assert_eq!(session.settings.statement_timeout, Duration::ZERO);
    session
        .execute("SET LOCAL statement_timeout = '1.5s'")
        .unwrap();
    assert_eq!(
        session.settings.statement_timeout,
        Duration::from_millis(1500)
    );
    session
        .execute("SET LOCAL statement_timeout = '0.5ms'")
        .unwrap();
    assert_eq!(session.settings.statement_timeout, Duration::ZERO);
    session
        .execute("SET LOCAL statement_timeout = '1.5'")
        .unwrap();
    assert_eq!(session.settings.statement_timeout, Duration::from_millis(2));
    session
        .execute("SET LOCAL statement_timeout = '1.0001min'")
        .unwrap();
    assert_eq!(session.settings.statement_timeout, Duration::from_secs(60));
    session
        .execute("SET LOCAL statement_timeout = '-0.5'")
        .unwrap();
    assert_eq!(session.settings.statement_timeout, Duration::ZERO);
    session
        .execute("SET LOCAL statement_timeout = '0x1d'")
        .unwrap();
    assert_eq!(
        session.settings.statement_timeout,
        Duration::from_millis(29)
    );
    session
        .execute("SET LOCAL statement_timeout = '0x1e'")
        .unwrap();
    assert_eq!(
        session.settings.statement_timeout,
        Duration::from_millis(30)
    );
    session.execute("ROLLBACK").unwrap();

    session.execute("BEGIN").unwrap();
    assert_eq!(
        session
            .execute("SET LOCAL statement_timeout = '1MS'")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidParameterValue
    );
    session.execute("ROLLBACK").unwrap();

    for timeout in ["08.5", "09e1"] {
        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .execute(&format!("SET LOCAL statement_timeout = '{timeout}'"))
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidParameterValue
        );
        session.execute("ROLLBACK").unwrap();
    }

    session.execute("BEGIN").unwrap();
    assert_eq!(
        session.execute("SELECT 1 / 0").unwrap_err().sqlstate,
        SqlState::DivisionByZero
    );
    assert_eq!(
        session
            .execute("SET LOCAL statement_timeout = '1s'")
            .unwrap_err()
            .sqlstate,
        SqlState::InFailedSqlTransaction
    );
    session.execute("ROLLBACK").unwrap();

    session.execute("SET statement_timeout = '1s'").unwrap();
    assert_eq!(session.settings.statement_timeout, Duration::from_secs(1));
    assert_eq!(
        session
            .execute("BEGIN; SET LOCAL lock_timeout = '25d'")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidParameterValue
    );
    session.execute("ROLLBACK").unwrap();
    session.execute("BEGIN").unwrap();
    session.settings.statement_timeout = Duration::from_nanos(1);
    assert_eq!(
        session.execute("DO $$ BEGIN END; $$").unwrap_err().sqlstate,
        SqlState::QueryCanceled
    );
    session.execute("ROLLBACK").unwrap();
}

#[test]
fn enforces_explicit_table_lock_compatibility_and_release() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute("CREATE TABLE public.items (id INTEGER)")
        .unwrap();

    assert_eq!(
        holder
            .execute("LOCK TABLE public.items IN EXCLUSIVE MODE")
            .unwrap_err()
            .sqlstate,
        SqlState::NoActiveSqlTransaction
    );
    holder.execute("BEGIN").unwrap();
    assert_eq!(
        holder
            .execute("LOCK TABLE public.items IN SHARE MODE")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    holder.execute("ROLLBACK").unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("LOCK TABLE public.items IN EXCLUSIVE MODE")
        .unwrap();
    assert!(reader.query("SELECT * FROM public.items", &[]).is_ok());

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute("INSERT INTO public.items VALUES (1)"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();

    let mut holder = db.create_session();
    let mut reader = db.create_session();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("LOCK TABLE public.items IN ACCESS EXCLUSIVE MODE")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(reader.query("SELECT * FROM public.items", &[]))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    holder.execute("ROLLBACK").unwrap();
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    handle.join().unwrap();
}

#[test]
fn distinguishes_statement_and_lock_timeouts() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    holder.execute("CREATE TABLE items (id INTEGER)").unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("LOCK TABLE items IN ACCESS EXCLUSIVE MODE")
        .unwrap();

    let mut statement_waiter = db.create_session();
    statement_waiter.execute("BEGIN").unwrap();
    statement_waiter
        .execute("SET LOCAL lock_timeout = '200ms'")
        .unwrap();
    statement_waiter
        .execute("SET LOCAL statement_timeout = '20ms'")
        .unwrap();
    assert_eq!(
        statement_waiter
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::QueryCanceled
    );
    statement_waiter.execute("ROLLBACK").unwrap();

    let mut lock_waiter = db.create_session();
    lock_waiter.execute("BEGIN").unwrap();
    lock_waiter
        .execute("SET LOCAL lock_timeout = '20ms'")
        .unwrap();
    lock_waiter
        .execute("SET LOCAL statement_timeout = '200ms'")
        .unwrap();
    assert_eq!(
        lock_waiter
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::LockNotAvailable
    );
    lock_waiter.execute("ROLLBACK").unwrap();
    holder.execute("ROLLBACK").unwrap();
}

#[test]
fn resets_statement_timeout_for_each_statement() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::ZERO)
        .build();
    let mut setup = db.create_session();
    setup
        .execute("CREATE TABLE first_table (id INTEGER); CREATE TABLE second_table (id INTEGER)")
        .unwrap();
    let mut waiter = db.create_session();
    waiter.execute("BEGIN").unwrap();
    waiter
        .execute("SET LOCAL statement_timeout = '250ms'")
        .unwrap();

    let mut first_holder = db.create_session();
    first_holder.execute("BEGIN").unwrap();
    first_holder
        .execute("LOCK TABLE first_table IN ACCESS EXCLUSIVE MODE")
        .unwrap();
    let mut second_holder = db.create_session();
    second_holder.execute("BEGIN").unwrap();
    second_holder
        .execute("LOCK TABLE second_table IN ACCESS EXCLUSIVE MODE")
        .unwrap();

    let (first_sender, first_receiver) = mpsc::channel();
    let (second_sender, second_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        waiter.query("SELECT * FROM first_table", &[]).unwrap();
        first_sender.send(()).unwrap();
        let result = waiter.query("SELECT * FROM second_table", &[]);
        waiter.execute("ROLLBACK").unwrap();
        second_sender.send(result).unwrap();
    });

    wait_until_relation_blocked(&db);
    thread::sleep(Duration::from_millis(150));
    first_holder.execute("ROLLBACK").unwrap();
    first_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    wait_until_relation_blocked(&db);
    thread::sleep(Duration::from_millis(150));
    second_holder.execute("ROLLBACK").unwrap();
    assert!(
        second_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    handle.join().unwrap();
}

#[test]
fn clears_relation_waits_when_a_waited_relation_is_dropped() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_millis(100))
        .build();
    let mut dropper = db.create_session();
    dropper.execute("CREATE TABLE items (id INTEGER)").unwrap();
    dropper.execute("BEGIN; DROP TABLE items").unwrap();

    let mut waiter = db.create_session();
    waiter.execute("BEGIN").unwrap();
    let (result_sender, result_receiver) = mpsc::channel();
    let (finish_sender, finish_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(
                waiter
                    .execute("LOCK TABLE items IN ACCESS EXCLUSIVE MODE")
                    .unwrap_err()
                    .sqlstate,
            )
            .unwrap();
        finish_receiver.recv().unwrap();
        waiter.execute("ROLLBACK").unwrap();
    });

    wait_until_relation_blocked(&db);
    dropper.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        SqlState::UndefinedTable
    );
    let mut creator = db.create_session();
    creator.execute("CREATE TABLE items (id INTEGER)").unwrap();
    finish_sender.send(()).unwrap();
    handle.join().unwrap();
}

#[test]
fn interrupts_prepared_scans_at_the_statement_deadline() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();
    let query = session
        .prepare("SELECT id / ($1 - id) FROM items WHERE id <= $1")
        .unwrap();
    session.execute("BEGIN").unwrap();
    session.settings.statement_timeout = Duration::from_nanos(1);
    assert_eq!(
        session
            .query_prepared(&query, &[Value::Int4(1)])
            .unwrap_err()
            .sqlstate,
        SqlState::QueryCanceled
    );
    session.execute("ROLLBACK").unwrap();
}

#[test]
fn applies_one_statement_deadline_to_a_do_block() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("BEGIN").unwrap();
    session.settings.statement_timeout = Duration::from_nanos(1);
    assert_eq!(
        session
            .execute(
                "DO $$ DECLARE value BIGINT := 1; \
                     BEGIN value := value + 1; END; $$"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::QueryCanceled
    );
    session.execute("ROLLBACK").unwrap();
}

#[test]
fn acquires_multi_table_lock_sets_atomically() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut setup = db.create_session();
    setup
        .execute("CREATE TABLE first_table (id INTEGER); CREATE TABLE second_table (id INTEGER)")
        .unwrap();

    let mut blocker = db.create_session();
    blocker.execute("BEGIN").unwrap();
    blocker
        .execute("LOCK TABLE second_table IN ACCESS EXCLUSIVE MODE")
        .unwrap();

    let mut waiter = db.create_session();
    waiter.execute("BEGIN").unwrap();
    waiter.execute("SET LOCAL lock_timeout = '100ms'").unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let sqlstate = waiter
            .execute("LOCK TABLE first_table, second_table IN ACCESS EXCLUSIVE MODE")
            .unwrap_err()
            .sqlstate;
        waiter.execute("ROLLBACK").unwrap();
        sender.send(sqlstate).unwrap();
    });
    wait_until_relation_blocked(&db);

    let mut independent = db.create_session();
    independent.execute("BEGIN").unwrap();
    independent
        .execute("LOCK TABLE first_table IN ACCESS EXCLUSIVE MODE")
        .unwrap();
    independent.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        SqlState::LockNotAvailable
    );
    handle.join().unwrap();
    blocker.execute("ROLLBACK").unwrap();
}

#[test]
fn detects_deadlocks_in_explicit_table_locks() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE first_table (id INTEGER); CREATE TABLE second_table (id INTEGER)")
        .unwrap();
    first.execute("BEGIN").unwrap();
    second.execute("BEGIN").unwrap();
    first
        .execute("LOCK TABLE first_table IN ACCESS EXCLUSIVE MODE")
        .unwrap();
    second
        .execute("LOCK TABLE second_table IN ACCESS EXCLUSIVE MODE")
        .unwrap();

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let result = first.execute("LOCK TABLE second_table IN ACCESS EXCLUSIVE MODE");
        sender.send(result).unwrap();
    });
    wait_until_relation_blocked(&db);
    assert_eq!(
        second
            .execute("LOCK TABLE first_table IN ACCESS EXCLUSIVE MODE")
            .unwrap_err()
            .sqlstate,
        SqlState::DeadlockDetected
    );
    second.execute("ROLLBACK").unwrap();
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    handle.join().unwrap();
}

#[test]
fn strengthens_foreign_key_reads_against_explicit_locks() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY); \
                 CREATE TABLE children (parent_id INTEGER REFERENCES parents); \
                 INSERT INTO parents VALUES (1)",
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("LOCK TABLE parents IN EXCLUSIVE MODE")
        .unwrap();

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute("INSERT INTO children SELECT id FROM parents"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    holder.execute("ROLLBACK").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn renames_trigger_catalog_identity_transactionally() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE accounts (id INTEGER); \
                 CREATE FUNCTION audit_changes() RETURNS TRIGGER AS $$ \
                 BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql; \
                 CREATE TRIGGER audit_changes BEFORE INSERT ON accounts \
                 FOR EACH ROW EXECUTE FUNCTION audit_changes()",
        )
        .unwrap();
    let trigger_id = db
        .state
        .lock()
        .unwrap()
        .catalog
        .require_table("accounts")
        .unwrap()
        .triggers[0]
        .id;

    session
        .execute(
            "BEGIN; \
                 ALTER TRIGGER audit_changes ON accounts RENAME TO audit_accounts; \
                 ROLLBACK",
        )
        .unwrap();
    session
        .execute("ALTER TRIGGER audit_changes ON accounts RENAME TO audit_accounts")
        .unwrap();
    let (renamed_id, renamed_name, definition_name) = {
        let state = db.state.lock().unwrap();
        let trigger = &state.catalog.require_table("accounts").unwrap().triggers[0];
        (
            trigger.id,
            trigger.name.clone(),
            executor::normalize_unqualified_object_name(&trigger.definition.name).unwrap(),
        )
    };
    assert_eq!(renamed_id, trigger_id);
    assert_eq!(renamed_name, "audit_accounts");
    assert_eq!(definition_name, "audit_accounts");
    assert_eq!(
        session
            .execute("ALTER TRIGGER audit_changes ON accounts RENAME TO ignored")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedObject
    );
    assert_eq!(
        session.execute("INSERT INTO accounts VALUES (1)").unwrap(),
        create_affected_results(1)
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn enforces_foreign_keys_and_keeps_failed_multi_row_writes_atomic() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    session
        .execute(
            "CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents)",
        )
        .unwrap();
    let error = session
        .execute("INSERT INTO children VALUES (1, 99), (2, 99)")
        .unwrap_err();
    assert_eq!(error.sqlstate, SqlState::ForeignKeyViolation);
    assert!(
        session
            .query("SELECT * FROM children", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    session.execute("INSERT INTO parents VALUES (99)").unwrap();
    session
        .execute("INSERT INTO children VALUES (1, 99)")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT parent_id FROM children", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(99)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn applies_foreign_key_actions_to_updates_and_deletes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY, replacement INTEGER)")
        .unwrap();
    session.execute("CREATE TABLE cascade_children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE CASCADE ON UPDATE CASCADE)").unwrap();
    session.execute("CREATE TABLE null_children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE SET NULL ON UPDATE CASCADE)").unwrap();
    session.execute("CREATE TABLE default_children (id INTEGER PRIMARY KEY, parent_id INTEGER DEFAULT 7 REFERENCES parents(id) ON DELETE SET DEFAULT ON UPDATE CASCADE)").unwrap();
    session
        .execute("INSERT INTO parents VALUES (7, NULL), (1, NULL)")
        .unwrap();
    session
        .execute("INSERT INTO cascade_children VALUES (1, 1)")
        .unwrap();
    session
        .execute("INSERT INTO null_children VALUES (1, 1)")
        .unwrap();
    session
        .execute("INSERT INTO default_children VALUES (1, 1)")
        .unwrap();
    session
        .execute("UPDATE parents SET id = 2 WHERE id = 1")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT parent_id FROM cascade_children", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
    session.execute("DELETE FROM parents WHERE id = 2").unwrap();
    assert!(
        session
            .query("SELECT * FROM cascade_children", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        session
            .query("SELECT parent_id FROM null_children", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        session
            .query("SELECT parent_id FROM default_children", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(7)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validates_deferred_foreign_keys_at_commit_and_allows_repairs() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    session.execute("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER CONSTRAINT children_parent_fkey REFERENCES parents DEFERRABLE INITIALLY DEFERRED)").unwrap();
    session.execute("BEGIN").unwrap();
    session
        .execute("INSERT INTO children VALUES (1, 2)")
        .unwrap();
    session.execute("INSERT INTO parents VALUES (2)").unwrap();
    session.execute("COMMIT").unwrap();
    session.execute("BEGIN").unwrap();
    session
        .execute("INSERT INTO children VALUES (3, 4)")
        .unwrap();
    let error = session.execute("COMMIT").unwrap_err();
    assert_eq!(error.sqlstate, SqlState::ForeignKeyViolation);
    assert_eq!(
        session.query("SELECT id FROM children", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn set_constraints_changes_deferrable_foreign_key_timing() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    session.execute("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER CONSTRAINT children_parent_fkey REFERENCES parents DEFERRABLE)").unwrap();
    session.execute("BEGIN").unwrap();
    session
        .execute("SET CONSTRAINTS children_parent_fkey DEFERRED")
        .unwrap();
    session
        .execute("INSERT INTO children VALUES (1, 2)")
        .unwrap();
    session.execute("INSERT INTO parents VALUES (2)").unwrap();
    session.execute("SET CONSTRAINTS ALL IMMEDIATE").unwrap();
    session.execute("COMMIT").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn accepts_self_references_and_match_simple_nulls() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE nodes (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES nodes)")
        .unwrap();
    session.execute("INSERT INTO nodes VALUES (1, 1)").unwrap();
    session
            .execute("CREATE TABLE parents (first_id INTEGER, second_id INTEGER, PRIMARY KEY (first_id, second_id))")
            .unwrap();
    session
            .execute("CREATE TABLE children (id INTEGER PRIMARY KEY, first_id INTEGER, second_id INTEGER, FOREIGN KEY (first_id, second_id) REFERENCES parents(first_id, second_id))")
            .unwrap();
    session
        .execute("INSERT INTO children VALUES (1, NULL, 2), (2, 1, NULL), (3, NULL, NULL)")
        .unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parses_compares_and_generates_uuid_values() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id UUID PRIMARY KEY)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES ('{A0EEBC99-9C0B-4EF8-BBA9-6A6C0F3B0AF7}')")
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT id FROM items WHERE id = 'a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7'",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Uuid(
            uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7").unwrap()
        )]]
    );
    let generated = session
        .query("SELECT gen_random_uuid(), uuidv4() FROM items", &[])
        .unwrap();
    assert!(matches!(generated.rows[0][0], Value::Uuid(_)));
    assert_ne!(generated.rows[0][0], generated.rows[0][1]);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reproduces_seeded_uuid_generation_and_supports_v7() {
    let initial = chrono::DateTime::parse_from_rfc3339("2024-02-29T12:34:56Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let generate = |db: &Db| {
        db.set_time(initial).unwrap();
        let mut session = db.create_session();
        session.execute("CREATE TABLE source (id INTEGER)").unwrap();
        session.execute("INSERT INTO source VALUES (1)").unwrap();
        session
            .query(
                "SELECT gen_random_uuid(), uuidv4(), uuidv7() FROM source",
                &[],
            )
            .unwrap()
            .rows
    };
    let first = generate(
        &Db::create_builder()
            .set_mock_time_enabled(true)
            .set_random_seed(42)
            .build(),
    );
    let second = generate(
        &Db::create_builder()
            .set_mock_time_enabled(true)
            .set_random_seed(42)
            .build(),
    );
    assert_eq!(first, second);
    let Value::Uuid(v4) = first[0][0] else {
        panic!("uuid generator must return uuid")
    };
    let Value::Uuid(v7) = first[0][2] else {
        panic!("uuidv7 must return uuid")
    };
    assert_eq!(v4.get_version(), Some(uuid::Version::Random));
    assert_eq!(v7.get_version(), Some(uuid::Version::SortRand));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn processes_timestamp_values_and_timezone_setting() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE events (plain TIMESTAMP(3), instant TIMESTAMPTZ)")
        .unwrap();
    session
        .execute(
            "INSERT INTO events VALUES ('2024-02-29 12:34:56.789123', '2024-02-29T12:34:56+03:00')",
        )
        .unwrap();
    let result = session
        .query("SELECT plain, instant FROM events", &[])
        .unwrap();
    assert_eq!(
        result.columns[0].type_oid,
        crate::value::BaseType::Timestamp.map_to_oid()
    );
    assert_eq!(
        result.columns[1].type_oid,
        crate::value::BaseType::TimestampTz.map_to_oid()
    );
    assert_eq!(
        result.rows[0][0].format_postgres_text(),
        "2024-02-29 12:34:56.789"
    );
    assert_eq!(
        result.rows[0][1].format_postgres_text(),
        "2024-02-29 09:34:56+00"
    );
    session.execute("SET TIME ZONE 'UTC'").unwrap();
    assert_eq!(
        session.query("SHOW TimeZone", &[]).unwrap().rows,
        vec![vec![Value::Text("UTC".into())]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_interval_calendar_and_clock_parts() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE events (started TIMESTAMP, duration INTERVAL)")
        .unwrap();
    session
        .execute("INSERT INTO events VALUES ('2024-01-31 12:00:00', '1 month 2 days 03:04:05')")
        .unwrap();
    let result = session
        .query("SELECT started + duration, duration * 2 FROM events", &[])
        .unwrap();
    assert_eq!(
        result.columns[0].type_oid,
        crate::value::BaseType::Timestamp.map_to_oid()
    );
    assert_eq!(
        result.columns[1].type_oid,
        crate::value::BaseType::Interval.map_to_oid()
    );
    assert_eq!(
        result.rows[0][0].format_postgres_text(),
        "2024-03-02 15:04:05"
    );
    assert_eq!(
        result.rows[0][1].format_postgres_text(),
        "2 mons 4 days 06:08:10"
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn freezes_and_controls_mock_clock() {
    let db = Db::create_builder().set_mock_time_enabled(true).build();
    let initial = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    db.set_time(initial).unwrap();
    assert_eq!(db.read_clock(), initial);
    db.advance_time(chrono::Duration::minutes(90)).unwrap();
    assert_eq!(db.read_clock(), initial + chrono::Duration::minutes(90));
    assert!(Db::create().set_time(initial).is_err());
    assert!(
        Db::create()
            .advance_time(chrono::Duration::seconds(1))
            .is_err()
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn observes_timestamp_function_boundaries() {
    let db = Db::create_builder().set_mock_time_enabled(true).build();
    let initial = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    db.set_time(initial).unwrap();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE clock_source (id INTEGER)")
        .unwrap();
    session
        .execute("INSERT INTO clock_source VALUES (1)")
        .unwrap();
    session.execute("BEGIN").unwrap();
    let clock = session
        .prepare("SELECT now(), statement_timestamp(), clock_timestamp() FROM clock_source")
        .unwrap();
    let scan = session.prepare("SELECT id FROM clock_source").unwrap();
    let first = session.query_prepared(&clock, &[]).unwrap();
    db.advance_time(chrono::Duration::seconds(1)).unwrap();
    assert_eq!(
        session.query_prepared(&scan, &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    let second = session.query_prepared(&clock, &[]).unwrap();
    assert_eq!(first.rows[0][0], second.rows[0][0]);
    assert_ne!(first.rows[0][1], second.rows[0][1]);
    assert_ne!(first.rows[0][2], second.rows[0][2]);
    session.execute("COMMIT").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_postgres_date_and_time_special_forms() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE values_table (day DATE, moment TIME(6))")
        .unwrap();
    session
        .execute("INSERT INTO values_table VALUES ('infinity', '24:00:00')")
        .unwrap();
    let result = session
        .query("SELECT day, moment FROM values_table", &[])
        .unwrap();
    assert_eq!(result.rows[0][0].format_postgres_text(), "infinity");
    assert_eq!(result.rows[0][1].format_postgres_text(), "24:00:00");
    assert_eq!(result.columns[0].type_oid, BaseType::Date.map_to_oid());
    assert_eq!(result.columns[1].type_oid, BaseType::Time.map_to_oid());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rejects_partially_null_match_full_keys() {
    let db = Db::create();
    let mut session = db.create_session();
    session
            .execute("CREATE TABLE parents (first_id INTEGER, second_id INTEGER, PRIMARY KEY (first_id, second_id))")
            .unwrap();
    session
            .execute("CREATE TABLE children (id INTEGER PRIMARY KEY, first_id INTEGER, second_id INTEGER, FOREIGN KEY (first_id, second_id) REFERENCES parents(first_id, second_id) MATCH FULL)")
            .unwrap();
    session
        .execute("INSERT INTO children VALUES (1, NULL, NULL)")
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO children VALUES (2, NULL, 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::ForeignKeyViolation
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn creates_and_drops_tables_in_autocommit() {
    let db = Db::create();
    let mut session = db.create_session();
    assert_eq!(
        session
            .execute(
                "CREATE TABLE items (id INTEGER NOT NULL, name VARCHAR(12), amount NUMERIC(8, 2))"
            )
            .unwrap(),
        create_affected_results(0)
    );
    let state = db.state.lock().unwrap();
    let table = state.catalog.require_table("items").unwrap();
    assert_eq!(table.columns[0].data_type.base, BaseType::Int4);
    assert_eq!(table.columns[1].data_type.typmod, 16);
    assert_eq!(table.columns[2].data_type.typmod, (8 << 16) + 2 + 4);
    drop(state);
    assert_eq!(
        session.execute("DROP TABLE items").unwrap(),
        create_affected_results(0)
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolves_qualified_and_temporary_relations_per_session() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE public.items (id INTEGER); INSERT INTO public.items VALUES (1)")
        .unwrap();
    first
        .execute("CREATE TEMP TABLE items (id INTEGER) ON COMMIT PRESERVE ROWS")
        .unwrap();
    second
        .execute("CREATE TEMPORARY TABLE pg_temp.items (id INTEGER)")
        .unwrap();
    first.execute("INSERT INTO items VALUES (2)").unwrap();
    second.execute("INSERT INTO items VALUES (3)").unwrap();

    assert_eq!(
        first.query("SELECT id FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(2)]]
    );
    assert_eq!(
        first
            .query("SELECT id FROM public.items", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        second
            .query("SELECT id FROM pg_temp.items", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(3)]]
    );
    assert_eq!(
        first
            .query(
                "WITH first_value AS (SELECT id FROM items), \
                     items AS (SELECT 99 AS id) SELECT id FROM first_value",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn applies_temporary_relation_transaction_and_session_lifetimes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TEMP TABLE retained (id INTEGER); INSERT INTO retained VALUES (1)")
        .unwrap();
    assert_eq!(
        session.query("SELECT id FROM retained", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    {
        let mut transaction = session.begin().unwrap();
        transaction.execute("DROP TABLE retained").unwrap();
        transaction.rollback().unwrap();
    }
    assert_eq!(
        session.query("SELECT id FROM retained", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    {
        let mut transaction = session.begin().unwrap();
        transaction
            .execute("CREATE TEMP TABLE fleeting (id INTEGER) ON COMMIT DROP")
            .unwrap();
        transaction
            .execute("INSERT INTO fleeting VALUES (2)")
            .unwrap();
        transaction.commit().unwrap();
    }
    assert_eq!(
        session
            .query("SELECT * FROM pg_temp.fleeting", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    drop(session);

    let mut replacement = db.create_session();
    assert_eq!(
        replacement
            .query("SELECT * FROM pg_temp.retained", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    replacement
        .execute("CREATE TEMP TABLE retained (id INTEGER)")
        .unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn keeps_qualified_prepared_relations_and_sequences_stable() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE public.items (id INTEGER); \
                 INSERT INTO public.items VALUES (1); \
                 CREATE TABLE public.generated (id SERIAL); \
                 CREATE TABLE public.owned (id SERIAL); \
                 CREATE SEQUENCE public.ids START WITH 10; \
                 CREATE TEMP SEQUENCE ids START WITH 20; \
                 CREATE TEMP SEQUENCE generated_id_seq START WITH 100; \
                 CREATE TEMP TABLE owned (id SERIAL)",
        )
        .unwrap();
    let prepared = session.prepare("SELECT id FROM public.items").unwrap();
    session
        .execute("CREATE TEMP TABLE items (id INTEGER); INSERT INTO items VALUES (2)")
        .unwrap();
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        session.query("SELECT nextval('ids')", &[]).unwrap().rows,
        vec![vec![Value::Int8(20)]]
    );
    assert_eq!(
        session
            .query("SELECT nextval('public.ids')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(10)]]
    );
    session
        .execute("INSERT INTO public.generated DEFAULT VALUES")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT id FROM public.generated", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        session
            .query("SELECT nextval('generated_id_seq')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(100)]]
    );
    assert_eq!(
        session
            .query(
                "SELECT pg_get_serial_sequence('public.owned', 'id'), \
                            pg_get_serial_sequence('pg_temp.owned', 'id')",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Text("public.owned_id_seq".into()),
            Value::Text("pg_temp.owned_id_seq".into()),
        ]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn prevents_prepared_statements_from_retargeting_temporary_shadows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE public.items (id INTEGER); \
                 CREATE TABLE public.dropped (id INTEGER); \
                 CREATE SEQUENCE public.ids START WITH 10",
        )
        .unwrap();
    let insert = session.prepare("INSERT INTO items VALUES (1)").unwrap();
    let aggregate = session.prepare("SELECT count(*) FROM items").unwrap();
    let drop_table = session.prepare("DROP TABLE dropped").unwrap();
    let next_value = session.prepare("SELECT nextval('ids')").unwrap();
    let cast_next_value = session.prepare("SELECT nextval('ids'::text)").unwrap();
    session
        .execute(
            "CREATE TEMP TABLE items (id INTEGER); \
                 CREATE TEMP TABLE dropped (id INTEGER); \
                 CREATE TEMP SEQUENCE ids START WITH 20",
        )
        .unwrap();

    assert_eq!(
        session.execute_prepared(&insert, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .query_prepared(&aggregate, &[])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute_prepared(&drop_table, &[])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .query_prepared(&next_value, &[])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .query_prepared(&cast_next_value, &[])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert!(session.query("SELECT * FROM public.dropped", &[]).is_ok());
    assert!(session.query("SELECT * FROM pg_temp.dropped", &[]).is_ok());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn binds_explicit_sequence_defaults_before_temporary_shadowing() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE SEQUENCE public.ids; \
                 CREATE TABLE public.generated (id BIGINT DEFAULT nextval('ids')); \
                 CREATE TEMP SEQUENCE ids START WITH 100",
        )
        .unwrap();
    assert_eq!(
        session
            .query(
                "INSERT INTO public.generated DEFAULT VALUES RETURNING id",
                &[]
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
    assert_eq!(
        session
            .execute("DROP SEQUENCE public.ids")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    assert_eq!(
        session
            .execute("CREATE TABLE missing_default (id BIGINT DEFAULT nextval('missing'))")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        session
            .execute("CREATE TABLE wrong_kind (id BIGINT DEFAULT nextval('generated'))")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute("CREATE TABLE permanent_temp_default (id BIGINT DEFAULT nextval('ids'))")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute(
                "CREATE TEMP TABLE temporary_public_default \
                     (id BIGINT DEFAULT nextval('public.ids'))",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute(
                "CREATE TABLE compound_default \
                     (id BIGINT DEFAULT nextval('public.ids') + 1)",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute(
                "CREATE TABLE cast_default \
                     (id BIGINT DEFAULT nextval('public.ids')::smallint)",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute(
                "CREATE TABLE currval_default \
                     (id BIGINT DEFAULT currval('public.ids'))",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute(
                "CREATE TABLE setval_default \
                     (id BIGINT DEFAULT setval('public.ids', 10))",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rejects_unsupported_relation_namespaces_explicitly() {
    let db = Db::create();
    let mut session = db.create_session();
    assert_eq!(
        session
            .execute("CREATE TABLE private.items (id INTEGER)")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidSchemaName
    );
    assert_eq!(
        session
            .execute("CREATE TEMP TABLE public.items (id INTEGER)")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTableDefinition
    );
    session.execute("SET search_path TO public").unwrap();
    assert_eq!(
        session
            .query("SELECT pg_get_serial_sequence('private.items', 'id')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidSchemaName
    );
    assert_eq!(
        session
            .query("SELECT pg_get_serial_sequence('public.missing', 'id')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    session
        .execute("CREATE SEQUENCE public.not_a_table")
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT pg_get_serial_sequence('public.not_a_table', 'id')",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedColumn
    );
    session
        .execute("CREATE TEMP TABLE temporary_parent (id INTEGER PRIMARY KEY)")
        .unwrap();
    assert_eq!(
        session
            .execute(
                "CREATE TABLE public.permanent_child \
                     (parent_id INTEGER REFERENCES temporary_parent(id))",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTableDefinition
    );
    session
        .execute("CREATE TABLE public.permanent_parent (id INTEGER PRIMARY KEY)")
        .unwrap();
    assert_eq!(
        session
            .execute(
                "CREATE TEMP TABLE temporary_child \
                     (parent_id INTEGER REFERENCES public.permanent_parent(id))",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTableDefinition
    );
    assert_eq!(
        session
            .execute(
                "CREATE SEQUENCE public.cross_owned \
                     OWNED BY temporary_parent.id",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::ObjectNotInPrerequisiteState
    );
    assert_eq!(
        session
            .execute(
                "CREATE TEMP SEQUENCE cross_owned \
                     OWNED BY public.permanent_parent.id",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::ObjectNotInPrerequisiteState
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn selects_projections_with_metadata_in_row_id_order() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, name TEXT)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (2, 'second'), (1, 'first')")
        .unwrap();

    let result = session.query("SELECT name, id FROM items", &[]).unwrap();

    assert_eq!(
        result.columns,
        vec![
            ColumnMeta {
                name: "name".into(),
                type_oid: BaseType::Text.map_to_oid(),
                typmod: -1,
            },
            ColumnMeta {
                name: "id".into(),
                type_oid: BaseType::Int4.map_to_oid(),
                typmod: -1,
            },
        ]
    );
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Text("second".into()), Value::Int4(2)],
            vec![Value::Text("first".into()), Value::Int4(1)],
        ]
    );
    let all_columns = session.query("SELECT * FROM items", &[]).unwrap();
    assert_eq!(
        all_columns.rows,
        vec![
            vec![Value::Int4(2), Value::Text("second".into())],
            vec![Value::Int4(1), Value::Text("first".into())],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn binds_typed_parameters_and_prepared_statements() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, name TEXT, amount SMALLINT)")
        .unwrap();

    let insert = session
        .prepare("INSERT INTO items VALUES ($1, $2, $3)")
        .unwrap();
    assert_eq!(
        session.execute_prepared(
            &insert,
            &[Value::Int4(1), Value::Text("first".into()), Value::Int2(10),],
        ),
        Ok(1)
    );
    assert_eq!(
        session.execute_prepared(
            &insert,
            &[
                Value::Int4(2),
                Value::Text("second".into()),
                Value::Int2(20),
            ],
        ),
        Ok(1)
    );
    assert_eq!(
        session.execute_params(
            "UPDATE items SET amount = $1 WHERE id = $2",
            &[Value::Int2(11), Value::Int4(1)],
        ),
        Ok(1)
    );

    let select = session
        .prepare("SELECT name, amount FROM items WHERE id = $1")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&select, &[Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Text("first".into()), Value::Int2(11)]]
    );
    assert_eq!(
        session
            .query_prepared(&select, &[Value::Int4(2)])
            .unwrap()
            .rows,
        vec![vec![Value::Text("second".into()), Value::Int2(20)]]
    );
    assert!(
        session
            .query(
                "SELECT id FROM items WHERE name = $1 AND amount = $2",
                &[Value::Text("missing".into()), Value::Null],
            )
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_comparison_coercion_for_point_lookup_candidates() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE indexed_smallints (id SMALLINT PRIMARY KEY); \
                 CREATE TABLE scanned_smallints (id SMALLINT); \
                 CREATE TABLE indexed_integers (id INTEGER PRIMARY KEY); \
                 INSERT INTO indexed_smallints VALUES (1); \
                 INSERT INTO scanned_smallints VALUES (1); \
                 INSERT INTO indexed_integers VALUES (1)",
        )
        .unwrap();

    assert_eq!(
        session
            .query("SELECT id FROM indexed_smallints WHERE id = 1", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int2(1)]]
    );
    assert_eq!(
        session
            .query("SELECT id FROM scanned_smallints WHERE id = 1", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int2(1)]]
    );
    assert_eq!(
        session
            .query("SELECT id FROM indexed_integers WHERE id = 1.0", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn skips_scans_for_missing_prepared_unique_keys() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE SEQUENCE point_probe START WITH 1; \
                 CREATE TABLE items (id INTEGER PRIMARY KEY); \
                 INSERT INTO items VALUES (1), (2)",
        )
        .unwrap();
    let statement = session
        .prepare(
            "SELECT id FROM items \
                 WHERE nextval('point_probe') > 0 AND id = $1",
        )
        .unwrap();

    assert!(
        session
            .query_prepared(&statement, &[Value::Int4(99)])
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        session
            .query("SELECT nextval('point_probe')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn finishes_implicit_prepared_transactions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();
    let insert = session.prepare("INSERT INTO items VALUES ($1)").unwrap();

    assert_eq!(session.execute_prepared(&insert, &[Value::Int4(1)]), Ok(1));
    assert!(session.transaction.is_none());
    assert_eq!(
        session
            .execute_prepared(&insert, &[Value::Int4(1)])
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    assert!(session.transaction.is_none());
    assert_eq!(session.execute_prepared(&insert, &[Value::Int4(2)]), Ok(1));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn matches_prepared_statement_parameter_contract() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();
    let skipped = session
        .prepare("SELECT id FROM items WHERE id = $2 OR id = $2")
        .unwrap();

    assert_eq!(
        session
            .query_prepared(&skipped, &[Value::Text("unused".into())])
            .unwrap_err()
            .sqlstate,
        SqlState::ProtocolViolation
    );
    assert_eq!(
        session
            .query_prepared(
                &skipped,
                &[Value::Text("unused".into()), Value::Text("wrong".into())],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::CannotCoerce
    );
    assert_eq!(
        session
            .query_prepared(&skipped, &[Value::Text("unused".into()), Value::Int4(1)],)
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        session
            .query("SELECT id FROM items WHERE id = $1", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::ProtocolViolation
    );
    assert_eq!(
        session
            .execute_params(
                "INSERT INTO items VALUES ($1); INSERT INTO items VALUES ($1)",
                &[Value::Int4(2)],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::SyntaxError
    );
    assert_eq!(
        session
            .prepare("SELECT missing FROM items WHERE id = $1")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedColumn
    );
    assert_eq!(
        session
            .prepare("SELECT id + TRUE FROM items WHERE id = $1")
            .unwrap_err()
            .sqlstate,
        SqlState::DatatypeMismatch
    );
    assert_eq!(
        session
            .prepare("SELECT id FROM missing WHERE id = $1")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        session
            .prepare("SELECT id FROM items WHERE id = $0")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedParameter
    );

    let prepared = session
        .prepare("SELECT id FROM items WHERE id = $1")
        .unwrap();
    session.execute("DROP TABLE items").unwrap();
    assert_eq!(
        session
            .query_prepared(&prepared, &[Value::Int4(1)])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn execute_returns_each_multi_statement_result() {
    let db = Db::create();
    let mut session = db.create_session();

    let results = session
        .execute(
            "CREATE TABLE items (id INTEGER, name TEXT); \
                 INSERT INTO items VALUES (1, 'one'), (2, 'two'); \
                 UPDATE items SET name = upper(name) WHERE id = 2; \
                 SELECT id, name FROM items ORDER BY id",
        )
        .unwrap();

    assert_eq!(
        results,
        vec![
            StatementResult::Affected(0),
            StatementResult::Affected(2),
            StatementResult::Affected(1),
            StatementResult::Query(QueryResult {
                columns: vec![
                    ColumnMeta {
                        name: "id".into(),
                        type_oid: BaseType::Int4.map_to_oid(),
                        typmod: -1,
                    },
                    ColumnMeta {
                        name: "name".into(),
                        type_oid: BaseType::Text.map_to_oid(),
                        typmod: -1,
                    },
                ],
                rows: vec![
                    vec![Value::Int4(1), Value::Text("one".into())],
                    vec![Value::Int4(2), Value::Text("TWO".into())],
                ],
            }),
        ]
    );
    assert!(session.execute(" ; ; ").unwrap().is_empty());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rolls_back_implicit_batches_at_first_error() {
    let db = Db::create();
    let mut session = db.create_session();
    let original_timeout = session.settings.lock_timeout;
    assert_eq!(
        session
            .execute(
                "SET lock_timeout = '2s'; \
                     CREATE TABLE discarded (id INTEGER); \
                     INSERT INTO discarded VALUES (1); \
                     INSERT INTO discarded VALUES ('bad'); \
                     INSERT INTO discarded VALUES (2)",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    assert_eq!(session.settings.lock_timeout, original_timeout);
    assert_eq!(
        session
            .query("SELECT * FROM discarded", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );

    session.execute("CREATE TABLE kept (id INTEGER)").unwrap();
    assert_eq!(
        session
            .execute(
                "INSERT INTO kept VALUES (1); \
                     INSERT INTO kept VALUES ('bad'); \
                     INSERT INTO kept VALUES (2)",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    assert!(
        session
            .query("SELECT * FROM kept", &[])
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn splits_simple_query_transactions_at_explicit_controls() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();

    assert_eq!(
        session
            .execute(
                "INSERT INTO items VALUES (1); \
                     BEGIN; \
                     INSERT INTO items VALUES (2); \
                     COMMIT; \
                     INSERT INTO items VALUES (3); \
                     INSERT INTO items VALUES ('bad')",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );

    assert_eq!(
        session
            .execute(
                "BEGIN; \
                     INSERT INTO items VALUES (4); \
                     INSERT INTO items VALUES ('bad'); \
                     ROLLBACK",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    assert_eq!(
        session
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InFailedSqlTransaction
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );

    assert_eq!(
        session
            .execute("INSERT INTO items VALUES (5); COMMIT; SELCT missing")
            .unwrap_err()
            .sqlstate,
        SqlState::SyntaxError
    );
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_metadata_for_every_phase_one_type() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE types (
                    flag BOOLEAN,
                    small_value SMALLINT,
                    int_value INTEGER,
                    big_value BIGINT,
                    real_value REAL,
                    double_value DOUBLE PRECISION,
                    numeric_value NUMERIC(5, 2),
                    text_value TEXT,
                    varying_value VARCHAR(3),
                    fixed_value CHAR(2),
                    bytes BYTEA
                )",
        )
        .unwrap();

    let metadata = session.query("SELECT * FROM types", &[]).unwrap().columns;
    assert_eq!(
        metadata
            .iter()
            .map(|column| (column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![
            (BaseType::Bool.map_to_oid(), -1),
            (BaseType::Int2.map_to_oid(), -1),
            (BaseType::Int4.map_to_oid(), -1),
            (BaseType::Int8.map_to_oid(), -1),
            (BaseType::Float4.map_to_oid(), -1),
            (BaseType::Float8.map_to_oid(), -1),
            (BaseType::Numeric.map_to_oid(), (5 << 16) + 2 + 4),
            (BaseType::Text.map_to_oid(), -1),
            (BaseType::Varchar.map_to_oid(), 3 + 4),
            (BaseType::Bpchar.map_to_oid(), 2 + 4),
            (BaseType::Bytea.map_to_oid(), -1),
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn excludes_other_transactions_uncommitted_rows_from_selects() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    let mut state = db.state.lock().unwrap();
    let writer = state.transactions.begin();
    let table_id = state.catalog.require_table("items").unwrap().id;
    state.tables.get_mut(&table_id).unwrap().insert(
        writer,
        crate::txn::CommandId(0),
        vec![Value::Int4(1)],
    );
    drop(state);

    assert!(
        session
            .query("SELECT * FROM items", &[])
            .unwrap()
            .rows
            .is_empty()
    );

    let mut state = db.state.lock().unwrap();
    state.transactions.abort(writer);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_unknown_tables_and_columns_in_selects() {
    let db = Db::create();
    let mut session = db.create_session();

    assert_eq!(
        session
            .query("SELECT * FROM missing", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    assert_eq!(
        session
            .query("SELECT missing FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedColumn
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluates_arithmetic_and_comparison_projections() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER, name TEXT, price NUMERIC)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (7, 3, 'seven', 2.5)")
        .unwrap();

    let result = session
            .query(
                "SELECT id + amount, id - amount, id * amount, id / amount, id % amount, id > amount, name = 'seven', price * 2.0 FROM items",
                &[],
            )
            .unwrap();

    assert_eq!(
        result.rows,
        vec![vec![
            Value::Int4(10),
            Value::Int4(4),
            Value::Int4(21),
            Value::Int4(2),
            Value::Int4(1),
            Value::Bool(true),
            Value::Bool(true),
            Value::Numeric("5.00".parse().unwrap()),
        ]]
    );
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec!["?column?"; 8]
    );
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| (column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![
            (BaseType::Int4.map_to_oid(), -1),
            (BaseType::Int4.map_to_oid(), -1),
            (BaseType::Int4.map_to_oid(), -1),
            (BaseType::Int4.map_to_oid(), -1),
            (BaseType::Int4.map_to_oid(), -1),
            (BaseType::Bool.map_to_oid(), -1),
            (BaseType::Bool.map_to_oid(), -1),
            (BaseType::Numeric.map_to_oid(), -1),
        ]
    );
    assert_eq!(
        session
            .query("SELECT id / 0 FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::DivisionByZero
    );
    session
        .execute("INSERT INTO items VALUES (2147483647, 1, 'max', 1.0)")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT id + 1 FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::NumericValueOutOfRange
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluates_case_and_common_scalar_functions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE items (
                    id INTEGER,
                    score INTEGER,
                    label TEXT,
                    delta INTEGER
                )",
        )
        .unwrap();
    session
        .execute(
            "INSERT INTO items VALUES
                    (1, 7, 'MiXeD', 3),
                    (2, 0, NULL, NULL),
                    (3, NULL, 'third', 4)",
        )
        .unwrap();

    let result = session
        .query(
            "SELECT
                    CASE
                        WHEN score > 5 THEN 'high'
                        WHEN score IS NULL THEN 'missing'
                        ELSE 'low'
                    END,
                    CASE id
                        WHEN 1 THEN 'one'
                        WHEN 2 THEN NULL
                        ELSE 'other'
                    END,
                    CASE WHEN score > 100 THEN score END,
                    COALESCE(label, 'fallback'),
                    NULLIF(score, 0),
                    GREATEST(score, 5),
                    LEAST(score, 5),
                    length(label),
                    lower(label),
                    upper(label),
                    abs(-delta)
                 FROM items",
            &[],
        )
        .unwrap();

    assert_eq!(
        result.rows,
        vec![
            vec![
                Value::Text("high".into()),
                Value::Text("one".into()),
                Value::Null,
                Value::Text("MiXeD".into()),
                Value::Int4(7),
                Value::Int4(7),
                Value::Int4(5),
                Value::Int4(5),
                Value::Text("mixed".into()),
                Value::Text("MIXED".into()),
                Value::Int4(3),
            ],
            vec![
                Value::Text("low".into()),
                Value::Null,
                Value::Null,
                Value::Text("fallback".into()),
                Value::Null,
                Value::Int4(5),
                Value::Int4(0),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
            vec![
                Value::Text("missing".into()),
                Value::Text("other".into()),
                Value::Null,
                Value::Text("third".into()),
                Value::Null,
                Value::Int4(5),
                Value::Int4(5),
                Value::Int4(5),
                Value::Text("third".into()),
                Value::Text("THIRD".into()),
                Value::Int4(4),
            ],
        ]
    );
    assert_eq!(
        session
            .query(
                "SELECT
                        CASE WHEN id = 1 THEN 10 ELSE 1 / (id - 1) END,
                        COALESCE(score, 1 / (score - 7))
                     FROM items
                     WHERE id = 1",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int4(10), Value::Int4(7)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn accepts_minimum_int4_literal_in_simple_case() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (0)").unwrap();

    assert_eq!(
        session
            .query(
                "SELECT CASE id WHEN -2147483648 THEN 'minimum' ELSE 'other' END FROM items",
                &[]
            )
            .unwrap()
            .rows,
        vec![vec![Value::Text("other".into())]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn supports_all_phase_one_numeric_types_in_abs() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE numbers (
                    int2_value SMALLINT,
                    int4_value INTEGER,
                    int8_value BIGINT,
                    float4_value REAL,
                    float8_value DOUBLE PRECISION,
                    numeric_value NUMERIC
                )",
        )
        .unwrap();
    let mut state = db.state.lock().unwrap();
    let xid = state.transactions.begin();
    let table_id = state.catalog.require_table("numbers").unwrap().id;
    state.tables.get_mut(&table_id).unwrap().insert(
        xid,
        crate::txn::CommandId(0),
        vec![
            Value::Int2(-2),
            Value::Int4(-4),
            Value::Int8(-8),
            Value::Float4(-4.5),
            Value::Float8(-8.5),
            Value::Numeric("-12.25".parse().unwrap()),
        ],
    );
    state.transactions.commit(xid);
    drop(state);

    assert_eq!(
        session
            .query(
                "SELECT
                        abs(int2_value),
                        abs(int4_value),
                        abs(int8_value),
                        abs(float4_value),
                        abs(float8_value),
                        abs(numeric_value)
                     FROM numbers",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Int2(2),
            Value::Int4(4),
            Value::Int8(8),
            Value::Float4(4.5),
            Value::Float8(8.5),
            Value::Numeric("12.25".parse().unwrap()),
        ]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_case_and_function_type_errors() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();

    assert_eq!(
        session
            .query(
                "SELECT CASE WHEN id = 1 THEN id ELSE TRUE END FROM items",
                &[]
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DatatypeMismatch
    );
    assert_eq!(
        session
            .query("SELECT unknown_function(id) FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedFunction
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn coerces_phase_one_types_in_all_cast_contexts() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE types (
                    small_value SMALLINT,
                    int_value INTEGER,
                    big_value BIGINT,
                    numeric_value NUMERIC,
                    real_value REAL,
                    double_value DOUBLE PRECISION,
                    short_label VARCHAR(4)
                )",
        )
        .unwrap();
    session
        .execute("INSERT INTO types VALUES (1, 2, 3, 4, 5, 6, 'abcd')")
        .unwrap();

    assert_eq!(
        session
            .query(
                "SELECT
                        small_value + int_value,
                        int_value + big_value,
                        big_value + numeric_value,
                        numeric_value + real_value,
                        real_value + int_value,
                        real_value + double_value,
                        int_value = '2',
                        CASE WHEN TRUE THEN int_value ELSE numeric_value END,
                        COALESCE(NULL, int_value, numeric_value)
                     FROM types",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Int4(3),
            Value::Int8(5),
            Value::Numeric("7".parse().unwrap()),
            Value::Float8(9.0),
            Value::Float8(7.0),
            Value::Float8(11.0),
            Value::Bool(true),
            Value::Numeric("2".parse().unwrap()),
            Value::Numeric("2".parse().unwrap()),
        ]]
    );
    assert_eq!(
        session
            .query(
                "SELECT
                        CAST('42' AS INTEGER),
                        '3.5'::NUMERIC,
                        CAST(2.6 AS INTEGER),
                        CAST(1 AS TEXT),
                        CAST(TRUE AS TEXT),
                        1::BOOLEAN,
                        TRUE::INTEGER,
                        258::BYTEA,
                        '\\x00000102'::BYTEA::INTEGER,
                        CAST('abcdef' AS VARCHAR(3)),
                        CAST(12.36 AS NUMERIC(4, 1))
                     FROM types",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Int4(42),
            Value::Numeric("3.5".parse().unwrap()),
            Value::Int4(3),
            Value::Text("1".into()),
            Value::Text("true".into()),
            Value::Bool(true),
            Value::Int4(1),
            Value::Bytea(vec![0, 0, 1, 2]),
            Value::Int4(258),
            Value::Text("abc".into()),
            Value::Numeric("12.4".parse().unwrap()),
        ]]
    );

    session
        .execute("UPDATE types SET small_value = int_value, int_value = 2.6")
        .unwrap();
    session.execute("UPDATE types SET int_value = '7'").unwrap();
    assert_eq!(
        session
            .query("SELECT small_value, int_value FROM types", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int2(2), Value::Int4(7)]]
    );

    session
        .execute("UPDATE types SET real_value = -0.02")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT real_value IS DISTINCT FROM -0.02 FROM types", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Bool(true)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_postgres_coercion_error_categories() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE assignments (
                    small_value SMALLINT,
                    short_label VARCHAR(3),
                    fixed_numeric NUMERIC(4, 1)
                )",
        )
        .unwrap();

    assert_eq!(
        session
            .execute("INSERT INTO assignments VALUES ('bad', 'abc', 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    assert_eq!(
        session
            .execute("INSERT INTO assignments VALUES (40000, 'abc', 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::NumericValueOutOfRange
    );
    assert_eq!(
        session
            .execute("INSERT INTO assignments VALUES (1, 'toolong', 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::StringDataRightTruncation
    );
    assert_eq!(
        session
            .execute("INSERT INTO assignments VALUES (1, 'abc', 1234.5)")
            .unwrap_err()
            .sqlstate,
        SqlState::NumericValueOutOfRange
    );
    assert_eq!(
        session
            .query("SELECT TRUE::BYTEA FROM assignments", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::CannotCoerce
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn orders_rows_by_columns_expressions_and_output_positions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE items (
                    id INTEGER,
                    name TEXT,
                    score INTEGER,
                    optional INTEGER
                )",
        )
        .unwrap();
    session
        .execute(
            "INSERT INTO items VALUES
                    (1, 'b', 2, NULL),
                    (2, 'a', 2, 5),
                    (3, 'c', 1, 3),
                    (4, NULL, 1, NULL),
                    (5, 'a', 2, 1)",
        )
        .unwrap();

    assert_eq!(
        session
            .query(
                "SELECT id, name FROM items
                     ORDER BY name ASC NULLS LAST, id DESC",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(5), Value::Text("a".into())],
            vec![Value::Int4(2), Value::Text("a".into())],
            vec![Value::Int4(1), Value::Text("b".into())],
            vec![Value::Int4(3), Value::Text("c".into())],
            vec![Value::Int4(4), Value::Null],
        ]
    );
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY score ASC, id DESC", &[],)
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(4)],
            vec![Value::Int4(3)],
            vec![Value::Int4(5)],
            vec![Value::Int4(2)],
            vec![Value::Int4(1)],
        ]
    );
    assert_eq!(
        session
            .query(
                "SELECT name, id FROM items
                     ORDER BY 1 DESC NULLS FIRST, 2 ASC",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Null, Value::Int4(4)],
            vec![Value::Text("c".into()), Value::Int4(3)],
            vec![Value::Text("b".into()), Value::Int4(1)],
            vec![Value::Text("a".into()), Value::Int4(2)],
            vec![Value::Text("a".into()), Value::Int4(5)],
        ]
    );
    assert_eq!(
        session
            .query(
                "SELECT id FROM items
                     ORDER BY score + id DESC, id ASC",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(5)],
            vec![Value::Int4(4)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)],
            vec![Value::Int4(1)],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn applies_postgres_order_by_null_defaults_and_validates_positions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, optional INTEGER)")
        .unwrap();
    session
        .execute(
            "INSERT INTO items VALUES
                    (1, NULL), (2, 5), (3, 3), (4, NULL), (5, 1)",
        )
        .unwrap();

    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY optional ASC", &[])
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(5)],
            vec![Value::Int4(3)],
            vec![Value::Int4(2)],
            vec![Value::Int4(1)],
            vec![Value::Int4(4)],
        ]
    );
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY optional DESC", &[])
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(4)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)],
            vec![Value::Int4(5)],
        ]
    );
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY 0", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidColumnReference
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY 2", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidColumnReference
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn applies_limits_and_offsets_after_ordering() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session
        .execute("INSERT INTO items VALUES (4), (1), (5), (2), (3)")
        .unwrap();

    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY id LIMIT 2 OFFSET 1", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
    );
    assert_eq!(
        session
            .query(
                "SELECT id FROM items ORDER BY id DESC LIMIT 2 OFFSET 1",
                &[]
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int4(4)], vec![Value::Int4(3)]]
    );
    assert_eq!(
        session
            .query("SELECT id FROM items LIMIT 2", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(4)], vec![Value::Int4(1)]]
    );
    assert_eq!(
        session
            .query("SELECT id FROM items OFFSET 3", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
    );
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY id LIMIT 0 OFFSET 2", &[])
            .unwrap()
            .rows,
        Vec::<Vec<Value>>::new()
    );
    assert_eq!(
        session
            .query(
                "SELECT id FROM items ORDER BY id LIMIT NULL OFFSET NULL",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)],
            vec![Value::Int4(4)],
            vec![Value::Int4(5)],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rejects_negative_limit_and_offset() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();

    assert_eq!(
        session
            .query("SELECT id FROM items LIMIT -1", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidRowCountInLimitClause
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        session
            .query("SELECT id FROM items OFFSET -1", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidRowCountInResultOffsetClause
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn applies_defaults_to_inserted_and_updated_rows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE items (
                    id INTEGER NOT NULL DEFAULT 10,
                    amount INTEGER NOT NULL DEFAULT 2 + 3,
                    label TEXT DEFAULT upper('mixed'),
                    optional INTEGER
                )",
        )
        .unwrap();

    session.execute("INSERT INTO items DEFAULT VALUES").unwrap();
    session
        .execute("INSERT INTO items (id, label) VALUES (1, DEFAULT), (2, NULL)")
        .unwrap();
    session
        .execute("INSERT INTO items (id, amount) VALUES (3, DEFAULT)")
        .unwrap();
    session
        .execute("UPDATE items SET amount = DEFAULT, label = DEFAULT WHERE id = 2")
        .unwrap();

    assert_eq!(
        session
            .query(
                "SELECT id, amount, label, optional FROM items ORDER BY id",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![
                Value::Int4(1),
                Value::Int4(5),
                Value::Text("MIXED".into()),
                Value::Null,
            ],
            vec![
                Value::Int4(2),
                Value::Int4(5),
                Value::Text("MIXED".into()),
                Value::Null,
            ],
            vec![
                Value::Int4(3),
                Value::Int4(5),
                Value::Text("MIXED".into()),
                Value::Null,
            ],
            vec![
                Value::Int4(10),
                Value::Int4(5),
                Value::Text("MIXED".into()),
                Value::Null,
            ],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn enforces_not_null_after_defaults_and_assignments() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER NOT NULL, optional INTEGER)")
        .unwrap();

    assert_eq!(
        session
            .execute("INSERT INTO items (optional) VALUES (1)")
            .unwrap_err()
            .sqlstate,
        SqlState::NotNullViolation
    );
    session
        .execute("INSERT INTO items VALUES (1, NULL)")
        .unwrap();
    assert_eq!(
        session
            .execute("UPDATE items SET id = DEFAULT")
            .unwrap_err()
            .sqlstate,
        SqlState::NotNullViolation
    );
    assert_eq!(
        session
            .execute("CREATE TABLE invalid_default (a INTEGER, b INTEGER DEFAULT a)")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn enforces_check_constraints_on_insert_and_update() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE ranges (
                    value INTEGER CHECK (value > 0),
                    lower_bound INTEGER,
                    upper_bound INTEGER,
                    CHECK (lower_bound < upper_bound)
                )",
        )
        .unwrap();

    session
        .execute("INSERT INTO ranges VALUES (1, 1, 2), (NULL, NULL, NULL)")
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO ranges VALUES (-1, 1, 2)")
            .unwrap_err()
            .sqlstate,
        SqlState::CheckViolation
    );
    assert_eq!(
        session
            .execute("INSERT INTO ranges VALUES (2, 3, 2)")
            .unwrap_err()
            .sqlstate,
        SqlState::CheckViolation
    );
    assert_eq!(
        session
            .execute("UPDATE ranges SET value = -1 WHERE value = 1")
            .unwrap_err()
            .sqlstate,
        SqlState::CheckViolation
    );
    session
        .execute("UPDATE ranges SET lower_bound = NULL WHERE value = 1")
        .unwrap();
    assert_eq!(
        session
            .execute("CREATE TABLE invalid_check (value INTEGER CHECK (value + 1))")
            .unwrap_err()
            .sqlstate,
        SqlState::DatatypeMismatch
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn enforces_primary_and_multi_column_unique_constraints() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE accounts (
                    id INTEGER PRIMARY KEY,
                    tenant INTEGER,
                    email TEXT,
                    UNIQUE (tenant, email)
                )",
        )
        .unwrap();
    session
        .execute("INSERT INTO accounts VALUES (1, 1, 'a'), (2, 1, 'b')")
        .unwrap();

    assert_eq!(
        session
            .execute("INSERT INTO accounts VALUES (1, 2, 'c')")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    assert_eq!(
        session
            .execute("INSERT INTO accounts VALUES (3, 1, 'a')")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    assert_eq!(
        session
            .execute("UPDATE accounts SET id = 1 WHERE id = 2")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    assert_eq!(
        session
            .execute("INSERT INTO accounts VALUES (NULL, 2, 'd')")
            .unwrap_err()
            .sqlstate,
        SqlState::NotNullViolation
    );

    session
        .execute("INSERT INTO accounts VALUES (3, NULL, 'a'), (4, NULL, 'a')")
        .unwrap();
    session
        .execute("UPDATE accounts SET id = 5, email = 'c' WHERE id = 2")
        .unwrap();
    session
        .execute("DELETE FROM accounts WHERE id = 1")
        .unwrap();
    session
        .execute("INSERT INTO accounts VALUES (1, 1, 'a')")
        .unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rebuilds_unique_indexes_after_rollback() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();
    session.execute("BEGIN").unwrap();
    session.execute("UPDATE items SET id = 2").unwrap();
    session.execute("ROLLBACK").unwrap();

    session.execute("INSERT INTO items VALUES (2)").unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO items VALUES (1)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn controls_insert_and_update_visibility_with_explicit_transactions() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
        .unwrap();
    first.execute("INSERT INTO items VALUES (1, 1)").unwrap();

    first.execute("BEGIN").unwrap();
    first
        .execute("UPDATE items SET amount = amount + 1 WHERE id = 1")
        .unwrap();
    first.execute("INSERT INTO items VALUES (2, 2)").unwrap();
    assert_eq!(
        first.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(2)]
        ]
    );
    assert_eq!(
        second.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1), Value::Int4(1)]]
    );
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(2)]
        ]
    );

    first.execute("BEGIN").unwrap();
    first.execute("INSERT INTO items VALUES (3, 3)").unwrap();
    first.execute("ROLLBACK").unwrap();
    assert_eq!(
        second.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(2)]
        ]
    );
    first.execute("INSERT INTO items VALUES (4, 4)").unwrap();
    assert_eq!(
        second.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(2)],
            vec![Value::Int4(4), Value::Int4(4)],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn controls_snapshot_lifetime_by_isolation_level() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TABLE items (id INTEGER)").unwrap();
    first.execute("INSERT INTO items VALUES (1)").unwrap();

    first.execute("BEGIN").unwrap();
    assert_eq!(
        first.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    second.execute("INSERT INTO items VALUES (2)").unwrap();
    assert_eq!(
        first.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
    first.execute("COMMIT").unwrap();

    first
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    assert_eq!(
        first.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
    second.execute("INSERT INTO items VALUES (3)").unwrap();
    assert_eq!(
        first.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
    first.execute("COMMIT").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reclaims_deleted_rows_between_autocommit_statements() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();

    for id in 0..100 {
        session
            .execute(&format!("INSERT INTO items VALUES ({id})"))
            .unwrap();
        session
            .execute(&format!("DELETE FROM items WHERE id = {id}"))
            .unwrap();
    }

    let state = db.state.lock().unwrap();
    let table_id = state.catalog.require_table("items").unwrap().id;
    assert_eq!(
        state
            .tables
            .get(&table_id)
            .unwrap()
            .iterate_version_chains()
            .count(),
        0
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn retains_deleted_rows_until_repeatable_read_snapshot_finishes() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
    writer.execute("INSERT INTO items VALUES (1)").unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );

    writer.execute("DELETE FROM items WHERE id = 1").unwrap();
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    {
        let state = db.state.lock().unwrap();
        let table_id = state.catalog.require_table("items").unwrap().id;
        assert_eq!(
            state
                .tables
                .get(&table_id)
                .unwrap()
                .iterate_version_chains()
                .count(),
            1
        );
    }

    reader.execute("ROLLBACK").unwrap();
    let state = db.state.lock().unwrap();
    let table_id = state.catalog.require_table("items").unwrap().id;
    assert_eq!(
        state
            .tables
            .get(&table_id)
            .unwrap()
            .iterate_version_chains()
            .count(),
        0
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn retains_dropped_table_until_repeatable_read_snapshot_finishes() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
    writer.execute("INSERT INTO items VALUES (1)").unwrap();
    let table_id = db
        .state
        .lock()
        .unwrap()
        .catalog
        .require_table("items")
        .unwrap()
        .id;
    reader
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(writer.execute("DROP TABLE items"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    assert!(db.state.lock().unwrap().tables.contains_key(&table_id));

    reader.execute("ROLLBACK").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
    assert!(!db.state.lock().unwrap().tables.contains_key(&table_id));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn exposes_catalog_and_rows_atomically_at_commit() {
    let mut state = DatabaseState::create();
    let writer = state.transactions.begin();
    let concurrent_snapshot = Snapshot::create(&state.transactions);
    state.load_catalog(
        Some(writer),
        concurrent_snapshot.use_command(crate::txn::CommandId(0)),
        None,
    );
    let previous = state.catalog.clone();
    let table_id = state
        .catalog
        .create_table(
            "items".into(),
            vec![crate::catalog::ColumnDef {
                name: "id".into(),
                data_type: crate::value::PgType::create(BaseType::Int4),
                nullable: false,
                default: None,
                default_sequence: None,
                identity: None,
            }],
            vec![],
        )
        .unwrap();
    let schema = state.catalog.require_table("items").unwrap().clone();
    let mut table = Table::create(schema);
    table.insert(writer, crate::txn::CommandId(0), vec![Value::Int4(1)]);
    state.tables.insert(table_id, table);
    state.record_catalog_changes(&previous, writer, crate::txn::CommandId(0));
    let concurrent_reader = state.transactions.begin();

    assert!(
        state
            .catalog_history
            .materialize(None, concurrent_snapshot, &state.transactions)
            .require_table("items")
            .is_err()
    );
    assert!(
        crate::txn::find_visible_version(
            state
                .tables
                .get(&table_id)
                .unwrap()
                .iterate_version_chains()
                .next()
                .unwrap()
                .1,
            &concurrent_snapshot,
            concurrent_reader,
            &state.transactions,
        )
        .is_none()
    );

    state.transactions.commit(writer);
    let committed_snapshot = Snapshot::create(&state.transactions);
    let committed_reader = state.transactions.begin();
    assert_eq!(
        state
            .catalog_history
            .materialize(None, committed_snapshot, &state.transactions)
            .require_table("items")
            .unwrap()
            .id,
        table_id
    );
    assert_eq!(
        crate::txn::find_visible_version(
            state
                .tables
                .get(&table_id)
                .unwrap()
                .iterate_version_chains()
                .next()
                .unwrap()
                .1,
            &committed_snapshot,
            committed_reader,
            &state.transactions,
        )
        .unwrap()
        .row,
        vec![Value::Int4(1)]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn retains_dropped_table_while_a_read_committed_writer_uses_it() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut writer = db.create_session();
    let mut dropper = db.create_session();
    writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
    writer.execute("INSERT INTO items VALUES (1)").unwrap();
    let table_id = db
        .state
        .lock()
        .unwrap()
        .catalog
        .require_table("items")
        .unwrap()
        .id;

    writer.execute("BEGIN").unwrap();
    writer.execute("UPDATE items SET id = 2").unwrap();
    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP TABLE items"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    assert!(db.state.lock().unwrap().tables.contains_key(&table_id));

    writer.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
    assert!(!db.state.lock().unwrap().tables.contains_key(&table_id));
    assert_eq!(
        writer
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn prepared_query_does_not_retarget_a_recreated_relation() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();
    let prepared = session.prepare("SELECT id FROM items").unwrap();

    session.execute("DROP TABLE items").unwrap();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (2)").unwrap();

    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn prepared_fallback_queries_and_mutations_do_not_retarget_recreated_relations() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();
    let ordered = session.prepare("SELECT id FROM items ORDER BY id").unwrap();
    let update = session.prepare("UPDATE items SET id = id + 1").unwrap();

    session.execute("DROP TABLE items").unwrap();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (2)").unwrap();

    assert_eq!(
        session.query_prepared(&ordered, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session.execute_prepared(&update, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session.query("SELECT id FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn prepared_queries_reject_changed_table_schema_versions() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    let prepared = session.prepare("SELECT * FROM items").unwrap();
    {
        let mut state = db.state.lock().unwrap();
        let changer = state.transactions.begin();
        let snapshot = Snapshot::create(&state.transactions);
        state.load_catalog(Some(changer), snapshot, None);
        let previous = state.catalog.clone();
        state
            .catalog
            .require_table_mut("items")
            .unwrap()
            .columns
            .push(crate::catalog::ColumnDef {
                name: "value".into(),
                data_type: crate::value::PgType::create(BaseType::Text),
                nullable: true,
                default: None,
                default_sequence: None,
                identity: None,
            });
        state.record_catalog_changes(&previous, changer, crate::txn::CommandId(0));
        state.transactions.commit(changer);
    }

    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn prepared_subqueries_ddl_sequences_and_constraints_keep_catalog_dependencies() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();
    session
        .execute("CREATE TABLE permissions (item_id INTEGER)")
        .unwrap();
    session.execute("CREATE SEQUENCE ids").unwrap();
    session.execute("CREATE SEQUENCE \"Ids\"").unwrap();
    assert!(session.query("SELECT nextval('IDS')", &[]).is_ok());
    assert!(session.query("SELECT nextval('public.ids')", &[]).is_ok());
    assert!(session.query("SELECT nextval('\"Ids\"')", &[]).is_ok());
    let subquery = session
            .prepare(
                "SELECT id FROM items WHERE EXISTS (SELECT 1 FROM permissions WHERE item_id = items.id)",
            )
            .unwrap();
    let drop_table = session.prepare("DROP TABLE permissions").unwrap();
    let sequence = session.prepare("SELECT nextval('IDS')").unwrap();
    let qualified_sequence = session.prepare("SELECT nextval('public.ids')").unwrap();
    let quoted_sequence = session.prepare("SELECT nextval('\"Ids\"')").unwrap();
    let conflict = session
        .prepare("INSERT INTO items VALUES (1) ON CONFLICT ON CONSTRAINT items_pkey DO NOTHING")
        .unwrap();
    assert!(
        conflict
            .catalog_dependencies
            .iter()
            .any(|dependency| { matches!(dependency, CatalogDependency::Constraint { .. }) })
    );

    session.execute("DROP TABLE permissions").unwrap();
    session
        .execute("CREATE TABLE permissions (item_id INTEGER)")
        .unwrap();
    assert_eq!(
        session.query_prepared(&subquery, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .execute_prepared(&drop_table, &[])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert!(session.prepare("SELECT * FROM permissions").is_ok());

    session.execute("DROP SEQUENCE ids").unwrap();
    session.execute("CREATE SEQUENCE ids").unwrap();
    session.execute("DROP SEQUENCE \"Ids\"").unwrap();
    session.execute("CREATE SEQUENCE \"Ids\"").unwrap();
    assert_eq!(
        session.query_prepared(&sequence, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .query_prepared(&qualified_sequence, &[])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        session
            .query_prepared(&quoted_sequence, &[])
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn follows_postgres_isolation_selection_order() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TABLE items (id INTEGER)").unwrap();
    first.execute("INSERT INTO items VALUES (1)").unwrap();
    first
        .execute("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .unwrap();

    first.execute("BEGIN").unwrap();
    first.query("SELECT * FROM items", &[]).unwrap();
    second.execute("INSERT INTO items VALUES (2)").unwrap();
    assert_eq!(
        first.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    first.execute("COMMIT").unwrap();

    first.execute("BEGIN").unwrap();
    first
        .execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .unwrap();
    first.query("SELECT * FROM items", &[]).unwrap();
    second.execute("INSERT INTO items VALUES (3)").unwrap();
    assert_eq!(
        first.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );
    first.execute("COMMIT").unwrap();

    {
        let mut transaction = first.begin_with(IsolationLevel::RepeatableRead).unwrap();
        transaction.query("SELECT * FROM items", &[]).unwrap();
        second.execute("INSERT INTO items VALUES (4)").unwrap();
        assert_eq!(
            transaction
                .query("SELECT * FROM items", &[])
                .unwrap()
                .rows
                .len(),
            3
        );
        transaction.commit().unwrap();
    }

    first.execute("BEGIN").unwrap();
    first.query("SELECT * FROM items", &[]).unwrap();
    assert_eq!(
        first
            .execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .unwrap_err()
            .sqlstate,
        SqlState::ActiveSqlTransaction
    );
    assert_eq!(
        first
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InFailedSqlTransaction
    );
    first.execute("ROLLBACK").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn blocks_and_rechecks_read_committed_writer_after_commit() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
        .unwrap();
    first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
    first.execute("BEGIN").unwrap();
    first.execute("UPDATE items SET amount = 2").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(second.execute("UPDATE items SET amount = amount + 1 WHERE id = 1"))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
    assert_eq!(
        first.query("SELECT amount FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(3)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn allows_blocked_writer_after_holder_rollback() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
        .unwrap();
    first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
    first.execute("BEGIN").unwrap();
    first.execute("UPDATE items SET amount = 5").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(second.execute("UPDATE items SET amount = amount + 1 WHERE id = 1"))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
    assert_eq!(
        first.query("SELECT amount FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn waits_for_on_conflict_rows_and_rechecks_commit_or_rollback() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();
    first.execute("BEGIN").unwrap();
    first.execute("INSERT INTO items VALUES (1)").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(second.execute("INSERT INTO items VALUES (1) ON CONFLICT (id) DO NOTHING"))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();

    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("BEGIN").unwrap();
    first.execute("INSERT INTO items VALUES (2)").unwrap();
    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(second.execute("INSERT INTO items VALUES (2) ON CONFLICT (id) DO NOTHING"))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("ROLLBACK").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
}

#[test]
fn locks_conflicts_after_before_insert_triggers() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE TABLE triggered_conflicts (
                    id INTEGER PRIMARY KEY,
                    action TEXT NOT NULL
                );
                CREATE FUNCTION rewrite_conflict_key() RETURNS TRIGGER AS $$
                BEGIN
                    IF NEW.action = 'skip' THEN
                        RETURN NULL;
                    ELSIF NEW.action = 'conflict' THEN
                        NEW.id := 1;
                    END IF;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER rewrite_conflict_key BEFORE INSERT ON triggered_conflicts
                    FOR EACH ROW EXECUTE FUNCTION rewrite_conflict_key();
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("INSERT INTO triggered_conflicts VALUES (1, 'held')")
        .unwrap();

    assert_eq!(
        writer
            .execute(
                "INSERT INTO triggered_conflicts VALUES (2, 'keep') \
                     ON CONFLICT (id) DO NOTHING",
            )
            .unwrap(),
        create_affected_results(1)
    );
    assert_eq!(
        writer
            .execute(
                "INSERT INTO triggered_conflicts VALUES (3, 'skip') \
                     ON CONFLICT (id) DO NOTHING",
            )
            .unwrap(),
        create_affected_results(0)
    );

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute(
                "INSERT INTO triggered_conflicts VALUES (4, 'conflict') \
                     ON CONFLICT (id) DO NOTHING",
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
}

#[test]
fn preserves_before_insert_values_across_conflict_waits() {
    for commits in [true, false] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut holder = db.create_session();
        let mut writer = db.create_session();
        let mut observer = db.create_session();
        holder
            .execute(
                r#"
                    CREATE SEQUENCE waited_trigger_values;
                    CREATE TABLE waited_trigger_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO waited_trigger_rows VALUES (1, 0);
                    CREATE FUNCTION allocate_waited_trigger_value() RETURNS TRIGGER AS $$
                    BEGIN NEW.value := nextval('waited_trigger_values'); RETURN NEW; END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER allocate_waited_trigger_value
                        BEFORE INSERT ON waited_trigger_rows
                        FOR EACH ROW EXECUTE FUNCTION allocate_waited_trigger_value();
                    "#,
            )
            .unwrap();
        holder.execute("BEGIN").unwrap();
        holder
            .execute("UPDATE waited_trigger_rows SET value = 10 WHERE id = 1")
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            sender
                .send(writer.query(
                    "INSERT INTO waited_trigger_rows VALUES (1, 0) \
                         ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                         RETURNING value",
                    &[],
                ))
                .unwrap();
        });
        wait_until_blocked(&db);
        assert_eq!(
            observer
                .query("SELECT nextval('waited_trigger_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(2)]]
        );
        holder
            .execute(if commits { "COMMIT" } else { "ROLLBACK" })
            .unwrap();
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
        handle.join().unwrap();
        assert_eq!(
            observer
                .query("SELECT value FROM waited_trigger_rows", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
    }
}

#[test]
fn preserves_before_insert_values_across_select_conflict_waits() {
    for commits in [true, false] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut holder = db.create_session();
        let mut writer = db.create_session();
        let mut observer = db.create_session();
        holder
            .execute(
                r#"
                    CREATE SEQUENCE waited_select_trigger_values;
                    CREATE TABLE waited_select_trigger_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO waited_select_trigger_rows VALUES (1, 0);
                    CREATE FUNCTION allocate_waited_select_trigger_value() RETURNS TRIGGER AS $$
                    BEGIN NEW.value := nextval('waited_select_trigger_values'); RETURN NEW; END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER allocate_waited_select_trigger_value
                        BEFORE INSERT ON waited_select_trigger_rows
                        FOR EACH ROW EXECUTE FUNCTION allocate_waited_select_trigger_value();
                    "#,
            )
            .unwrap();
        holder.execute("BEGIN").unwrap();
        holder
            .execute("UPDATE waited_select_trigger_rows SET value = 10 WHERE id = 1")
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            sender
                .send(writer.query(
                    "INSERT INTO waited_select_trigger_rows \
                         SELECT nextval('waited_select_trigger_values') * 0 + 1, 0 WHERE TRUE \
                         ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                         RETURNING value, nextval('waited_select_trigger_values')",
                    &[],
                ))
                .unwrap();
        });
        wait_until_blocked(&db);
        assert_eq!(
            observer
                .query("SELECT nextval('waited_select_trigger_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(3)]]
        );
        holder
            .execute(if commits { "COMMIT" } else { "ROLLBACK" })
            .unwrap();
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .rows,
            vec![vec![Value::Int8(2), Value::Int8(4)]]
        );
        handle.join().unwrap();
    }
}

#[test]
fn preserves_insert_select_offset_and_source_snapshot() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE offset_projection_values;
                CREATE TABLE offset_projection_source (id BIGINT PRIMARY KEY);
                CREATE TABLE offset_projection_rows (id BIGINT, value BIGINT);
                INSERT INTO offset_projection_source VALUES (1), (2), (3);
                CREATE FUNCTION preserve_insert_select_row() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_offset_projection_row
                    BEFORE INSERT ON offset_projection_rows
                    FOR EACH ROW EXECUTE FUNCTION preserve_insert_select_row();
                "#,
        )
        .unwrap();
    assert!(
        session
            .query(
                "INSERT INTO offset_projection_rows \
                     SELECT id, nextval('offset_projection_values') \
                     FROM offset_projection_source LIMIT 0 OFFSET 1 \
                     RETURNING id, value",
                &[],
            )
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        session
            .query("SELECT nextval('offset_projection_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
    session
        .execute("SELECT setval('offset_projection_values', 1, false)")
        .unwrap();
    assert_eq!(
        session
            .query(
                "INSERT INTO offset_projection_rows \
                     SELECT id, nextval('offset_projection_values') \
                     FROM offset_projection_source OFFSET 1 \
                     RETURNING id, value, nextval('offset_projection_values')",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(2), Value::Int8(2), Value::Int8(3)],
            vec![Value::Int8(3), Value::Int8(4), Value::Int8(5)],
        ]
    );
    assert_eq!(
        session
            .query("SELECT nextval('offset_projection_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(6)]]
    );

    session
        .execute(
            r#"
                CREATE SEQUENCE snapshot_trigger_values;
                CREATE TABLE snapshot_trigger_source (id BIGINT PRIMARY KEY);
                CREATE TABLE snapshot_trigger_rows (
                    id BIGINT PRIMARY KEY,
                    value BIGINT NOT NULL
                );
                INSERT INTO snapshot_trigger_source VALUES (1), (2), (3);
                INSERT INTO snapshot_trigger_rows VALUES (1, 0);
                CREATE FUNCTION allocate_snapshot_trigger_value() RETURNS TRIGGER AS $$
                BEGIN NEW.value := nextval('snapshot_trigger_values'); RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_snapshot_trigger_value
                    BEFORE INSERT ON snapshot_trigger_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_snapshot_trigger_value();
                "#,
        )
        .unwrap();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("UPDATE snapshot_trigger_rows SET value = 10 WHERE id = 1")
        .unwrap();
    holder
        .execute("DELETE FROM snapshot_trigger_source WHERE id = 1")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.query(
                "INSERT INTO snapshot_trigger_rows \
                     SELECT id, 0 FROM snapshot_trigger_source WHERE TRUE \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                     RETURNING id, value, nextval('snapshot_trigger_values')",
                &[],
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    assert_eq!(
        session
            .query("SELECT nextval('snapshot_trigger_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1), Value::Int8(1), Value::Int8(3)],
            vec![Value::Int8(2), Value::Int8(4), Value::Int8(5)],
            vec![Value::Int8(3), Value::Int8(6), Value::Int8(7)],
        ]
    );
    handle.join().unwrap();
}

#[test]
fn preserves_statement_source_snapshot_across_insert_ctes() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut setup = db.create_session();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    setup
        .execute(
            r#"
                CREATE TABLE cte_snapshot_source (id BIGINT PRIMARY KEY);
                CREATE TABLE cte_snapshot_rows (id BIGINT PRIMARY KEY, value BIGINT);
                INSERT INTO cte_snapshot_source VALUES (1), (2), (3);
                INSERT INTO cte_snapshot_rows VALUES (1, 0);
                CREATE FUNCTION preserve_cte_snapshot_row() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_cte_snapshot_row
                    BEFORE INSERT ON cte_snapshot_rows
                    FOR EACH ROW EXECUTE FUNCTION preserve_cte_snapshot_row();
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("UPDATE cte_snapshot_rows SET value = 10 WHERE id = 1")
        .unwrap();

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.query(
                "WITH first_insert AS (\
                         INSERT INTO cte_snapshot_rows VALUES (1, 0) \
                         ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                         RETURNING id\
                     ), later_insert AS (\
                         INSERT INTO cte_snapshot_rows \
                         SELECT id + 10, 0 FROM cte_snapshot_source \
                         RETURNING id\
                     ) \
                     SELECT first_insert.id, later_insert.id \
                     FROM first_insert CROSS JOIN later_insert \
                     ORDER BY later_insert.id",
                &[],
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    holder
        .execute("INSERT INTO cte_snapshot_source VALUES (4)")
        .unwrap();
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1), Value::Int8(11)],
            vec![Value::Int8(1), Value::Int8(12)],
            vec![Value::Int8(1), Value::Int8(13)],
        ]
    );
    handle.join().unwrap();
}

#[test]
fn preserves_before_insert_values_across_materialized_select_waits() {
    for source in [
        "SELECT id, 0 FROM materialized_trigger_source ORDER BY id",
        "SELECT DISTINCT id, 0 FROM materialized_trigger_source",
        "SELECT max(id), 0 FROM materialized_trigger_source",
        "SELECT id, 0 FROM materialized_trigger_source WHERE abs(id) = 1",
    ] {
        for commits in [true, false] {
            let db = Db::create_builder()
                .set_lock_timeout(Duration::from_secs(2))
                .build();
            let mut holder = db.create_session();
            let mut writer = db.create_session();
            let mut observer = db.create_session();
            holder
                .execute(
                    r#"
                        CREATE SEQUENCE materialized_trigger_values;
                        CREATE TABLE materialized_trigger_source (id BIGINT);
                        CREATE TABLE materialized_trigger_rows (
                            id BIGINT PRIMARY KEY,
                            value BIGINT NOT NULL
                        );
                        INSERT INTO materialized_trigger_source VALUES (1);
                        INSERT INTO materialized_trigger_rows VALUES (1, 0);
                        CREATE FUNCTION allocate_materialized_trigger_value() RETURNS TRIGGER AS $$
                        BEGIN
                            NEW.value := nextval('materialized_trigger_values');
                            RETURN NEW;
                        END;
                        $$ LANGUAGE plpgsql;
                        CREATE TRIGGER allocate_materialized_trigger_value
                            BEFORE INSERT ON materialized_trigger_rows
                            FOR EACH ROW EXECUTE FUNCTION allocate_materialized_trigger_value();
                        "#,
                )
                .unwrap();
            holder.execute("BEGIN").unwrap();
            holder
                .execute("UPDATE materialized_trigger_rows SET value = 10 WHERE id = 1")
                .unwrap();

            let query = format!(
                "INSERT INTO materialized_trigger_rows {source} \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                     RETURNING value"
            );
            let (sender, receiver) = mpsc::channel();
            let handle = thread::spawn(move || {
                sender.send(writer.query(&query, &[])).unwrap();
            });
            wait_until_blocked(&db);
            assert_eq!(
                observer
                    .query("SELECT nextval('materialized_trigger_values')", &[])
                    .unwrap()
                    .rows,
                vec![vec![Value::Int8(2)]],
                "source: {source}, commits: {commits}"
            );
            holder
                .execute(if commits { "COMMIT" } else { "ROLLBACK" })
                .unwrap();
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .unwrap()
                    .rows,
                vec![vec![Value::Int8(1)]],
                "source: {source}, commits: {commits}"
            );
            handle.join().unwrap();
        }
    }
}

#[test]
fn resumes_volatile_insert_select_stream_state() {
    let cases = [
        (
            "SELECT DISTINCT id, 0 FROM stream_resume_source ORDER BY id",
            "(1), (2)",
            2,
            vec![
                vec![Value::Int8(1), Value::Int8(1), Value::Int8(3)],
                vec![Value::Int8(2), Value::Int8(4), Value::Int8(5)],
            ],
        ),
        (
            "SELECT id, max(id) * 0 FROM stream_resume_source GROUP BY id ORDER BY id",
            "(1), (2)",
            2,
            vec![
                vec![Value::Int8(1), Value::Int8(1), Value::Int8(3)],
                vec![Value::Int8(2), Value::Int8(4), Value::Int8(5)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source GROUP BY id ORDER BY id",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1 \
                 UNION ALL \
                 SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 2",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1 \
                 UNION ALL \
                 SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 2 \
                 UNION ALL \
                 SELECT id + 2, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
                vec![Value::Int8(3), Value::Int8(9), Value::Int8(10)],
            ],
        ),
        (
            "((SELECT id, nextval('stream_resume_values') \
                    FROM stream_resume_source WHERE id = 1 LIMIT 1) \
                  UNION ALL \
                  SELECT id, nextval('stream_resume_values') \
                  FROM stream_resume_source WHERE id = 2) \
                 UNION ALL \
                 SELECT id + 2, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
                vec![Value::Int8(3), Value::Int8(9), Value::Int8(10)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1 \
                 UNION ALL (\
                     SELECT id, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 2 \
                     UNION ALL \
                     SELECT id + 2, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 1 \
                     LIMIT 2\
                 )",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
                vec![Value::Int8(3), Value::Int8(9), Value::Int8(10)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1 \
                 UNION ALL (\
                     SELECT id, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 2 \
                     UNION ALL \
                     SELECT id + 2, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 1 \
                     OFFSET 0\
                 )",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
                vec![Value::Int8(3), Value::Int8(9), Value::Int8(10)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1 \
                 UNION ALL (\
                     SELECT id, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 2 \
                     UNION ALL \
                     SELECT id + 2, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 1\
                 )",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
                vec![Value::Int8(3), Value::Int8(9), Value::Int8(10)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source WHERE id = 1 \
                 UNION ALL (\
                     SELECT id, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 2 \
                     UNION ALL \
                     SELECT id + 2, nextval('stream_resume_values') \
                     FROM stream_resume_source WHERE id = 1 \
                     ORDER BY id\
                 )",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(7), Value::Int8(8)],
                vec![Value::Int8(3), Value::Int8(9), Value::Int8(10)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source ORDER BY 2",
            "(1), (2)",
            4,
            vec![
                vec![Value::Int8(1), Value::Int8(3), Value::Int8(5)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
            ],
        ),
        (
            "SELECT id, 0 FROM stream_resume_source WHERE id = 1 \
                 UNION ALL \
                 SELECT id + 1, 1 / (2 - id) FROM stream_resume_source \
                 LIMIT 2",
            "(1), (2)",
            2,
            vec![
                vec![Value::Int8(1), Value::Int8(1), Value::Int8(3)],
                vec![Value::Int8(2), Value::Int8(4), Value::Int8(5)],
            ],
        ),
        (
            "SELECT id, 0 FROM stream_resume_source \
                 WHERE nextval('stream_resume_values') > 0",
            "(1), (2)",
            3,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(4)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
            ],
        ),
        (
            "SELECT id, nextval('stream_resume_values') \
                 FROM stream_resume_source ORDER BY id LIMIT 2 OFFSET 1",
            "(0), (1), (2), (3), (4)",
            4,
            vec![
                vec![Value::Int8(1), Value::Int8(3), Value::Int8(5)],
                vec![Value::Int8(2), Value::Int8(7), Value::Int8(8)],
            ],
        ),
    ];
    for (source, source_values, observed, expected) in cases {
        for commits in [true, false] {
            let db = Db::create_builder()
                .set_lock_timeout(Duration::from_secs(2))
                .build();
            let mut holder = db.create_session();
            let mut writer = db.create_session();
            let mut observer = db.create_session();
            holder
                .execute(&format!(
                    r#"
                        CREATE SEQUENCE stream_resume_values;
                        CREATE TABLE stream_resume_source (id BIGINT);
                        CREATE TABLE stream_resume_rows (
                            id BIGINT PRIMARY KEY,
                            value BIGINT NOT NULL
                        );
                        INSERT INTO stream_resume_source VALUES {source_values};
                        INSERT INTO stream_resume_rows VALUES (1, 0);
                        CREATE FUNCTION allocate_stream_resume_value() RETURNS TRIGGER AS $$
                        BEGIN
                            NEW.value := nextval('stream_resume_values');
                            RETURN NEW;
                        END;
                        $$ LANGUAGE plpgsql;
                        CREATE TRIGGER allocate_stream_resume_value
                            BEFORE INSERT ON stream_resume_rows
                            FOR EACH ROW EXECUTE FUNCTION allocate_stream_resume_value();
                        "#
                ))
                .unwrap();
            holder.execute("BEGIN").unwrap();
            holder
                .execute("UPDATE stream_resume_rows SET value = 10 WHERE id = 1")
                .unwrap();

            let query = format!(
                "INSERT INTO stream_resume_rows {source} \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                     RETURNING id, value, nextval('stream_resume_values')"
            );
            let (sender, receiver) = mpsc::channel();
            let handle = thread::spawn(move || {
                sender.send(writer.query(&query, &[])).unwrap();
            });
            wait_until_blocked(&db);
            assert_eq!(
                observer
                    .query("SELECT nextval('stream_resume_values')", &[])
                    .unwrap()
                    .rows,
                vec![vec![Value::Int8(observed)]],
                "source: {source}, commits: {commits}"
            );
            holder
                .execute(if commits { "COMMIT" } else { "ROLLBACK" })
                .unwrap();
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .unwrap()
                    .rows,
                expected,
                "source: {source}, commits: {commits}"
            );
            handle.join().unwrap();
        }
    }
}

#[test]
fn evaluates_immutable_ordered_insert_projections_before_trigger_waits() {
    for (projection, groupings) in [
        ("1 / (2 - id)", &["", " GROUP BY id"][..]),
        (
            "CASE WHEN FALSE THEN nextval('ordered_error_values') ELSE 1 / (2 - id) END",
            &[""][..],
        ),
        (
            "(CASE WHEN FALSE THEN nextval('ordered_error_values') ELSE 1 / (2 - id) END) + 0",
            &[""][..],
        ),
        (
            "CAST(CASE WHEN FALSE THEN nextval('ordered_error_values') ELSE 1 / (2 - id) END AS BIGINT)",
            &[""][..],
        ),
        (
            "(CASE WHEN FALSE THEN nextval('ordered_error_values') WHEN id = 1 THEN 1 ELSE 1 / (2 - id) END) + 0",
            &[""][..],
        ),
    ] {
        for grouping in groupings {
            let db = Db::create_builder()
                .set_lock_timeout(Duration::from_secs(2))
                .build();
            let mut setup = db.create_session();
            let mut holder = db.create_session();
            let mut writer = db.create_session();
            setup
                .execute(
                    r#"
                    CREATE SEQUENCE ordered_error_values;
                    CREATE TABLE ordered_error_source (id BIGINT);
                    CREATE TABLE ordered_error_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO ordered_error_source VALUES (1), (2);
                    INSERT INTO ordered_error_rows VALUES (1, 0);
                    CREATE FUNCTION allocate_ordered_error_value() RETURNS TRIGGER AS $$
                    BEGIN
                        NEW.value := nextval('ordered_error_values');
                        RETURN NEW;
                    END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER allocate_ordered_error_value
                        BEFORE INSERT ON ordered_error_rows
                        FOR EACH ROW EXECUTE FUNCTION allocate_ordered_error_value();
                    "#,
                )
                .unwrap();
            holder.execute("BEGIN").unwrap();
            holder
                .execute("UPDATE ordered_error_rows SET value = 10 WHERE id = 1")
                .unwrap();

            let query = format!(
                "INSERT INTO ordered_error_rows \
                 SELECT id, {projection} FROM ordered_error_source{grouping} ORDER BY id \
                 ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                 RETURNING id, value, nextval('ordered_error_values')"
            );
            let (sender, receiver) = mpsc::channel();
            let handle = thread::spawn(move || {
                sender.send(writer.execute(&query)).unwrap();
            });
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .unwrap_err()
                    .sqlstate,
                SqlState::DivisionByZero,
                "grouping: {grouping:?}"
            );
            holder.execute("ROLLBACK").unwrap();
            handle.join().unwrap();
            assert_eq!(
                setup
                    .query("SELECT nextval('ordered_error_values')", &[])
                    .unwrap()
                    .rows,
                vec![vec![Value::Int8(1)]],
                "projection: {projection}, grouping: {grouping:?}"
            );
        }
    }
}

#[test]
fn interleaves_group_having_and_volatile_order_projections() {
    for source_values in ["(1), (2)", "(2), (1)"] {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(&format!(
                r#"
                CREATE SEQUENCE grouped_having_values;
                CREATE TABLE grouped_having_source (id BIGINT);
                CREATE TABLE grouped_having_rows (
                    id BIGINT PRIMARY KEY,
                    value BIGINT NOT NULL
                );
                INSERT INTO grouped_having_source VALUES {source_values};
                INSERT INTO grouped_having_rows VALUES (1, 0);
                CREATE FUNCTION allocate_grouped_having_value() RETURNS TRIGGER AS $$
                BEGIN
                    NEW.value := nextval('grouped_having_values');
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_grouped_having_value
                    BEFORE INSERT ON grouped_having_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_grouped_having_value();
                "#
            ))
            .unwrap();
        assert_eq!(
            session
                .query(
                    "INSERT INTO grouped_having_rows \
                     SELECT id, nextval('grouped_having_values') AS z \
                     FROM grouped_having_source GROUP BY id \
                     HAVING nextval('grouped_having_values') % 2 = 1 \
                     ORDER BY z \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                     RETURNING id, value, nextval('grouped_having_values')",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Int8(2), Value::Int8(5), Value::Int8(6)],
                vec![Value::Int8(1), Value::Int8(7), Value::Int8(8)],
            ],
            "source values: {source_values}"
        );
    }
}

#[test]
fn visits_integer_groups_in_postgres_hash_table_order() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE group_hash_values;
                CREATE TABLE group_hash_source (id BIGINT);
                CREATE TABLE group_hash_rows (id BIGINT PRIMARY KEY, value BIGINT NOT NULL);
                INSERT INTO group_hash_source VALUES (1), (2), (3), (4);
                INSERT INTO group_hash_rows VALUES (1, 0);
                CREATE FUNCTION allocate_group_hash_value() RETURNS TRIGGER AS $$
                BEGIN
                    NEW.value := nextval('group_hash_values');
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_group_hash_value
                    BEFORE INSERT ON group_hash_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_group_hash_value();
                "#,
        )
        .unwrap();

    assert_eq!(
        session
            .query(
                "INSERT INTO group_hash_rows \
                     SELECT id, nextval('group_hash_values') AS z \
                     FROM group_hash_source GROUP BY id \
                     HAVING nextval('group_hash_values') % 2 = 1 \
                     ORDER BY z \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                     RETURNING id, value, nextval('group_hash_values')",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(3), Value::Int8(9), Value::Int8(10)],
            vec![Value::Int8(4), Value::Int8(11), Value::Int8(12)],
            vec![Value::Int8(2), Value::Int8(13), Value::Int8(14)],
            vec![Value::Int8(1), Value::Int8(15), Value::Int8(16)],
        ]
    );
}

#[test]
fn evaluates_aggregate_inputs_in_source_row_order() {
    for (source, expected_rows, next_value) in [
        (
            "(1), (2)",
            vec![
                vec![Value::Int8(1), Value::Int8(1)],
                vec![Value::Int8(2), Value::Int8(2)],
            ],
            3,
        ),
        (
            "(1), (2), (1)",
            vec![vec![Value::Int8(1), Value::Int8(4)]],
            4,
        ),
    ] {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(&format!(
                r#"
                    CREATE SEQUENCE aggregate_source_values;
                    CREATE TABLE aggregate_source_rows (id BIGINT);
                    CREATE TABLE aggregate_result_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO aggregate_source_rows VALUES {source};
                    INSERT INTO aggregate_result_rows VALUES (1, 0);
                    CREATE FUNCTION preserve_aggregate_value() RETURNS TRIGGER AS $$
                    BEGIN
                        RETURN NEW;
                    END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER preserve_aggregate_value
                        BEFORE INSERT ON aggregate_result_rows
                        FOR EACH ROW EXECUTE FUNCTION preserve_aggregate_value();
                    "#
            ))
            .unwrap();
        let having = if source == "(1), (2)" {
            ""
        } else {
            " HAVING count(*) > 1"
        };
        session
            .execute(&format!(
                "INSERT INTO aggregate_result_rows \
                     SELECT id, sum(nextval('aggregate_source_values'))::BIGINT \
                     FROM aggregate_source_rows GROUP BY id{having} ORDER BY id \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value"
            ))
            .unwrap();
        assert_eq!(
            session
                .query(
                    "SELECT id, value FROM aggregate_result_rows ORDER BY id",
                    &[],
                )
                .unwrap()
                .rows,
            expected_rows
        );
        assert_eq!(
            session
                .query("SELECT nextval('aggregate_source_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(next_value)]]
        );
    }
}

#[test]
fn preserves_aggregate_filter_evaluation_across_rows_and_executions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE SEQUENCE aggregate_filter_arguments;
             CREATE SEQUENCE aggregate_filter_checks;
             CREATE TABLE aggregate_filter_rows (id INTEGER, bucket INTEGER);
             INSERT INTO aggregate_filter_rows VALUES (1, 1), (2, 1), (3, 2), (4, 2)",
        )
        .unwrap();
    let query = session
        .prepare(
            "SELECT bucket,
                    (sum(nextval('aggregate_filter_arguments')) FILTER (WHERE id % 2 = 1))::BIGINT,
                    count(*) FILTER (WHERE nextval('aggregate_filter_checks') > 0)
             FROM aggregate_filter_rows GROUP BY bucket ORDER BY bucket",
        )
        .unwrap();
    for first in [1, 3] {
        assert_eq!(
            session.query_prepared(&query, &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int8(first), Value::Int8(2)],
                vec![Value::Int4(2), Value::Int8(first + 1), Value::Int8(2)],
            ]
        );
    }
    assert_eq!(
        session
            .query(
                "SELECT nextval('aggregate_filter_arguments'), nextval('aggregate_filter_checks')",
                &[]
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int8(5), Value::Int8(9)]]
    );
}

#[test]
fn preserves_volatile_aggregate_occurrences_and_prunes_dead_ones() {
    for (projection, expected_rows, next_value) in [
        (
            "(sum(nextval('aggregate_occurrence_values')) + \
                 sum(nextval('aggregate_occurrence_values')))::BIGINT",
            vec![
                vec![Value::Int8(1), Value::Int8(3)],
                vec![Value::Int8(2), Value::Int8(7)],
            ],
            5,
        ),
        (
            "(CASE WHEN FALSE THEN sum(1 / (2 - id)) ELSE 0 END)::BIGINT",
            vec![
                vec![Value::Int8(1), Value::Int8(0)],
                vec![Value::Int8(2), Value::Int8(0)],
            ],
            1,
        ),
    ] {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(
                r#"
                    CREATE SEQUENCE aggregate_occurrence_values;
                    CREATE TABLE aggregate_occurrence_source (id BIGINT);
                    CREATE TABLE aggregate_occurrence_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO aggregate_occurrence_source VALUES (1), (2);
                    INSERT INTO aggregate_occurrence_rows VALUES (1, 0);
                    CREATE FUNCTION preserve_aggregate_occurrence() RETURNS TRIGGER AS $$
                    BEGIN
                        RETURN NEW;
                    END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER preserve_aggregate_occurrence
                        BEFORE INSERT ON aggregate_occurrence_rows
                        FOR EACH ROW EXECUTE FUNCTION preserve_aggregate_occurrence();
                    "#,
            )
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO aggregate_occurrence_rows \
                     SELECT id, {projection} FROM aggregate_occurrence_source \
                     GROUP BY id ORDER BY id \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value"
            ))
            .unwrap();
        assert_eq!(
            session
                .query(
                    "SELECT id, value FROM aggregate_occurrence_rows ORDER BY id",
                    &[],
                )
                .unwrap()
                .rows,
            expected_rows
        );
        assert_eq!(
            session
                .query("SELECT nextval('aggregate_occurrence_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(next_value)]]
        );
    }
}

#[test]
fn preserves_aggregate_occurrence_ownership_and_case_null_semantics() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE aggregate_owner_values;
                CREATE TABLE aggregate_owner_source (id BIGINT);
                INSERT INTO aggregate_owner_source VALUES (1), (2);
                "#,
        )
        .unwrap();

    assert_eq!(
        session
            .query(
                "SELECT id, sum(nextval('aggregate_owner_values'))::BIGINT \
                     FROM aggregate_owner_source GROUP BY id \
                     HAVING sum(nextval('aggregate_owner_values')) > 0 ORDER BY id",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1), Value::Int8(1)],
            vec![Value::Int8(2), Value::Int8(3)],
        ]
    );
    assert_eq!(
        session
            .query("SELECT nextval('aggregate_owner_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(5)]]
    );
    assert_eq!(
        session
            .query(
                "SELECT id, \
                     coalesce(CASE WHEN FALSE THEN sum(id) END, 7)::BIGINT, \
                     (CASE WHEN NULL THEN sum(1 / (2 - id)) ELSE 0 END)::BIGINT, \
                     ((CASE WHEN FALSE THEN sum(id) ELSE 1 END) / 2)::BIGINT, \
                     (coalesce(CASE WHEN FALSE THEN sum(id) END, 1) / 2)::BIGINT \
                     FROM aggregate_owner_source GROUP BY id ORDER BY id",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![
                Value::Int8(1),
                Value::Int8(7),
                Value::Int8(0),
                Value::Int8(1),
                Value::Int8(1),
            ],
            vec![
                Value::Int8(2),
                Value::Int8(7),
                Value::Int8(0),
                Value::Int8(1),
                Value::Int8(1),
            ],
        ]
    );
    session
        .execute("CREATE SEQUENCE aggregate_distinct_values")
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT DISTINCT ON (max(nextval('aggregate_distinct_values'))) \
                     id, max(nextval('aggregate_distinct_values')) \
                     FROM aggregate_owner_source GROUP BY id \
                     ORDER BY max(nextval('aggregate_distinct_values'))",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1), Value::Int8(1)],
            vec![Value::Int8(2), Value::Int8(2)],
        ]
    );
    assert_eq!(
        session
            .query("SELECT nextval('aggregate_distinct_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(3)]]
    );
    session
        .execute("CREATE SEQUENCE aggregate_unordered_distinct_values")
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT DISTINCT ON (max(nextval('aggregate_unordered_distinct_values'))) \
                     id, max(nextval('aggregate_unordered_distinct_values')) \
                     FROM aggregate_owner_source GROUP BY id",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1), Value::Int8(1)],
            vec![Value::Int8(2), Value::Int8(2)],
        ]
    );
    assert_eq!(
        session
            .query("SELECT nextval('aggregate_unordered_distinct_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(3)]]
    );
}

#[test]
fn propagates_limited_union_errors_and_skips_unread_group_projections() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE TABLE limited_union_source (id BIGINT);
                CREATE TABLE limited_union_rows (id BIGINT PRIMARY KEY, value BIGINT NOT NULL);
                INSERT INTO limited_union_source VALUES (1), (2);
                CREATE FUNCTION keep_limited_union_row() RETURNS TRIGGER AS $$
                BEGIN
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER keep_limited_union_row
                    BEFORE INSERT ON limited_union_rows
                    FOR EACH ROW EXECUTE FUNCTION keep_limited_union_row();
                "#,
        )
        .unwrap();

    assert_eq!(
        session
            .query(
                "INSERT INTO limited_union_rows \
                     SELECT id, 1 / (id - 1) FROM limited_union_source GROUP BY id \
                     UNION ALL SELECT 3, 0 LIMIT 1 RETURNING id, value",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2), Value::Int8(1)]]
    );

    session
        .execute(
            r#"
                CREATE FUNCTION reject_limited_union_row() RETURNS TRIGGER AS $$
                BEGIN
                    NEW.value := 1 / 0;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER reject_limited_union_row
                    BEFORE INSERT ON limited_union_rows
                    FOR EACH ROW EXECUTE FUNCTION reject_limited_union_row();
                "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute(
                "INSERT INTO limited_union_rows \
                     SELECT id, 0 FROM limited_union_source \
                     UNION ALL SELECT 3, 0 LIMIT 1"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DivisionByZero
    );

    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE limited_conflict_values;
                CREATE TABLE limited_conflict_source (id BIGINT);
                CREATE TABLE limited_conflict_rows (
                    id BIGINT PRIMARY KEY,
                    value BIGINT NOT NULL
                );
                INSERT INTO limited_conflict_source VALUES (1), (2);
                INSERT INTO limited_conflict_rows VALUES (1, 0);
                CREATE FUNCTION allocate_limited_conflict_value() RETURNS TRIGGER AS $$
                BEGIN
                    NEW.value := nextval('limited_conflict_values');
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_limited_conflict_value
                    BEFORE INSERT ON limited_conflict_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_limited_conflict_value();
                "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute(
                "INSERT INTO limited_conflict_rows \
                     SELECT id, 0 FROM limited_conflict_source \
                     UNION ALL SELECT 3, 0 LIMIT 1 \
                     ON CONFLICT (id) DO UPDATE SET value = 1 / 0"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DivisionByZero
    );
    assert_eq!(
        session
            .query("SELECT nextval('limited_conflict_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
}

#[test]
fn evaluates_volatile_aggregate_inputs_before_trigger_waits() {
    for commits in [true, false] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut holder = db.create_session();
        let mut writer = db.create_session();
        let mut observer = db.create_session();
        holder
            .execute(
                r#"
                    CREATE SEQUENCE aggregate_input_values;
                    CREATE TABLE aggregate_input_source (id BIGINT);
                    CREATE TABLE aggregate_input_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO aggregate_input_source VALUES (1), (2);
                    INSERT INTO aggregate_input_rows VALUES (1, 0);
                    CREATE FUNCTION allocate_aggregate_input_value() RETURNS TRIGGER AS $$
                    BEGIN
                        NEW.value := nextval('aggregate_input_values');
                        RETURN NEW;
                    END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER allocate_aggregate_input_value
                        BEFORE INSERT ON aggregate_input_rows
                        FOR EACH ROW EXECUTE FUNCTION allocate_aggregate_input_value();
                    "#,
            )
            .unwrap();
        holder.execute("BEGIN").unwrap();
        holder
            .execute("UPDATE aggregate_input_rows SET value = 10 WHERE id = 1")
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            sender
                .send(writer.query(
                    "INSERT INTO aggregate_input_rows \
                         SELECT id, sum(nextval('aggregate_input_values')) \
                         FROM aggregate_input_source GROUP BY id ORDER BY id \
                         ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                         RETURNING id, value, nextval('aggregate_input_values')",
                    &[],
                ))
                .unwrap();
        });
        wait_until_blocked(&db);
        assert_eq!(
            observer
                .query("SELECT nextval('aggregate_input_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(4)]],
            "commits: {commits}"
        );
        holder
            .execute(if commits { "COMMIT" } else { "ROLLBACK" })
            .unwrap();
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .rows,
            vec![
                vec![Value::Int8(1), Value::Int8(3), Value::Int8(5)],
                vec![Value::Int8(2), Value::Int8(6), Value::Int8(7)],
            ],
            "commits: {commits}"
        );
        handle.join().unwrap();
    }
}

#[test]
fn stops_insert_select_evaluation_at_limit() {
    for (selection, expected, next) in [
        (
            "1 / (2 - id) > 0",
            vec![vec![Value::Int8(1), Value::Int8(1), Value::Int8(2)]],
            3,
        ),
        (
            "nextval('limited_insert_values') > 0",
            vec![vec![Value::Int8(1), Value::Int8(2), Value::Int8(3)]],
            4,
        ),
    ] {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(
                r#"
                    CREATE SEQUENCE limited_insert_values;
                    CREATE TABLE limited_insert_source (id BIGINT);
                    CREATE TABLE limited_insert_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO limited_insert_source VALUES (1), (2);
                    INSERT INTO limited_insert_rows VALUES (1, 0);
                    CREATE FUNCTION allocate_limited_insert_value() RETURNS TRIGGER AS $$
                    BEGIN
                        NEW.value := nextval('limited_insert_values');
                        RETURN NEW;
                    END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER allocate_limited_insert_value
                        BEFORE INSERT ON limited_insert_rows
                        FOR EACH ROW EXECUTE FUNCTION allocate_limited_insert_value();
                    "#,
            )
            .unwrap();
        assert_eq!(
            session
                .query(
                    &format!(
                        "INSERT INTO limited_insert_rows \
                             SELECT id, 0 FROM limited_insert_source \
                             WHERE {selection} LIMIT 1 \
                             ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                             RETURNING id, value, nextval('limited_insert_values')"
                    ),
                    &[],
                )
                .unwrap()
                .rows,
            expected
        );
        assert_eq!(
            session
                .query("SELECT nextval('limited_insert_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(next)]]
        );
    }
}

#[test]
fn waits_before_later_union_insert_source_errors() {
    for commits in [true, false] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut holder = db.create_session();
        let mut writer = db.create_session();
        let mut observer = db.create_session();
        holder
            .execute(
                r#"
                    CREATE SEQUENCE union_error_trigger_values;
                    CREATE TABLE union_error_source (id BIGINT);
                    CREATE TABLE union_error_rows (
                        id BIGINT PRIMARY KEY,
                        value BIGINT NOT NULL
                    );
                    INSERT INTO union_error_source VALUES (1), (2);
                    INSERT INTO union_error_rows VALUES (1, 0);
                    CREATE FUNCTION allocate_union_error_trigger_value() RETURNS TRIGGER AS $$
                    BEGIN
                        NEW.value := nextval('union_error_trigger_values');
                        RETURN NEW;
                    END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER allocate_union_error_trigger_value
                        BEFORE INSERT ON union_error_rows
                        FOR EACH ROW EXECUTE FUNCTION allocate_union_error_trigger_value();
                    "#,
            )
            .unwrap();
        holder.execute("BEGIN").unwrap();
        holder
            .execute("UPDATE union_error_rows SET value = 10 WHERE id = 1")
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            sender
                .send(writer.execute(
                    "INSERT INTO union_error_rows \
                         SELECT id, 1 / (2 - id) FROM union_error_source WHERE id = 1 \
                         UNION ALL \
                         SELECT id, 1 / (2 - id) FROM union_error_source WHERE id = 2 \
                         ON CONFLICT (id) DO UPDATE SET value = excluded.value",
                ))
                .unwrap();
        });
        wait_until_blocked(&db);
        assert_eq!(
            observer
                .query("SELECT nextval('union_error_trigger_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(2)]]
        );
        holder
            .execute(if commits { "COMMIT" } else { "ROLLBACK" })
            .unwrap();
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .sqlstate,
            SqlState::DivisionByZero
        );
        handle.join().unwrap();
    }
}

#[test]
fn resumes_multirow_trigger_effects_after_conflict_waits() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    let mut observer = db.create_session();
    holder
        .execute(
            r#"
                CREATE SEQUENCE resumed_trigger_values;
                CREATE TABLE resumed_trigger_rows (
                    id BIGINT PRIMARY KEY,
                    value BIGINT NOT NULL
                );
                INSERT INTO resumed_trigger_rows VALUES (1, 0);
                CREATE FUNCTION allocate_resumed_trigger_value() RETURNS TRIGGER AS $$
                BEGIN NEW.value := nextval('resumed_trigger_values'); RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_resumed_trigger_value
                    BEFORE INSERT ON resumed_trigger_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_resumed_trigger_value();
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("UPDATE resumed_trigger_rows SET value = 10 WHERE id = 1")
        .unwrap();

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.query(
                "INSERT INTO resumed_trigger_rows VALUES (1, 0), (2, 0) \
                     ON CONFLICT DO NOTHING \
                     RETURNING id, value, nextval('resumed_trigger_values')",
                &[],
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    assert_eq!(
        observer
            .query("SELECT nextval('resumed_trigger_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2), Value::Int8(3), Value::Int8(4)]]
    );
    handle.join().unwrap();
}

#[test]
fn resumes_after_skipped_trigger_rows_and_default_values() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    let mut observer = db.create_session();
    holder
        .execute(
            r#"
                CREATE SEQUENCE skipped_trigger_values;
                CREATE TABLE skipped_trigger_rows (
                    id BIGINT PRIMARY KEY,
                    action TEXT NOT NULL,
                    value BIGINT NOT NULL
                );
                INSERT INTO skipped_trigger_rows VALUES (2, 'held', 0);
                CREATE FUNCTION allocate_or_skip_trigger_value() RETURNS TRIGGER AS $$
                BEGIN
                    NEW.value := nextval('skipped_trigger_values');
                    IF NEW.action = 'skip' THEN RETURN NULL; END IF;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_or_skip_trigger_value
                    BEFORE INSERT ON skipped_trigger_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_or_skip_trigger_value();
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("UPDATE skipped_trigger_rows SET value = 10 WHERE id = 2")
        .unwrap();

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.query(
                "INSERT INTO skipped_trigger_rows VALUES \
                         (1, 'skip', 0), (2, 'keep', 0), (3, 'keep', 0) \
                     ON CONFLICT DO NOTHING RETURNING id, value",
                &[],
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    assert_eq!(
        observer
            .query("SELECT nextval('skipped_trigger_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(3)]]
    );
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .rows,
        vec![vec![Value::Int8(3), Value::Int8(4)]]
    );
    handle.join().unwrap();

    observer
        .execute(
            r#"
                CREATE SEQUENCE default_trigger_values;
                CREATE TABLE default_trigger_rows (
                    id BIGINT PRIMARY KEY DEFAULT 1,
                    value BIGINT NOT NULL DEFAULT 0
                );
                INSERT INTO default_trigger_rows VALUES (1, 0);
                CREATE FUNCTION allocate_default_trigger_value() RETURNS TRIGGER AS $$
                BEGIN NEW.value := nextval('default_trigger_values'); RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_default_trigger_value
                    BEFORE INSERT ON default_trigger_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_default_trigger_value();
                "#,
        )
        .unwrap();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("UPDATE default_trigger_rows SET value = 10 WHERE id = 1")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.query(
                "INSERT INTO default_trigger_rows DEFAULT VALUES \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value \
                     RETURNING value",
                &[],
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    assert_eq!(
        observer
            .query("SELECT nextval('default_trigger_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    holder.execute("ROLLBACK").unwrap();
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
    handle.join().unwrap();
}

#[test]
fn preserves_trigger_order_across_insert_cte_foreign_key_waits() {
    for (source_kind, commits) in [
        (0, true),
        (0, false),
        (1, true),
        (1, false),
        (2, true),
        (2, false),
    ] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut holder = db.create_session();
        let mut writer = db.create_session();
        let mut observer = db.create_session();
        holder
            .execute(
                r#"
                    CREATE SEQUENCE cte_parent_values;
                    CREATE TABLE cte_trigger_parents (id BIGINT PRIMARY KEY);
                    CREATE TABLE cte_trigger_children (
                        id BIGINT PRIMARY KEY,
                        parent_id BIGINT REFERENCES cte_trigger_parents
                    );
                    CREATE TABLE cte_trigger_source (id BIGINT PRIMARY KEY);
                    INSERT INTO cte_trigger_parents VALUES (1), (2);
                    INSERT INTO cte_trigger_source VALUES (1), (2);
                    CREATE FUNCTION allocate_cte_parent() RETURNS TRIGGER AS $$
                    BEGIN NEW.parent_id := nextval('cte_parent_values'); RETURN NEW; END;
                    $$ LANGUAGE plpgsql;
                    CREATE TRIGGER allocate_cte_parent
                        BEFORE INSERT ON cte_trigger_children
                        FOR EACH ROW EXECUTE FUNCTION allocate_cte_parent();
                    "#,
            )
            .unwrap();
        holder.execute("BEGIN").unwrap();
        holder
            .execute("DELETE FROM cte_trigger_parents WHERE id = 2")
            .unwrap();

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let query = if source_kind == 1 {
                "WITH first_insert AS ( \
                         INSERT INTO cte_trigger_children SELECT 1, 0 \
                         RETURNING id, parent_id \
                     ), second_insert AS ( \
                         INSERT INTO cte_trigger_children SELECT 2, 0 \
                         RETURNING id, parent_id \
                     ) \
                     SELECT * FROM first_insert UNION ALL SELECT * FROM second_insert"
            } else if source_kind == 2 {
                "WITH first_insert AS ( \
                         INSERT INTO cte_trigger_children \
                         SELECT id, 0 FROM cte_trigger_source WHERE id = 1 \
                         RETURNING id, parent_id \
                     ), second_insert AS ( \
                         INSERT INTO cte_trigger_children \
                         SELECT id, 0 FROM cte_trigger_source WHERE id = 2 \
                         RETURNING id, parent_id \
                     ) \
                     SELECT * FROM first_insert UNION ALL SELECT * FROM second_insert"
            } else {
                "WITH first_insert AS ( \
                         INSERT INTO cte_trigger_children VALUES (1, 0) \
                         RETURNING id, parent_id \
                     ), second_insert AS ( \
                         INSERT INTO cte_trigger_children VALUES (2, 0) \
                         RETURNING id, parent_id \
                     ) \
                     SELECT * FROM first_insert UNION ALL SELECT * FROM second_insert"
            };
            sender.send(writer.query(query, &[])).unwrap();
        });
        wait_until_blocked(&db);
        assert_eq!(
            observer
                .query("SELECT nextval('cte_parent_values')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(3)]]
        );
        holder
            .execute(if commits { "COMMIT" } else { "ROLLBACK" })
            .unwrap();
        let result = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        if commits {
            assert_eq!(result.unwrap_err().sqlstate, SqlState::ForeignKeyViolation);
            assert!(
                observer
                    .query("SELECT * FROM cte_trigger_children", &[])
                    .unwrap()
                    .rows
                    .is_empty()
            );
        } else {
            assert_eq!(
                result.unwrap().rows,
                vec![
                    vec![Value::Int8(1), Value::Int8(1)],
                    vec![Value::Int8(2), Value::Int8(2)],
                ]
            );
        }
        handle.join().unwrap();
    }
}

#[test]
fn does_not_lock_trigger_functions_for_reads() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_millis(50))
        .build();
    let mut replacer = db.create_session();
    let mut reader = db.create_session();
    replacer
        .execute(
            r#"
                CREATE TABLE trigger_read_items (id INTEGER);
                CREATE FUNCTION trigger_read_function() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER trigger_read BEFORE INSERT ON trigger_read_items
                    FOR EACH ROW EXECUTE FUNCTION trigger_read_function();
                INSERT INTO trigger_read_items VALUES (1);
                "#,
        )
        .unwrap();
    replacer.execute("BEGIN").unwrap();
    replacer
        .execute(
            r#"CREATE OR REPLACE FUNCTION trigger_read_function() RETURNS TRIGGER AS $$
                BEGIN RETURN NULL; END;
                $$ LANGUAGE plpgsql"#,
        )
        .unwrap();
    assert_eq!(
        reader
            .query("SELECT id FROM trigger_read_items", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    replacer.execute("ROLLBACK").unwrap();
}

#[test]
fn locks_foreign_keys_after_before_insert_triggers() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE TABLE triggered_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE triggered_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES triggered_parents,
                    action TEXT NOT NULL
                );
                CREATE FUNCTION rewrite_parent_key() RETURNS TRIGGER AS $$
                BEGIN
                    IF NEW.action = 'skip' THEN
                        RETURN NULL;
                    ELSIF NEW.action = 'one' THEN
                        NEW.parent_id := 1;
                    ELSIF NEW.action = 'three' THEN
                        NEW.parent_id := 3;
                    ELSIF NEW.action = 'four' THEN
                        NEW.parent_id := 4;
                    END IF;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER rewrite_parent_key BEFORE INSERT ON triggered_children
                    FOR EACH ROW EXECUTE FUNCTION rewrite_parent_key();
                INSERT INTO triggered_parents VALUES (1), (2);
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("SELECT id FROM triggered_parents WHERE id = 1 FOR UPDATE")
        .unwrap();
    assert_eq!(
        writer
            .execute("INSERT INTO triggered_children VALUES (1, 2, 'two')")
            .unwrap(),
        create_affected_results(1)
    );
    assert_eq!(
        writer
            .execute("INSERT INTO triggered_children VALUES (2, 1, 'skip')")
            .unwrap(),
        create_affected_results(0)
    );

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute("INSERT INTO triggered_children VALUES (3, 2, 'one')"))
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();

    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("INSERT INTO triggered_parents VALUES (3)")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute("INSERT INTO triggered_children VALUES (4, 2, 'three')"))
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();

    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("INSERT INTO triggered_parents VALUES (4)")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(
                writer
                    .execute("INSERT INTO triggered_children VALUES (5, 2, 'four')")
                    .unwrap_err()
                    .sqlstate,
            )
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("ROLLBACK").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        SqlState::ForeignKeyViolation
    );
    handle.join().unwrap();
}

#[test]
fn locks_foreign_keys_after_before_update_triggers() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE TABLE update_trigger_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE update_trigger_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES update_trigger_parents,
                    action TEXT NOT NULL
                );
                CREATE FUNCTION rewrite_updated_parent() RETURNS TRIGGER AS $$
                BEGIN
                    IF NEW.action = 'skip' THEN
                        RETURN NULL;
                    ELSIF NEW.action = 'move' THEN
                        NEW.parent_id := 2;
                    END IF;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER rewrite_updated_parent
                    BEFORE UPDATE ON update_trigger_children
                    FOR EACH ROW EXECUTE FUNCTION rewrite_updated_parent();
                INSERT INTO update_trigger_parents VALUES (1), (2);
                INSERT INTO update_trigger_children VALUES (1, 1, 'keep');
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("DELETE FROM update_trigger_parents WHERE id = 2")
        .unwrap();

    assert_eq!(
        writer
            .execute("UPDATE update_trigger_children SET action = 'skip' WHERE id = 1",)
            .unwrap(),
        create_affected_results(0)
    );

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(
                writer
                    .execute("UPDATE update_trigger_children SET action = 'move' WHERE id = 1")
                    .unwrap_err()
                    .sqlstate,
            )
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        SqlState::ForeignKeyViolation
    );
    handle.join().unwrap();

    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute("INSERT INTO update_trigger_parents VALUES (2)")
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("DELETE FROM update_trigger_parents WHERE id = 2")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute("UPDATE update_trigger_children SET action = 'move' WHERE id = 1"))
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("ROLLBACK").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
}

#[test]
fn recomputes_before_update_triggers_after_target_lock_waits() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE TABLE stale_update_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE stale_update_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES stale_update_parents,
                    value INTEGER NOT NULL
                );
                CREATE FUNCTION preserve_updated_row() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_updated_row BEFORE UPDATE ON stale_update_children
                    FOR EACH ROW EXECUTE FUNCTION preserve_updated_row();
                INSERT INTO stale_update_parents VALUES (1);
                INSERT INTO stale_update_children VALUES (1, 1, 0);
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("UPDATE stale_update_children SET value = 10 WHERE id = 1")
        .unwrap();

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute("UPDATE stale_update_children SET value = value + 1 WHERE id = 1"))
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
    assert_eq!(
        holder
            .query("SELECT value FROM stale_update_children", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(11)]]
    );
}

#[test]
fn locks_foreign_keys_after_on_conflict_update_triggers() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE TABLE conflict_trigger_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE conflict_trigger_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES conflict_trigger_parents,
                    action TEXT NOT NULL
                );
                CREATE FUNCTION rewrite_conflict_parent() RETURNS TRIGGER AS $$
                BEGIN
                    IF NEW.action = 'skip' THEN
                        RETURN NULL;
                    ELSIF NEW.action = 'move' THEN
                        NEW.parent_id := 2;
                    END IF;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER rewrite_conflict_parent
                    BEFORE UPDATE ON conflict_trigger_children
                    FOR EACH ROW EXECUTE FUNCTION rewrite_conflict_parent();
                INSERT INTO conflict_trigger_parents VALUES (1), (2);
                INSERT INTO conflict_trigger_children VALUES (1, 1, 'keep');
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("DELETE FROM conflict_trigger_parents WHERE id = 2")
        .unwrap();

    assert_eq!(
        writer
            .execute(
                "INSERT INTO conflict_trigger_children VALUES (1, 1, 'skip') \
                     ON CONFLICT (id) DO UPDATE SET action = excluded.action",
            )
            .unwrap(),
        create_affected_results(0)
    );

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(
                writer
                    .execute(
                        "INSERT INTO conflict_trigger_children VALUES (1, 1, 'move') \
                             ON CONFLICT (id) DO UPDATE SET action = excluded.action",
                    )
                    .unwrap_err()
                    .sqlstate,
            )
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        SqlState::ForeignKeyViolation
    );
    handle.join().unwrap();

    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute("INSERT INTO conflict_trigger_parents VALUES (2)")
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("DELETE FROM conflict_trigger_parents WHERE id = 2")
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute(
                "INSERT INTO conflict_trigger_children VALUES (1, 1, 'move') \
                     ON CONFLICT (id) DO UPDATE SET action = excluded.action",
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("ROLLBACK").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
}

#[test]
fn locks_triggered_update_from_keys_without_false_upsert_waits() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE TABLE joined_trigger_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE joined_trigger_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES joined_trigger_parents,
                    value INTEGER NOT NULL
                );
                CREATE FUNCTION rewrite_joined_parent() RETURNS TRIGGER AS $$
                BEGIN
                    IF NEW.value = 9 THEN RETURN NULL; END IF;
                    NEW.parent_id := 2;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER rewrite_joined_parent BEFORE UPDATE ON joined_trigger_children
                    FOR EACH ROW EXECUTE FUNCTION rewrite_joined_parent();
                INSERT INTO joined_trigger_parents VALUES (1), (2);
                INSERT INTO joined_trigger_children VALUES (1, 1, 0);
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("DELETE FROM joined_trigger_parents WHERE id = 2")
        .unwrap();

    assert_eq!(
        writer
            .execute(
                "INSERT INTO joined_trigger_children VALUES (1, 2, 9) \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
            )
            .unwrap(),
        create_affected_results(0)
    );

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(
                writer
                    .execute(
                        "UPDATE joined_trigger_children SET value = 1 \
                             FROM (VALUES (1)) source(id) \
                             WHERE joined_trigger_children.id = source.id",
                    )
                    .unwrap_err()
                    .sqlstate,
            )
            .unwrap();
    });
    wait_until_blocked(&db);
    holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        SqlState::ForeignKeyViolation
    );
    handle.join().unwrap();
}

#[test]
fn rechecks_upsert_trigger_keys_after_uncommitted_conflicts() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut conflict_holder = db.create_session();
    let mut parent_holder = db.create_session();
    let mut writer = db.create_session();
    conflict_holder
        .execute(
            r#"
                CREATE TABLE delayed_conflict_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE delayed_conflict_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES delayed_conflict_parents,
                    value INTEGER NOT NULL
                );
                CREATE FUNCTION rewrite_delayed_conflict_parent() RETURNS TRIGGER AS $$
                BEGIN NEW.parent_id := 2; RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER rewrite_delayed_conflict_parent
                    BEFORE UPDATE ON delayed_conflict_children
                    FOR EACH ROW EXECUTE FUNCTION rewrite_delayed_conflict_parent();
                INSERT INTO delayed_conflict_parents VALUES (1), (2);
                "#,
        )
        .unwrap();
    conflict_holder.execute("BEGIN").unwrap();
    conflict_holder
        .execute("INSERT INTO delayed_conflict_children VALUES (1, 1, 0)")
        .unwrap();
    parent_holder.execute("BEGIN").unwrap();
    parent_holder
        .execute("DELETE FROM delayed_conflict_parents WHERE id = 2")
        .unwrap();

    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
                .send(
                    writer
                        .execute(
                            "INSERT INTO delayed_conflict_children VALUES (1, 1, 0) \
                             ON CONFLICT (id) DO UPDATE SET value = delayed_conflict_children.value + 1",
                        )
                        .unwrap_err()
                        .sqlstate,
                )
                .unwrap();
    });
    wait_until_blocked(&db);
    conflict_holder.execute("COMMIT").unwrap();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    parent_holder.execute("COMMIT").unwrap();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        SqlState::ForeignKeyViolation
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rechecks_concurrent_on_conflict_updates_for_each_isolation_level() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT UNIQUE)")
        .unwrap();
    first.execute("BEGIN").unwrap();
    first
        .execute("INSERT INTO items VALUES (1, 'old')")
        .unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(second.execute(
                "INSERT INTO items VALUES (1, 'committed') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();

    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("BEGIN").unwrap();
    first
        .execute("INSERT INTO items VALUES (2, 'old')")
        .unwrap();
    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(second.execute(
                "INSERT INTO items VALUES (2, 'after rollback') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
            ))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("ROLLBACK").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();

    let mut first = db.create_session();
    let mut second = db.create_session();
    second
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    second.query("SELECT * FROM items", &[]).unwrap();
    first.execute("BEGIN").unwrap();
    first
        .execute("UPDATE items SET value = 'holder' WHERE id = 1")
        .unwrap();
    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let error = second
            .execute(
                "INSERT INTO items VALUES (1, 'repeatable') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
            )
            .unwrap_err();
        second.execute("ROLLBACK").unwrap();
        result_sender.send(error.sqlstate).unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        SqlState::SerializationFailure
    );
    handle.join().unwrap();

    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("BEGIN").unwrap();
    first
        .execute("INSERT INTO items VALUES (3, 'reserved')")
        .unwrap();
    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let error = second
            .execute(
                "INSERT INTO items VALUES (1, 'reserved') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
            )
            .unwrap_err();
        result_sender.send(error.sqlstate).unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        SqlState::UniqueViolation
    );
    handle.join().unwrap();

    assert_eq!(
        first
            .query("SELECT id, value FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(1), Value::Text("holder".into())],
            vec![Value::Int4(2), Value::Text("after rollback".into())],
            vec![Value::Int4(3), Value::Text("reserved".into())],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn aborts_newest_deadlocked_transaction_and_allows_survivor() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut setup = db.create_session();
    let mut first = db.create_session();
    let mut second = db.create_session();
    setup
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
        .unwrap();
    setup
        .execute("INSERT INTO items VALUES (1, 0), (2, 0)")
        .unwrap();

    first.execute("BEGIN").unwrap();
    first
        .execute("UPDATE items SET amount = 10 WHERE id = 1")
        .unwrap();
    second.execute("BEGIN").unwrap();
    second
        .execute("UPDATE items SET amount = 20 WHERE id = 2")
        .unwrap();

    let (victim_sender, victim_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let error = second
            .execute("UPDATE items SET amount = 11 WHERE id = 1")
            .unwrap_err();
        let abort_with_error = second.query("SELECT * FROM items", &[]).unwrap_err();
        second.execute("ROLLBACK").unwrap();
        victim_sender
            .send((error.sqlstate, abort_with_error.sqlstate))
            .unwrap();
    });
    wait_until_blocked(&db);

    assert_eq!(
        first.execute("UPDATE items SET amount = 1 WHERE id = 2"),
        Ok(create_affected_results(1))
    );
    assert_eq!(
        victim_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        (SqlState::DeadlockDetected, SqlState::InFailedSqlTransaction)
    );
    handle.join().unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        setup
            .query("SELECT amount FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(10)], vec![Value::Int4(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn fails_repeatable_read_writer_after_concurrent_commit() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
        .unwrap();
    first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .unwrap();
    second.query("SELECT * FROM items", &[]).unwrap();
    first.execute("BEGIN").unwrap();
    first.execute("UPDATE items SET amount = 2").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let error = second
            .execute("UPDATE items SET amount = amount + 1 WHERE id = 1")
            .unwrap_err();
        second.execute("ROLLBACK").unwrap();
        result_sender.send(error.sqlstate).unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        SqlState::SerializationFailure
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn applies_update_and_share_row_lock_compatibility() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    let mut third = db.create_session();
    first.execute("CREATE TABLE items (id INTEGER)").unwrap();
    first.execute("INSERT INTO items VALUES (1)").unwrap();
    first.execute("BEGIN").unwrap();
    second.execute("BEGIN").unwrap();
    first.query("SELECT * FROM items FOR SHARE", &[]).unwrap();
    second.query("SELECT * FROM items FOR SHARE", &[]).unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(third.execute("DELETE FROM items WHERE id = 1"))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();
    assert!(matches!(
        result_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    second.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();

    first.execute("INSERT INTO items VALUES (2)").unwrap();
    first.execute("BEGIN").unwrap();
    first
        .query("SELECT * FROM items WHERE id = 2 FOR UPDATE", &[])
        .unwrap();
    let mut writer = db.create_session();
    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(writer.execute("UPDATE items SET id = 3 WHERE id = 2"))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn controls_waits_with_builder_and_session_lock_timeouts() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_millis(40))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    assert_eq!(second.settings.lock_timeout, Duration::from_millis(40));
    second.execute("SET lock_timeout = 250").unwrap();
    assert_eq!(second.settings.lock_timeout, Duration::from_millis(250));
    second.execute("SET lock_timeout = '2s'").unwrap();
    assert_eq!(second.settings.lock_timeout, Duration::from_secs(2));
    second.execute("SET lock_timeout = '20ms'").unwrap();
    assert_eq!(second.settings.lock_timeout, Duration::from_millis(20));

    first.execute("CREATE TABLE items (id INTEGER)").unwrap();
    first.execute("INSERT INTO items VALUES (1)").unwrap();
    first.execute("BEGIN").unwrap();
    first.execute("UPDATE items SET id = 2").unwrap();
    let started = Instant::now();
    assert_eq!(
        second
            .execute("UPDATE items SET id = 3")
            .unwrap_err()
            .sqlstate,
        SqlState::LockNotAvailable
    );
    assert!(started.elapsed() >= Duration::from_millis(10));
    first.execute("ROLLBACK").unwrap();
    second.execute("SET lock_timeout = 0").unwrap();
    assert_eq!(second.settings.lock_timeout, Duration::ZERO);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn restores_row_after_rolled_back_update() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
        .unwrap();
    session.execute("INSERT INTO items VALUES (1, 1)").unwrap();

    session.execute("BEGIN").unwrap();
    session.execute("UPDATE items SET amount = 2").unwrap();
    session.execute("ROLLBACK").unwrap();

    assert_eq!(
        session.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1), Value::Int4(1)]]
    );
    assert_eq!(
        session.execute("DELETE FROM items").unwrap(),
        create_affected_results(1)
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn deletes_matching_rows_and_all_rows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (1, 2), (2, NULL), (3, 4)")
        .unwrap();

    assert_eq!(
        session
            .execute("DELETE FROM items WHERE amount > 2")
            .unwrap(),
        create_affected_results(1)
    );
    assert_eq!(
        session.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Null],
        ]
    );
    assert_eq!(
        session.execute("DELETE FROM items").unwrap(),
        create_affected_results(2)
    );
    assert!(
        session
            .query("SELECT * FROM items", &[])
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn matches_delete_visibility_to_transaction_outcome() {
    let db = Db::create();
    let mut writer = db.create_session();
    let mut reader = db.create_session();
    writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
    writer
        .execute("INSERT INTO items VALUES (1), (2), (3)")
        .unwrap();

    writer.execute("BEGIN").unwrap();
    assert_eq!(
        writer.execute("DELETE FROM items WHERE id = 1").unwrap(),
        create_affected_results(1)
    );
    assert_eq!(
        writer.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
    );
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );
    writer.execute("ROLLBACK").unwrap();
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(3)]
        ]
    );

    writer.execute("BEGIN").unwrap();
    writer.execute("DELETE FROM items WHERE id = 2").unwrap();
    writer.execute("COMMIT").unwrap();
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(3)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn delete_requires_a_boolean_where_expression() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();

    assert_eq!(
        session
            .execute("DELETE FROM items WHERE id")
            .unwrap_err()
            .sqlstate,
        SqlState::DatatypeMismatch
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        session.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn aborts_explicit_transactions_after_errors_and_rolls_back_on_drop() {
    let db = Db::create();
    let mut session = db.create_session();
    let mut reader = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();

    session.execute("BEGIN").unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO missing VALUES (1)")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        session
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InFailedSqlTransaction
    );
    session.execute("ROLLBACK").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();

    {
        let mut transaction = session.begin().unwrap();
        transaction.execute("INSERT INTO items VALUES (2)").unwrap();
    }
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    let mut transaction = session.begin().unwrap();
    transaction.execute("INSERT INTO items VALUES (3)").unwrap();
    transaction.commit().unwrap();
    assert_eq!(
        reader.query("SELECT * FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(3)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rolls_back_created_tables_rows_constraints_and_sequences() {
    let db = Db::create();
    let mut session = db.create_session();
    let mut reader = db.create_session();

    session.execute("BEGIN").unwrap();
    assert_eq!(
        session
            .execute("CREATE TABLE items (id SERIAL PRIMARY KEY, value INTEGER UNIQUE)")
            .unwrap(),
        create_affected_results(0)
    );
    session
        .execute("INSERT INTO items (value) VALUES (10)")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT id, value FROM items", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Int4(10)]]
    );
    assert_eq!(
        reader
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        session
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        session
            .query("SELECT nextval('items_id_seq')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rolls_back_dropped_tables_and_keeps_sequence_allocations() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id SERIAL PRIMARY KEY, value INTEGER UNIQUE)")
        .unwrap();
    session
        .execute("INSERT INTO items (value) VALUES (10)")
        .unwrap();

    session.execute("BEGIN").unwrap();
    assert_eq!(
        session
            .query("SELECT nextval('items_id_seq')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    session.execute("DROP TABLE items").unwrap();
    assert_eq!(
        session
            .query("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    session.execute("ROLLBACK").unwrap();

    assert_eq!(
        session
            .query("SELECT id, value FROM items", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Int4(10)]]
    );
    assert_eq!(
        session
            .query("SELECT nextval('items_id_seq')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(3)]]
    );
    assert_eq!(
        session
            .execute("INSERT INTO items (id, value) VALUES (1, 20)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rolls_back_ddl_after_a_later_statement_failure() {
    let db = Db::create();
    let mut session = db.create_session();

    session.execute("BEGIN").unwrap();
    session
        .execute("CREATE TABLE transient (id INTEGER)")
        .unwrap();
    assert_eq!(
        session.execute("SELECT 1 / 0").unwrap_err().sqlstate,
        SqlState::DivisionByZero
    );
    assert_eq!(
        session
            .query("SELECT * FROM transient", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InFailedSqlTransaction
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        session
            .query("SELECT * FROM transient", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rolls_back_partial_multi_relation_ddl_failure() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE first (id INTEGER)").unwrap();
    session.execute("INSERT INTO first VALUES (1)").unwrap();

    session.execute("BEGIN").unwrap();
    assert_eq!(
        session
            .execute("DROP TABLE first, missing")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    session.execute("ROLLBACK").unwrap();

    assert_eq!(
        session.query("SELECT * FROM first", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
fn reuses_prepared_read_locks_while_ddl_waits() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut reader = db.create_session();
    reader
        .execute("CREATE TABLE held_read (id INTEGER); INSERT INTO held_read VALUES (1)")
        .unwrap();
    let statement = reader.prepare("SELECT id FROM held_read").unwrap();
    reader.execute("BEGIN").unwrap();
    reader.query_prepared(&statement, &[]).unwrap();
    let mut writer = db.create_session();
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        sender
            .send(writer.execute("ALTER TABLE held_read ADD COLUMN extra INTEGER"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    for _ in 0..3 {
        assert_eq!(
            reader.query_prepared(&statement, &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    reader.execute("COMMIT").unwrap();
    receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
    assert_eq!(
        reader.query_prepared(&statement, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn serializes_concurrent_relation_creation() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut creator = db.create_session();
    let mut contender = db.create_session();
    creator.execute("BEGIN").unwrap();
    creator.execute("CREATE TABLE items (id INTEGER)").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        contender.execute("BEGIN").unwrap();
        let result = contender.execute("CREATE TABLE items (id INTEGER)");
        contender.execute("ROLLBACK").unwrap();
        result_sender.send(result).unwrap();
    });
    wait_until_relation_blocked(&db);
    creator.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .sqlstate,
        SqlState::DuplicateTable
    );
    handle.join().unwrap();
    assert!(creator.query("SELECT * FROM items", &[]).is_ok());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn allows_concurrent_creation_after_the_first_creator_rolls_back() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut creator = db.create_session();
    let mut contender = db.create_session();
    creator.execute("BEGIN").unwrap();
    creator.execute("CREATE SEQUENCE ids START 10").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(contender.execute("CREATE SEQUENCE ids START 20"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    creator.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
    assert_eq!(
        creator.query("SELECT nextval('ids')", &[]).unwrap().rows,
        vec![vec![Value::Int8(20)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn serializes_dependency_creation_against_table_drop() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut creator = db.create_session();
    let mut dropper = db.create_session();
    creator
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    creator.execute("BEGIN").unwrap();
    creator
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
        .unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP TABLE parents"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    creator.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    handle.join().unwrap();
    assert!(creator.query("SELECT * FROM parents", &[]).is_ok());
    assert!(creator.query("SELECT * FROM children", &[]).is_ok());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn blocks_sequence_drop_while_an_explicit_transaction_uses_it() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut user = db.create_session();
    let mut dropper = db.create_session();
    user.execute("CREATE SEQUENCE ids").unwrap();
    user.execute("BEGIN").unwrap();
    assert_eq!(
        user.query("SELECT nextval('ids')", &[]).unwrap().rows,
        vec![vec![Value::Int8(1)]]
    );

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP SEQUENCE ids"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    user.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
    assert_eq!(
        user.query("SELECT nextval('ids')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn locks_sequences_used_by_column_defaults() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut user = db.create_session();
    let mut dropper = db.create_session();
    user.execute("CREATE SEQUENCE ids").unwrap();
    user.execute("CREATE TABLE generated (id BIGINT DEFAULT nextval('ids'))")
        .unwrap();
    user.execute("BEGIN").unwrap();
    user.execute("INSERT INTO generated DEFAULT VALUES")
        .unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP SEQUENCE ids"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    user.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    handle.join().unwrap();
    assert!(user.query("SELECT nextval('ids')", &[]).is_ok());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn locks_sequences_while_creating_column_defaults() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut creator = db.create_session();
    let mut dropper = db.create_session();
    creator.execute("CREATE SEQUENCE ids").unwrap();
    creator.execute("BEGIN").unwrap();
    creator
        .execute("CREATE TABLE generated (id BIGINT DEFAULT nextval('ids'))")
        .unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP SEQUENCE ids"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    creator.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    handle.join().unwrap();
    assert!(creator.query("SELECT nextval('ids')", &[]).is_ok());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rejects_cross_session_temporary_default_dependencies() {
    let db = Db::create();
    let mut temporary_user = db.create_session();
    let mut dropper = db.create_session();
    temporary_user
        .execute("CREATE SEQUENCE public.shared_ids")
        .unwrap();

    assert_eq!(
        temporary_user
            .execute(
                "CREATE TEMP TABLE generated \
                     (id BIGINT DEFAULT nextval('public.shared_ids'))",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        dropper.execute("DROP SEQUENCE public.shared_ids").unwrap(),
        create_affected_results(0)
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn blocks_sequence_drop_for_late_bound_sequence_names() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut user = db.create_session();
    let mut dropper = db.create_session();
    user.execute("CREATE SEQUENCE ids").unwrap();
    user.execute("BEGIN").unwrap();
    assert_eq!(
        user.query("SELECT nextval('ids'::text)", &[]).unwrap().rows,
        vec![vec![Value::Int8(1)]]
    );

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP SEQUENCE ids"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    user.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn blocks_sequence_drop_for_parameterized_sequence_names() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut user = db.create_session();
    let mut dropper = db.create_session();
    user.execute("CREATE SEQUENCE ids").unwrap();
    let next_value = user.prepare("SELECT nextval($1)").unwrap();
    user.execute("BEGIN").unwrap();
    assert_eq!(
        user.query_prepared(&next_value, &[Value::Text("ids".into())])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP SEQUENCE ids"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    user.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn blocks_parent_drop_while_child_dml_uses_the_foreign_key() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut writer = db.create_session();
    let mut dropper = db.create_session();
    writer
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    writer
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
        .unwrap();
    writer.execute("INSERT INTO parents VALUES (1)").unwrap();
    writer.execute("BEGIN").unwrap();
    writer.execute("INSERT INTO children VALUES (1)").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP TABLE parents"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    writer.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn blocks_child_drop_while_parent_dml_cascades_to_it() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut writer = db.create_session();
    let mut dropper = db.create_session();
    writer
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    writer
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents ON DELETE CASCADE)")
        .unwrap();
    writer.execute("INSERT INTO parents VALUES (1)").unwrap();
    writer.execute("INSERT INTO children VALUES (1)").unwrap();
    writer.execute("BEGIN").unwrap();
    writer.execute("DELETE FROM parents WHERE id = 1").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP TABLE children"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    writer.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn does_not_block_child_drop_for_parent_insert() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut writer = db.create_session();
    let mut dropper = db.create_session();
    writer
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    writer
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
        .unwrap();
    writer.execute("BEGIN").unwrap();
    writer.execute("INSERT INTO parents VALUES (1)").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP TABLE children"))
            .unwrap();
    });

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    writer.execute("ROLLBACK").unwrap();
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn does_not_block_child_drop_for_unrelated_parent_update() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut writer = db.create_session();
    let mut dropper = db.create_session();
    writer
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();
    writer
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
        .unwrap();
    writer
        .execute("INSERT INTO parents VALUES (1, 10)")
        .unwrap();
    writer.execute("BEGIN").unwrap();
    writer
        .execute("UPDATE parents SET value = 20 WHERE id = 1")
        .unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP TABLE children"))
            .unwrap();
    });

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    writer.execute("ROLLBACK").unwrap();
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rejects_computed_sequence_names() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE SEQUENCE first_ids").unwrap();
    session.execute("CREATE SEQUENCE second_ids").unwrap();

    assert_eq!(
        session
            .query(
                "SELECT nextval(CASE WHEN true THEN 'first_ids' ELSE 'second_ids' END)",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    session.execute("DROP SEQUENCE second_ids").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn lets_parent_drop_continue_after_child_drop_commits() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut child_dropper = db.create_session();
    let mut parent_dropper = db.create_session();
    child_dropper
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    child_dropper
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
        .unwrap();
    child_dropper.execute("BEGIN").unwrap();
    child_dropper.execute("DROP TABLE children").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(parent_dropper.execute("DROP TABLE parents"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    child_dropper.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_parent_dependency_after_child_drop_rolls_back() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut child_dropper = db.create_session();
    let mut parent_dropper = db.create_session();
    child_dropper
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    child_dropper
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
        .unwrap();
    child_dropper.execute("BEGIN").unwrap();
    child_dropper.execute("DROP TABLE children").unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(parent_dropper.execute("DROP TABLE parents"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    child_dropper.execute("ROLLBACK").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn wakes_table_drop_after_read_only_commit() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(10))
        .build();
    let mut reader = db.create_session();
    let mut dropper = db.create_session();
    reader.execute("CREATE TABLE items (id INTEGER)").unwrap();
    reader.execute("BEGIN").unwrap();
    reader.query("SELECT * FROM items", &[]).unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(dropper.execute("DROP TABLE items"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    reader.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn serializes_alter_table_with_active_readers() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(10))
        .build();
    let mut reader = db.create_session();
    let mut changer = db.create_session();
    reader
        .execute("CREATE TABLE alter_items (id INTEGER)")
        .unwrap();
    reader
        .execute("INSERT INTO alter_items VALUES (1)")
        .unwrap();
    reader.execute("BEGIN").unwrap();
    reader.query("SELECT * FROM alter_items", &[]).unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle =
        thread::spawn(move || {
            result_sender
                .send(changer.execute(
                    "ALTER TABLE alter_items ADD COLUMN marker INTEGER DEFAULT 7 NOT NULL",
                ))
                .unwrap();
        });
    wait_until_relation_blocked(&db);
    reader.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(0))
    );
    handle.join().unwrap();
    assert_eq!(
        reader
            .query("SELECT id, marker FROM alter_items", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Int4(7)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn drops_foreign_key_related_tables_as_one_set_in_either_order() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    session
        .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
        .unwrap();

    session.execute("BEGIN").unwrap();
    session.execute("DROP TABLE parents, children").unwrap();
    session.execute("ROLLBACK").unwrap();
    assert!(session.query("SELECT * FROM parents", &[]).is_ok());
    assert!(session.query("SELECT * FROM children", &[]).is_ok());

    session.execute("DROP TABLE children, parents").unwrap();
    assert_eq!(
        session
            .query("SELECT * FROM parents", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        session
            .query("SELECT * FROM children", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn aborts_open_transactions_when_sessions_are_dropped() {
    let db = Db::create();
    let mut abandoned = db.create_session();
    abandoned.execute("BEGIN").unwrap();
    abandoned
        .execute("CREATE TABLE abandoned (id INTEGER)")
        .unwrap();
    drop(abandoned);

    let mut successor = db.create_session();
    successor
        .execute("CREATE TABLE abandoned (id INTEGER)")
        .unwrap();
    assert!(successor.query("SELECT * FROM abandoned", &[]).is_ok());
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn restores_prepared_relation_identity_after_ddl_rollback() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (1)").unwrap();
    let select = session.prepare("SELECT id FROM items").unwrap();
    let drop = session.prepare("DROP TABLE items").unwrap();

    session.execute("BEGIN").unwrap();
    session.execute("DROP TABLE items").unwrap();
    session.execute("CREATE TABLE items (id INTEGER)").unwrap();
    session.execute("INSERT INTO items VALUES (2)").unwrap();
    assert_eq!(
        session.execute_prepared(&drop, &[]).unwrap_err().sqlstate,
        SqlState::FeatureNotSupported
    );
    session.execute("ROLLBACK").unwrap();

    assert_eq!(
        session.query_prepared(&select, &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn insert_uses_exact_literal_types_and_commits() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, name TEXT)")
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO items (name, id) VALUES ('one', 1), ('two', 2)")
            .unwrap(),
        create_affected_results(2)
    );
    let mut state = db.state.lock().unwrap();
    let reader = state.transactions.begin();
    let snapshot = Snapshot::create(&state.transactions);
    let schema = state.catalog.require_table("items").unwrap();
    let table = state.tables.get(&schema.id).unwrap();
    let rows = table
        .iterate_version_chains()
        .map(|(_, chain)| {
            find_visible_version(chain, &snapshot, reader, &state.transactions)
                .unwrap()
                .row
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            vec![Value::Int4(1), Value::Text("one".into())],
            vec![Value::Int4(2), Value::Text("two".into())]
        ]
    );
    let _ = table;
    drop(state);
    let error = session
        .execute("INSERT INTO items VALUES ('wrong', 'type')")
        .unwrap_err();
    assert_eq!(error.sqlstate, SqlState::InvalidTextRepresentation);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn executes_constant_select_values_and_default_rows() {
    let db = Db::create();
    let mut session = db.create_session();

    assert_eq!(
        session
            .query("SELECT 2 + 1 AS result ORDER BY result LIMIT 1", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(3)]]
    );
    let values = session
        .query(
            "VALUES (2), (1), (3) ORDER BY column1 LIMIT 1 OFFSET 1",
            &[],
        )
        .unwrap();
    assert_eq!(values.columns[0].name, "column1");
    assert_eq!(values.rows, vec![vec![Value::Int4(2)]]);
    session
        .execute("CREATE TABLE defaults (id INTEGER DEFAULT 7)")
        .unwrap();
    session
        .execute("INSERT INTO defaults DEFAULT VALUES")
        .unwrap();
    assert_eq!(
        session.query("SELECT id FROM defaults", &[]).unwrap().rows,
        vec![vec![Value::Int4(7)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn binds_single_table_aliases_and_qualified_columns() {
    let db = Db::create();
    let mut session = db.create_session();

    session
        .execute("CREATE TABLE items (id INTEGER, value TEXT)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (1, 'one')")
        .unwrap();
    let statement = session
            .prepare("SELECT item.value AS label, item.* FROM items AS item WHERE item.id = $1 ORDER BY label")
            .unwrap();
    assert_eq!(statement.get_parameter_types(), &[BaseType::Int4]);
    let result = session
        .query_prepared(&statement, &[Value::Int4(1)])
        .unwrap();
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| &column.name)
            .collect::<Vec<_>>(),
        vec!["label", "id", "value"]
    );
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Text("one".into()),
            Value::Int4(1),
            Value::Text("one".into())
        ]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn applies_quoted_aliases_and_reports_scope_errors() {
    let db = Db::create();
    let mut session = db.create_session();

    session
        .execute("CREATE TABLE \"Items\" (\"Value\" VARCHAR(5), other INTEGER)")
        .unwrap();
    session
        .execute("INSERT INTO \"Items\" VALUES ('two', 2)")
        .unwrap();
    let result = session
            .query(
                "SELECT \"I\".\"V\" AS \"Result\", \"I\".* FROM \"Items\" AS \"I\"(\"V\", \"Other\") WHERE \"I\".\"V\" = 'two' ORDER BY \"Result\"",
                &[],
            )
            .unwrap();
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| &column.name)
            .collect::<Vec<_>>(),
        vec!["Result", "V", "Other"]
    );
    assert_eq!(result.columns[0].typmod, result.columns[1].typmod);
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Text("two".into()),
            Value::Text("two".into()),
            Value::Int4(2)
        ]]
    );
    assert_eq!(
        session
            .query("SELECT missing FROM \"Items\"", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedColumn
    );
    assert_eq!(
        session
            .query("SELECT label FROM \"Items\" AS item(label, label)", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::AmbiguousColumn
    );
    assert_eq!(
        session
            .query("SELECT \"Items\".\"Value\" FROM \"Items\" AS item", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn joins_sources_and_merges_using_columns() {
    let db = Db::create();
    let mut session = db.create_session();

    session
        .execute(
            "CREATE TABLE left_rows (id INTEGER, left_value TEXT); \
                 CREATE TABLE right_rows (id INTEGER, right_value TEXT); \
                 INSERT INTO left_rows VALUES (1, 'one'), (2, 'two'); \
                 INSERT INTO right_rows VALUES (1, 'first'), (1, 'second'), (3, 'third')",
        )
        .unwrap();

    let cross = session
        .query(
            "SELECT left_rows.id, right_rows.id FROM left_rows, right_rows ORDER BY 1, 2",
            &[],
        )
        .unwrap();
    assert_eq!(cross.rows.len(), 6);

    let joined = session
            .query(
                "SELECT l.id, r.right_value FROM left_rows l INNER JOIN right_rows r ON l.id = r.id ORDER BY r.right_value",
                &[],
            )
            .unwrap();
    assert_eq!(
        joined.rows,
        vec![
            vec![Value::Int4(1), Value::Text("first".into())],
            vec![Value::Int4(1), Value::Text("second".into())],
        ]
    );

    let using = session
            .query(
                "SELECT * FROM (left_rows JOIN right_rows USING (id)) AS joined_rows ORDER BY id, right_value",
                &[],
            )
            .unwrap();
    assert_eq!(
        using
            .columns
            .iter()
            .map(|column| &column.name)
            .collect::<Vec<_>>(),
        vec!["id", "left_value", "right_value"]
    );
    assert_eq!(using.rows.len(), 2);
    let natural = session
        .query(
            "SELECT l.id, r.id FROM left_rows l NATURAL JOIN right_rows r ORDER BY l.id, r.id",
            &[],
        )
        .unwrap();
    assert_eq!(
        natural.rows,
        vec![
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(1), Value::Int4(1)],
        ]
    );
    assert_eq!(
        session
            .query("SELECT id FROM left_rows CROSS JOIN right_rows", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::AmbiguousColumn
    );

    let left = session
            .query(
                "SELECT l.id, r.id FROM left_rows l LEFT JOIN right_rows r ON l.id = r.id ORDER BY l.id, r.id",
                &[],
            )
            .unwrap();
    assert_eq!(
        left.rows,
        vec![
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(2), Value::Null],
        ]
    );
    let full = session
            .query(
                "SELECT id, left_value, right_value FROM left_rows FULL JOIN right_rows USING (id) ORDER BY id, right_value",
                &[],
            )
            .unwrap();
    assert_eq!(
        full.rows,
        vec![
            vec![
                Value::Int4(1),
                Value::Text("one".into()),
                Value::Text("first".into())
            ],
            vec![
                Value::Int4(1),
                Value::Text("one".into()),
                Value::Text("second".into())
            ],
            vec![Value::Int4(2), Value::Text("two".into()), Value::Null],
            vec![Value::Int4(3), Value::Null, Value::Text("third".into())],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_single_table_alias_scope_property() {
    for index in 0..32 {
        let db = Db::create();
        let mut session = db.create_session();
        let alias = format!("item_{index}");
        let output = format!("value_{index}");
        session
            .execute("CREATE TABLE items (id INTEGER, value TEXT)")
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO items VALUES ({index}, 'value_{index}')"
            ))
            .unwrap();
        let statement = session
                .prepare(&format!(
                    "SELECT {alias}.value AS {output}, {alias}.* FROM items AS {alias} WHERE {alias}.id = $1 ORDER BY {output}"
                ))
                .unwrap();
        assert_eq!(statement.get_parameter_types(), &[BaseType::Int4]);
        let result = session
            .query_prepared(&statement, &[Value::Int4(index)])
            .unwrap();
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| &column.name)
                .collect::<Vec<_>>(),
            vec![&output, "id", "value"]
        );
        assert_eq!(
            result.rows,
            vec![vec![
                Value::Text(format!("value_{index}")),
                Value::Int4(index),
                Value::Text(format!("value_{index}"))
            ]]
        );
    }
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materializes_derived_tables_and_uncorrelated_scalar_subqueries() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, value INTEGER)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (1, 10), (2, 20), (3, 30)")
        .unwrap();

    let derived = session
            .query(
                "SELECT source.item_id FROM (SELECT id AS item_id FROM items WHERE id > 1) AS source ORDER BY source.item_id",
                &[],
            )
            .unwrap();
    assert_eq!(
        derived.rows,
        vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
    );
    assert_eq!(
            session
                .query(
                    "SELECT nested.item_id FROM (SELECT source.item_id FROM (SELECT id AS item_id FROM items) AS source) AS nested ORDER BY nested.item_id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)], vec![Value::Int4(3)]]
        );

    let scalar = session
        .query(
            "SELECT id FROM items WHERE value < (SELECT 25) ORDER BY (SELECT 100) - id",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar.rows,
        vec![vec![Value::Int4(2)], vec![Value::Int4(1)]]
    );

    session
        .execute("UPDATE items SET value = (SELECT 99) WHERE id = (SELECT 1)")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT value FROM items WHERE id = 1", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(99)]]
    );
    assert_eq!(
        session
            .query("SELECT (SELECT value FROM items WHERE id > 1)", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::CardinalityViolation
    );
    session.execute("ROLLBACK").unwrap();
    let prepared = session.prepare("SELECT (SELECT 7)").unwrap();
    assert_eq!(
        prepared.get_result_columns()[0].type_oid,
        BaseType::Int4.map_to_oid()
    );
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows,
        vec![vec![Value::Int4(7)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materializes_uncorrelated_subquery_predicates() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER, pair INTEGER)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (1, 1), (2, 2), (NULL, 3)")
        .unwrap();

    let statement = session
        .prepare("SELECT id FROM items WHERE id = $1 AND id IN (SELECT id FROM items) ORDER BY id")
        .unwrap();
    assert_eq!(statement.get_parameter_types(), &[BaseType::Int4]);
    assert_eq!(
        session
            .query_prepared(&statement, &[Value::Int4(2)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
    assert_eq!(
            session
                .query(
                    "SELECT EXISTS (SELECT 1 FROM items WHERE id = 1), 3 NOT IN (SELECT id FROM items), 3 > ALL (SELECT id FROM items)",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Bool(true), Value::Null, Value::Null]]
        );
    assert_eq!(
        session
            .query("SELECT (1, 1) IN (SELECT id, pair FROM items)", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Bool(true)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn executes_correlated_subqueries_with_lexical_scopes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE parents (id INTEGER, threshold INTEGER)")
        .unwrap();
    session
        .execute("CREATE TABLE children (id INTEGER, parent_id INTEGER, value INTEGER)")
        .unwrap();
    session
        .execute("INSERT INTO parents VALUES (1, 15), (2, 5), (3, NULL)")
        .unwrap();
    session
        .execute("INSERT INTO children VALUES (10, 1, 10), (11, 1, 20), (12, 2, NULL)")
        .unwrap();

    let result = session
            .query(
                "SELECT p.id, EXISTS (SELECT 1 FROM children AS c WHERE c.parent_id = p.id AND c.value > p.threshold) AS has_match FROM parents AS p ORDER BY p.id",
                &[],
            )
            .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Bool(true)],
            vec![Value::Int4(2), Value::Bool(false)],
            vec![Value::Int4(3), Value::Bool(false)],
        ]
    );

    assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p WHERE p.id IN (SELECT c.parent_id FROM children AS c WHERE c.value > p.threshold) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p WHERE p.threshold < ANY (SELECT c.value FROM children AS c WHERE c.parent_id = p.id) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    let nested = session
            .prepare(
                "SELECT p.id FROM parents AS p WHERE EXISTS (SELECT 1 FROM children AS c WHERE c.parent_id = p.id AND EXISTS (SELECT 1 WHERE c.value > p.threshold)) ORDER BY p.id",
            )
            .unwrap();
    assert_eq!(
        session.query_prepared(&nested, &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p JOIN children AS c ON c.parent_id = p.id AND EXISTS (SELECT 1 WHERE c.value > p.threshold) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p WHERE EXISTS (SELECT 1 FROM children AS p WHERE p.parent_id = p.id) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            Vec::<Vec<Value>>::new()
        );
    assert_eq!(
            session
                .query(
                    "SELECT (SELECT c.value FROM children AS c WHERE c.parent_id = p.id) FROM parents AS p WHERE p.id = 1",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::CardinalityViolation
        );

    session
        .execute("CREATE TABLE empty_parents (id INTEGER)")
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT p.id FROM empty_parents AS p WHERE EXISTS (SELECT 1 WHERE missing = 1)",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedColumn
    );
    assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p CROSS JOIN parents AS other WHERE EXISTS (SELECT 1 WHERE id = 1)",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::AmbiguousColumn
        );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn tolerates_only_planner_settings_outside_strict_mode() {
    let db = Db::create();
    let mut session = db.create_session();

    assert_eq!(
        session.execute("ANALYZE").unwrap(),
        create_affected_results(0)
    );
    assert_eq!(
        session.execute("SET enable_hashjoin = off").unwrap(),
        create_affected_results(0)
    );
    assert_eq!(
        session.execute("RESET enable_hashjoin").unwrap(),
        create_affected_results(0)
    );
    let strict = Db::create_builder().set_strict_mode_enabled(true).build();
    assert_eq!(
        strict
            .create_session()
            .execute("ANALYZE")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materializes_non_recursive_ctes_once_with_aliases_and_empty_results() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (1), (2)")
        .unwrap();

    let result = session
            .query(
                "WITH source(value) AS (SELECT id FROM items), doubled AS (SELECT value * 2 AS value FROM source) SELECT source.value, doubled.value FROM source JOIN doubled ON doubled.value = source.value * 2 ORDER BY source.value",
                &[],
            )
            .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(4)],
        ]
    );

    session.execute("CREATE SEQUENCE samples").unwrap();
    let result = session
            .query(
                "WITH sampled(value) AS (SELECT nextval('samples')) SELECT left_sample.value = right_sample.value FROM sampled AS left_sample CROSS JOIN sampled AS right_sample",
                &[],
            )
            .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(true)]]);

    let result = session
        .query(
            "WITH values_cte(value) AS (SELECT 1) SELECT (WITH values_cte(value) AS (SELECT 2) SELECT value FROM values_cte) FROM values_cte",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int4(2)]]);
    assert_eq!(result.columns[0].name, "value");
    assert_eq!(
            session
                .query(
                    "WITH later_value(value) AS (SELECT value FROM first_value), first_value(value) AS (SELECT 1) SELECT value FROM later_value",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );

    let result = session
            .query(
                "WITH empty_values(value) AS (SELECT id FROM items WHERE false) SELECT value FROM empty_values",
                &[],
            )
            .unwrap();
    assert!(result.rows.is_empty());

    let statement = session
        .prepare("WITH parameterized(value) AS (SELECT $1) SELECT value FROM parameterized")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&statement, &[Value::Text("seven".into())])
            .unwrap()
            .rows,
        vec![vec![Value::Text("seven".into())]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn retains_parameters_from_unreferenced_ctes() {
    let db = Db::create();
    let mut session = db.create_session();
    let statement = session
        .prepare("WITH unused AS (SELECT $1) SELECT 1")
        .unwrap();

    assert_eq!(
        session
            .query_prepared(&statement, &[Value::Text("unused".into())])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn does_not_evaluate_unread_cte_rows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE SEQUENCE unused_cte_sequence")
        .unwrap();
    session
        .execute("CREATE SEQUENCE limited_cte_sequence")
        .unwrap();

    assert_eq!(
        session
            .query(
                "WITH unused AS (SELECT nextval('unused_cte_sequence')) SELECT 1",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        session
            .query("SELECT nextval('unused_cte_sequence')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );

    assert!(
            session
                .query(
                    "WITH limited(value) AS (SELECT nextval('limited_cte_sequence')) SELECT value FROM limited LIMIT 0",
                    &[],
                )
                .unwrap()
                .rows
                .is_empty()
        );
    assert_eq!(
        session
            .query("SELECT nextval('limited_cte_sequence')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn acquires_row_locks_requested_by_ctes() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();
    first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
    first.execute("BEGIN").unwrap();
    first
        .query(
            "WITH locked AS (SELECT * FROM items WHERE id = 1 FOR UPDATE) SELECT * FROM locked",
            &[],
        )
        .unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
            .send(second.execute("UPDATE items SET value = 2 WHERE id = 1"))
            .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        Ok(create_affected_results(1))
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn locks_only_referenced_foreign_keys_for_triggered_insert_ctes() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_millis(40))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE TABLE cte_lock_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE cte_lock_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES cte_lock_parents,
                    value INTEGER
                );
                CREATE TABLE cte_lock_helper (id INTEGER PRIMARY KEY);
                CREATE FUNCTION preserve_cte_lock_child() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_cte_lock_child
                    BEFORE INSERT OR UPDATE ON cte_lock_children
                    FOR EACH ROW EXECUTE FUNCTION preserve_cte_lock_child();
                INSERT INTO cte_lock_parents VALUES (1), (2);
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
        .execute("DELETE FROM cte_lock_parents WHERE id = 2")
        .unwrap();

    assert_eq!(
            writer
                .query(
                    "WITH inserted AS (INSERT INTO cte_lock_children SELECT 2, 1, 0 RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
    assert_eq!(
            writer
                .query(
                    "WITH source AS (SELECT 3 AS id, 1 AS parent_id, 0 AS value), inserted AS (INSERT INTO cte_lock_children SELECT id, parent_id, value FROM source RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3)]]
        );
    assert_eq!(
            writer
                .execute(
                    "WITH inserted AS (INSERT INTO cte_lock_children VALUES ((SELECT 4), 1, 0) RETURNING id) SELECT id FROM inserted",
                )
                .unwrap()
                .into_iter()
                .next(),
            Some(StatementResult::Query(QueryResult {
                columns: vec![ColumnMeta {
                    name: "id".into(),
                    type_oid: BaseType::Int4.map_to_oid(),
                    typmod: -1,
                }],
                rows: vec![vec![Value::Int4(4)]],
            }))
        );
    assert_eq!(
            writer
                .query(
                    "WITH source AS (SELECT 3 AS id), updated AS (UPDATE cte_lock_children SET value = 1 FROM source WHERE cte_lock_children.id = source.id RETURNING cte_lock_children.id) SELECT id FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3)]]
        );
    assert_eq!(
            writer
                .query(
                    "WITH source AS (INSERT INTO cte_lock_helper VALUES (1) RETURNING id), inserted AS (INSERT INTO cte_lock_children SELECT 5, source.id, 0 FROM source RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(5)]]
        );
    assert_eq!(
            writer
                .query(
                    "WITH source AS (INSERT INTO cte_lock_helper VALUES (3) RETURNING id), updated AS (UPDATE cte_lock_children SET value = 2 FROM source WHERE cte_lock_children.id = source.id RETURNING cte_lock_children.id) SELECT id FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3)]]
        );
    writer
            .execute(
                "WITH source AS (INSERT INTO cte_lock_helper VALUES ((SELECT 6)) RETURNING id), inserted AS (INSERT INTO cte_lock_children SELECT source.id, 1, 0 FROM source RETURNING id) SELECT id FROM inserted",
            )
            .unwrap();
    assert_eq!(
        writer
            .query("SELECT id FROM cte_lock_children WHERE id = 6", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(6)]]
    );
    for (query, expected) in [
        (
            "WITH source AS (INSERT INTO cte_lock_helper VALUES (7) RETURNING id - 6 AS parent_id), inserted AS (INSERT INTO cte_lock_children SELECT 7, source.parent_id, 0 FROM source RETURNING id) SELECT id FROM inserted",
            7,
        ),
        (
            "WITH source AS (INSERT INTO cte_lock_helper VALUES (10) RETURNING abs(id - 9) AS parent_id), inserted AS (INSERT INTO cte_lock_children SELECT 10, source.parent_id, 0 FROM source RETURNING id) SELECT id FROM inserted",
            10,
        ),
        (
            "WITH source AS (UPDATE cte_lock_helper SET id = id WHERE id = 6 RETURNING id - 5 AS parent_id), inserted AS (INSERT INTO cte_lock_children SELECT 8, source.parent_id, 0 FROM source RETURNING id) SELECT id FROM inserted",
            8,
        ),
        (
            "WITH source AS (DELETE FROM cte_lock_helper WHERE id = 7 RETURNING id - 6 AS parent_id), inserted AS (INSERT INTO cte_lock_children SELECT 9, source.parent_id, 0 FROM source RETURNING id) SELECT id FROM inserted",
            9,
        ),
    ] {
        assert_eq!(
            writer
                .query(query, &[])
                .unwrap_or_else(|error| panic!("{query}: {error:?}"))
                .rows,
            vec![vec![Value::Int4(expected)]],
            "{query}"
        );
    }
    holder.execute("ROLLBACK").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn acquires_forward_and_unprepared_cte_mutation_locks() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE cte_source_ids;
                CREATE TABLE cte_source (id INTEGER PRIMARY KEY, value INTEGER);
                CREATE TABLE cte_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE cte_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES cte_parents,
                    value INTEGER
                );
                CREATE FUNCTION preserve_cte_child() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_cte_child
                    BEFORE INSERT OR UPDATE ON cte_children
                    FOR EACH ROW EXECUTE FUNCTION preserve_cte_child();
                INSERT INTO cte_parents VALUES (1);
                INSERT INTO cte_source VALUES (1, 0), (4, 0);
                INSERT INTO cte_children VALUES (1, 1, 0), (3, 1, 0), (4, 1, 0);
                "#,
        )
        .unwrap();
    session
        .execute("BEGIN; SET LOCAL statement_timeout = '1s'")
        .unwrap();

    assert_eq!(
            session
                .query(
                    "WITH RECURSIVE inserted AS (INSERT INTO cte_children SELECT source.id + 1, 1, 0 FROM source RETURNING id), source AS (UPDATE cte_source SET value = value + 1 WHERE id = 1 RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH source AS (INSERT INTO cte_source VALUES (2, 0) RETURNING nextval('cte_source_ids')::INTEGER AS id), updated AS (UPDATE cte_children SET value = value + 1 FROM source WHERE cte_children.id = source.id RETURNING cte_children.id) SELECT id FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH source AS (INSERT INTO cte_source VALUES (3, 0) ON CONFLICT DO NOTHING RETURNING id), updated AS (UPDATE cte_children SET value = value + 1 FROM source WHERE cte_children.id = source.id RETURNING cte_children.id) SELECT id FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH source AS (INSERT INTO cte_source VALUES (6, 0) RETURNING nextval('cte_source_ids')::INTEGER AS id), first_update AS (UPDATE cte_children SET value = 5 FROM source WHERE cte_children.id = source.id RETURNING cte_children.id), second_update AS (UPDATE cte_children SET value = 10 WHERE id = 4 RETURNING id) SELECT first_update.id, second_update.id FROM first_update CROSS JOIN second_update",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2), Value::Int4(4)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH RECURSIVE inserted AS (INSERT INTO cte_children SELECT source.id + 1, 1, 0 FROM source RETURNING id), source AS (DELETE FROM cte_source WHERE id = 4 RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(5)]]
        );
    session.execute("COMMIT").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn keeps_prepared_trigger_rows_bound_to_their_cte_occurrence() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE update_source_ids;
                CREATE SEQUENCE insert_source_ids;
                CREATE TABLE cache_parents (id INTEGER PRIMARY KEY);
                CREATE TABLE cache_source (id INTEGER PRIMARY KEY);
                CREATE TABLE cache_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES cache_parents,
                    value INTEGER
                );
                CREATE FUNCTION preserve_cache_child() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_cache_child
                    BEFORE INSERT OR UPDATE ON cache_children
                    FOR EACH ROW EXECUTE FUNCTION preserve_cache_child();
                INSERT INTO cache_parents VALUES (1);
                INSERT INTO cache_children VALUES (1, 1, 0), (2, 1, 0);
                "#,
        )
        .unwrap();

    assert_eq!(
            session
                .query(
                    "WITH source AS (INSERT INTO cache_source VALUES (1) RETURNING nextval('update_source_ids')::INTEGER AS id), first_update AS (UPDATE cache_children SET value = 5 FROM source WHERE cache_children.id = source.id RETURNING cache_children.id), second_update AS (UPDATE cache_children SET value = 10 WHERE id = 2 RETURNING id) SELECT first_update.id, second_update.id FROM first_update CROSS JOIN second_update",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(2)]]
        );
    session
        .execute("DELETE FROM cache_children; DELETE FROM cache_source")
        .unwrap();
    assert_eq!(
            session
                .query(
                    "WITH source AS (INSERT INTO cache_source VALUES (2) RETURNING nextval('insert_source_ids')::INTEGER AS id), first_insert AS (INSERT INTO cache_children SELECT source.id, 1, 5 FROM source RETURNING id), second_insert AS (INSERT INTO cache_children VALUES (2, 1, 10) RETURNING id) SELECT first_insert.id, second_insert.id FROM first_insert CROSS JOIN second_insert",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(2)]]
        );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn locks_only_rows_resolved_by_staged_cte_mutations() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_millis(40))
        .build();
    let mut holder = db.create_session();
    let mut writer = db.create_session();
    holder
        .execute(
            r#"
                CREATE SEQUENCE staged_parent_ids;
                CREATE TABLE staged_parents (id INTEGER PRIMARY KEY, value INTEGER);
                CREATE TABLE staged_source (id INTEGER PRIMARY KEY);
                CREATE TABLE staged_children (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER REFERENCES staged_parents,
                    value INTEGER
                );
                CREATE FUNCTION preserve_staged_child() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_staged_child
                    BEFORE INSERT OR UPDATE ON staged_children
                    FOR EACH ROW EXECUTE FUNCTION preserve_staged_child();
                INSERT INTO staged_parents VALUES (1, 0), (2, 0);
                INSERT INTO staged_children VALUES (1, 1, 0), (2, 1, 0);
                "#,
        )
        .unwrap();
    holder.execute("BEGIN").unwrap();
    holder
            .execute(
                "UPDATE staged_parents SET value = 1 WHERE id = 2; UPDATE staged_children SET value = 1 WHERE id = 2",
            )
            .unwrap();

    assert_eq!(
            writer
                .query(
                    "WITH source AS (INSERT INTO staged_source VALUES (10) RETURNING nextval('staged_parent_ids')::INTEGER AS parent_id), inserted AS (INSERT INTO staged_children SELECT 3, source.parent_id, 0 FROM source RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3)]]
        );
    assert_eq!(
            writer
                .query(
                    "WITH source AS (INSERT INTO staged_source VALUES (1) ON CONFLICT DO NOTHING RETURNING id), updated AS (UPDATE staged_children SET parent_id = 1 FROM source WHERE staged_children.id = source.id RETURNING staged_children.id) SELECT id FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    assert_eq!(
            writer
                .query(
                    "WITH source AS (INSERT INTO staged_source VALUES (11) RETURNING id - 10 AS id, nextval('staged_parent_ids') AS consumed), deleted AS (DELETE FROM staged_children USING source WHERE staged_children.id = source.id RETURNING staged_children.id) SELECT id FROM deleted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    holder.execute("ROLLBACK").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolves_a_cte_self_name_to_an_existing_table() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();
    session
        .execute("INSERT INTO items VALUES (1), (2)")
        .unwrap();

    assert_eq!(
        session
            .query(
                "WITH items AS (SELECT * FROM items) SELECT * FROM items ORDER BY id",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_quoted_cte_output_column_names() {
    let db = Db::create();
    let mut session = db.create_session();

    let result = session
        .query(
            "WITH c(\"Value\") AS (SELECT 1) SELECT \"Value\" FROM c",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns[0].name, "Value");
    assert_eq!(result.rows, vec![vec![Value::Int4(1)]]);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn executes_with_prefixed_writes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();

    assert_eq!(
        session
            .execute("WITH source AS (SELECT 1, 10) INSERT INTO items SELECT * FROM source")
            .unwrap(),
        create_affected_results(1)
    );
    assert_eq!(
            session
                .query(
                    "WITH source(value) AS (SELECT 20) UPDATE items SET value = source.value FROM source WHERE id = 1 RETURNING items.value",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(20)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH source(id) AS (SELECT 1) DELETE FROM items USING source WHERE items.id = source.id RETURNING items.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn executes_data_modifying_ctes_once_with_statement_snapshot_visibility() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();
    session.execute("INSERT INTO items VALUES (1, 10)").unwrap();

    let result = session
            .query(
                "WITH inserted AS (INSERT INTO items VALUES (2, 20) RETURNING id, value) SELECT inserted.id, inserted.value, (SELECT count(*) FROM items) FROM inserted",
                &[],
            )
            .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Int4(2), Value::Int4(20), Value::Int8(1)]]
    );
    assert_eq!(result.columns[2].name, "count");
    assert_eq!(
        session
            .query("SELECT id, value FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(1), Value::Int4(10)],
            vec![Value::Int4(2), Value::Int4(20)],
        ]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_data_modifying_cte_snapshot_visibility_after_lock_wait() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_secs(2))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();
    first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
    first.execute("BEGIN").unwrap();
    first
        .execute("UPDATE items SET value = 2 WHERE id = 1")
        .unwrap();

    let (result_sender, result_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        result_sender
                .send(second.query(
                    "WITH updated AS (UPDATE items SET value = value + 1 WHERE id = 1 RETURNING value) SELECT updated.value, items.value FROM updated CROSS JOIN items",
                    &[],
                ))
                .unwrap();
    });
    wait_until_blocked(&db);
    first.execute("COMMIT").unwrap();

    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .rows,
        vec![vec![Value::Int4(3), Value::Int4(2)]]
    );
    handle.join().unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn executes_cte_mutation_dependencies_before_later_returning_expressions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE dependency_order_ids;
                CREATE TABLE dependency_order_source (
                    id INTEGER PRIMARY KEY,
                    value INTEGER CHECK (value > 0)
                );
                CREATE TABLE dependency_order_middle (id INTEGER);
                CREATE TABLE dependency_order_target (id BIGINT, value INTEGER);
                INSERT INTO dependency_order_source VALUES (1, 1);
                "#,
        )
        .unwrap();

    assert_eq!(
            session
                .execute(
                    "WITH bad AS (UPDATE dependency_order_source SET value = 0 WHERE id = 1 RETURNING id), source AS (INSERT INTO dependency_order_middle SELECT id FROM bad RETURNING 1 / 0 AS id), inserted AS (INSERT INTO dependency_order_target SELECT id, 0 FROM source RETURNING id) SELECT * FROM inserted",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::CheckViolation
        );
    assert_eq!(
            session
                .execute(
                    "WITH bad AS (UPDATE dependency_order_source SET value = 0 WHERE id = 1 RETURNING id), source AS (INSERT INTO dependency_order_middle SELECT id FROM bad RETURNING nextval('dependency_order_ids') AS id), inserted AS (INSERT INTO dependency_order_target SELECT id, 0 FROM source RETURNING id) SELECT * FROM inserted",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::CheckViolation
        );
    assert_eq!(
        session
            .query("SELECT nextval('dependency_order_ids')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn skips_cte_targets_already_mutated_by_the_same_command() {
    let db = Db::create();
    let mut session = db.create_session();
    session
            .execute("CREATE TABLE repeated_cte_rows (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO repeated_cte_rows VALUES (1, 1)")
            .unwrap();

    assert_eq!(
            session
                .query(
                    "WITH first_update AS (UPDATE repeated_cte_rows SET value = 2 WHERE id = 1 RETURNING value), second_update AS (UPDATE repeated_cte_rows SET value = 3 WHERE id = 1 RETURNING value) SELECT first_update.value, (SELECT count(*) FROM second_update) FROM first_update",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2), Value::Int8(0)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH first_delete AS (DELETE FROM repeated_cte_rows WHERE id = 1 RETURNING id), second_delete AS (DELETE FROM repeated_cte_rows WHERE id = 1 RETURNING id) SELECT first_delete.id, (SELECT count(*) FROM second_delete) FROM first_delete",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int8(0)]]
        );

    session
            .execute(
                "CREATE TABLE transaction_cte_rows (id BIGINT PRIMARY KEY, value BIGINT); BEGIN; INSERT INTO transaction_cte_rows VALUES (1, 1); UPDATE transaction_cte_rows SET value = 2 WHERE id = 1",
            )
            .unwrap();
    assert_eq!(
            session
                .query(
                    "WITH first_update AS (UPDATE transaction_cte_rows SET value = 3 WHERE id = 1 RETURNING value), second_update AS (UPDATE transaction_cte_rows SET value = 4 WHERE id = 1 RETURNING value) SELECT first_update.value, (SELECT count(*) FROM second_update) FROM first_update",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(3), Value::Int8(0)]]
        );
    session.execute("COMMIT").unwrap();
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluates_volatile_cte_mutation_targets_once() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE update_target_ids;
                CREATE SEQUENCE delete_target_ids;
                CREATE SEQUENCE dependent_target_ids;
                CREATE TABLE volatile_update_rows (id BIGINT PRIMARY KEY, value BIGINT);
                CREATE TABLE volatile_delete_rows (id BIGINT PRIMARY KEY, value BIGINT);
                CREATE TABLE volatile_dependent_rows (id BIGINT PRIMARY KEY, value BIGINT);
                CREATE TABLE volatile_copies (id BIGINT, value BIGINT);
                INSERT INTO volatile_update_rows VALUES (1, 0);
                INSERT INTO volatile_delete_rows VALUES (1, 0);
                INSERT INTO volatile_dependent_rows VALUES (1, 0);
                "#,
        )
        .unwrap();

    assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE volatile_update_rows SET value = 7 WHERE id = nextval('update_target_ids') RETURNING id, value) SELECT * FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1), Value::Int8(7)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('update_target_ids')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    assert_eq!(
        session
            .query(
                "UPDATE volatile_update_rows SET value = 8 WHERE id = 1 RETURNING value",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int8(8)]]
    );

    assert_eq!(
            session
                .query(
                    "WITH deleted AS (DELETE FROM volatile_delete_rows WHERE id = nextval('delete_target_ids') RETURNING id) SELECT * FROM deleted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('delete_target_ids')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );

    assert_eq!(
            session
                .query(
                    "WITH source AS (UPDATE volatile_dependent_rows SET value = 9 WHERE id = nextval('dependent_target_ids') RETURNING id, value), inserted AS (INSERT INTO volatile_copies SELECT * FROM source RETURNING id, value) SELECT * FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1), Value::Int8(9)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('dependent_target_ids')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn retains_volatile_targets_through_trigger_and_foreign_key_lock_passes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE direct_update_targets;
                CREATE SEQUENCE cte_update_targets;
                CREATE TABLE volatile_target_parents (id BIGINT PRIMARY KEY);
                CREATE TABLE volatile_trigger_rows (
                    id BIGINT PRIMARY KEY,
                    parent_id BIGINT REFERENCES volatile_target_parents,
                    value BIGINT
                );
                CREATE FUNCTION preserve_volatile_target() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_volatile_target
                    BEFORE UPDATE ON volatile_trigger_rows
                    FOR EACH ROW EXECUTE FUNCTION preserve_volatile_target();
                INSERT INTO volatile_target_parents VALUES (1);
                INSERT INTO volatile_trigger_rows VALUES (1, 1, 0);
                "#,
        )
        .unwrap();

    assert_eq!(
            session
                .query(
                    "UPDATE volatile_trigger_rows SET value = 7 WHERE id = nextval('direct_update_targets') RETURNING id, value",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1), Value::Int8(7)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('direct_update_targets')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE volatile_trigger_rows SET value = 8 WHERE id = nextval('cte_update_targets') RETURNING id, value) SELECT * FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1), Value::Int8(8)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('cte_update_targets')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluates_prepared_trigger_checks_once() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE prepared_insert_check_values;
                CREATE SEQUENCE prepared_update_check_values;
                CREATE TABLE prepared_check_parents (id BIGINT PRIMARY KEY);
                CREATE TABLE prepared_insert_checks (
                    id BIGINT CHECK (nextval('prepared_insert_check_values') = 1)
                );
                CREATE TABLE prepared_update_checks (
                    id BIGINT PRIMARY KEY,
                    parent_id BIGINT REFERENCES prepared_check_parents,
                    value BIGINT CHECK (nextval('prepared_update_check_values') = 1)
                );
                CREATE FUNCTION preserve_prepared_check() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_prepared_insert_check
                    BEFORE INSERT ON prepared_insert_checks
                    FOR EACH ROW EXECUTE FUNCTION preserve_prepared_check();
                CREATE TRIGGER preserve_prepared_update_check
                    BEFORE UPDATE ON prepared_update_checks
                    FOR EACH ROW EXECUTE FUNCTION preserve_prepared_check();
                INSERT INTO prepared_check_parents VALUES (1);
                INSERT INTO prepared_update_checks VALUES (1, 1, 0);
                SELECT setval('prepared_update_check_values', 1, false);
                "#,
        )
        .unwrap();

    session
        .execute("INSERT INTO prepared_insert_checks VALUES (1)")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT nextval('prepared_insert_check_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE prepared_update_checks SET value = 1 RETURNING value) SELECT * FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('prepared_update_check_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluates_plain_insert_checks_once() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE plain_insert_check_values;
                CREATE TABLE plain_insert_checks (
                    id BIGINT CHECK (nextval('plain_insert_check_values') = 1)
                );
                "#,
        )
        .unwrap();

    session
        .execute("INSERT INTO plain_insert_checks VALUES (1)")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT nextval('plain_insert_check_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn interleaves_before_insert_triggers_with_returning() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE returning_trigger_values;
                CREATE TABLE returning_trigger_rows (id BIGINT, value BIGINT);
                CREATE FUNCTION allocate_returning_trigger_value() RETURNS TRIGGER AS $$
                BEGIN
                    NEW.value := nextval('returning_trigger_values');
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_returning_trigger_value
                    BEFORE INSERT ON returning_trigger_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_returning_trigger_value();
                "#,
        )
        .unwrap();

    assert_eq!(
            session
                .query(
                    "INSERT INTO returning_trigger_rows VALUES (1, 0), (2, 0) RETURNING id, value, nextval('returning_trigger_values')",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Int8(1), Value::Int8(1), Value::Int8(2)],
                vec![Value::Int8(2), Value::Int8(3), Value::Int8(4)],
            ]
        );
    assert_eq!(
        session
            .query(
                "SELECT id, value FROM returning_trigger_rows ORDER BY id",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1), Value::Int8(1)],
            vec![Value::Int8(2), Value::Int8(3)],
        ]
    );

    session
        .execute(
            r#"
                CREATE SEQUENCE failing_returning_trigger_values;
                CREATE TABLE failing_returning_trigger_rows (id BIGINT, value BIGINT);
                CREATE FUNCTION allocate_failing_returning_trigger_value() RETURNS TRIGGER AS $$
                BEGIN
                    NEW.value := nextval('failing_returning_trigger_values');
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_failing_returning_trigger_value
                    BEFORE INSERT ON failing_returning_trigger_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_failing_returning_trigger_value();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .execute(
                    "INSERT INTO failing_returning_trigger_rows VALUES (1, 0), (2, 0) RETURNING 1 / (id - 1)",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::DivisionByZero
        );
    assert_eq!(
        session
            .query("SELECT nextval('failing_returning_trigger_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    assert!(
        session
            .query("SELECT * FROM failing_returning_trigger_rows", &[])
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn interleaves_before_insert_triggers_with_conflict_handling() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE conflict_nothing_values;
                CREATE TABLE conflict_nothing_rows (id BIGINT PRIMARY KEY, value BIGINT);
                INSERT INTO conflict_nothing_rows VALUES (2, 0);
                CREATE FUNCTION allocate_conflict_nothing_value() RETURNS TRIGGER AS $$
                BEGIN NEW.value := nextval('conflict_nothing_values'); RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_conflict_nothing_value
                    BEFORE INSERT ON conflict_nothing_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_conflict_nothing_value();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .query(
                    "INSERT INTO conflict_nothing_rows VALUES (1, 0), (2, 0), (3, 0) ON CONFLICT DO NOTHING RETURNING id, value, nextval('conflict_nothing_values')",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Int8(1), Value::Int8(1), Value::Int8(2)],
                vec![Value::Int8(3), Value::Int8(4), Value::Int8(5)],
            ]
        );

    session
        .execute(
            r#"
                CREATE SEQUENCE conflict_update_values;
                CREATE TABLE conflict_update_rows (id BIGINT PRIMARY KEY, value BIGINT);
                INSERT INTO conflict_update_rows VALUES (1, 0);
                CREATE FUNCTION allocate_conflict_update_value() RETURNS TRIGGER AS $$
                BEGIN NEW.value := nextval('conflict_update_values'); RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_conflict_update_value
                    BEFORE INSERT OR UPDATE ON conflict_update_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_conflict_update_value();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .query(
                    "WITH inserted AS (INSERT INTO conflict_update_rows VALUES (1, 0), (2, 0) ON CONFLICT (id) DO UPDATE SET value = excluded.value RETURNING id, value, nextval('conflict_update_values')) SELECT * FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Int8(1), Value::Int8(2), Value::Int8(3)],
                vec![Value::Int8(2), Value::Int8(4), Value::Int8(5)],
            ]
        );
    assert_eq!(
        session
            .query(
                "SELECT id, value FROM conflict_update_rows ORDER BY id",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1), Value::Int8(2)],
            vec![Value::Int8(2), Value::Int8(4)],
        ]
    );

    session
        .execute(
            r#"
                CREATE SEQUENCE failing_conflict_values;
                CREATE TABLE failing_conflict_rows (
                    id BIGINT PRIMARY KEY,
                    value BIGINT CHECK (value > 0)
                );
                INSERT INTO failing_conflict_rows VALUES (1, 1);
                CREATE FUNCTION allocate_failing_conflict_value() RETURNS TRIGGER AS $$
                BEGIN NEW.value := nextval('failing_conflict_values'); RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER allocate_failing_conflict_value
                    BEFORE INSERT ON failing_conflict_rows
                    FOR EACH ROW EXECUTE FUNCTION allocate_failing_conflict_value();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .execute(
                    "INSERT INTO failing_conflict_rows VALUES (1, 0), (2, 0) ON CONFLICT (id) DO UPDATE SET value = 0 RETURNING id, value",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::CheckViolation
        );
    assert_eq!(
        session
            .query("SELECT nextval('failing_conflict_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );

    session
        .execute(
            r#"
                CREATE TABLE cte_conflict_rows (id BIGINT PRIMARY KEY);
                CREATE FUNCTION preserve_cte_conflict_row() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER preserve_cte_conflict_row
                    BEFORE INSERT ON cte_conflict_rows
                    FOR EACH ROW EXECUTE FUNCTION preserve_cte_conflict_row();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .query(
                    "WITH first_insert AS (INSERT INTO cte_conflict_rows VALUES (1) RETURNING id), second_insert AS (INSERT INTO cte_conflict_rows VALUES (1) ON CONFLICT DO NOTHING RETURNING id) SELECT first_insert.id, (SELECT count(*) FROM second_insert) FROM first_insert",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1), Value::Int8(0)]]
        );

    session
        .execute(
            r#"
                CREATE SEQUENCE cte_conflict_returning_values;
                CREATE TABLE volatile_cte_conflict_rows (id BIGINT PRIMARY KEY);
                CREATE TRIGGER preserve_volatile_cte_conflict_row
                    BEFORE INSERT ON volatile_cte_conflict_rows
                    FOR EACH ROW EXECUTE FUNCTION preserve_cte_conflict_row();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .query(
                    "WITH first_insert AS (INSERT INTO volatile_cte_conflict_rows VALUES (1) RETURNING id), second_insert AS (INSERT INTO volatile_cte_conflict_rows VALUES (1) ON CONFLICT DO NOTHING RETURNING nextval('cte_conflict_returning_values')) SELECT first_insert.id, (SELECT count(*) FROM second_insert) FROM first_insert",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1), Value::Int8(0)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('cte_conflict_returning_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );

    session
        .execute(
            r#"
                CREATE SEQUENCE mixed_cte_conflict_returning_values;
                CREATE TABLE mixed_cte_conflict_source (id BIGINT PRIMARY KEY);
                CREATE TABLE mixed_cte_conflict_rows (id BIGINT PRIMARY KEY);
                INSERT INTO mixed_cte_conflict_source VALUES (1);
                CREATE TRIGGER preserve_mixed_cte_conflict_row
                    BEFORE INSERT ON mixed_cte_conflict_rows
                    FOR EACH ROW EXECUTE FUNCTION preserve_cte_conflict_row();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .query(
                    "WITH first_insert AS (INSERT INTO mixed_cte_conflict_rows SELECT id FROM mixed_cte_conflict_source RETURNING id), second_insert AS (INSERT INTO mixed_cte_conflict_rows VALUES (1) ON CONFLICT DO NOTHING RETURNING nextval('mixed_cte_conflict_returning_values')) SELECT first_insert.id, (SELECT count(*) FROM second_insert) FROM first_insert",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1), Value::Int8(0)]]
        );
    assert_eq!(
        session
            .query("SELECT nextval('mixed_cte_conflict_returning_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );

    session
        .execute(
            r#"
                CREATE SEQUENCE failing_cte_returning_values;
                CREATE TABLE failing_cte_returning_rows (id BIGINT PRIMARY KEY);
                CREATE TRIGGER preserve_failing_cte_returning_row
                    BEFORE INSERT ON failing_cte_returning_rows
                    FOR EACH ROW EXECUTE FUNCTION preserve_cte_conflict_row();
                "#,
        )
        .unwrap();
    assert_eq!(
            session
                .query(
                    "WITH first_insert AS (INSERT INTO failing_cte_returning_rows VALUES (1) RETURNING 1 / (id - 1) AS id), second_insert AS (INSERT INTO failing_cte_returning_rows VALUES (2) RETURNING nextval('failing_cte_returning_values') AS id) SELECT first_insert.id, second_insert.id FROM first_insert CROSS JOIN second_insert",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::DivisionByZero
        );
    assert_eq!(
        session
            .query("SELECT nextval('failing_cte_returning_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rejects_stale_repeatable_read_cte_mutation_targets() {
    for mutation in [
        "WITH changed AS (UPDATE stale_cte_rows SET value = value + 1 WHERE id = 1 RETURNING value) SELECT * FROM changed",
        "WITH changed AS (DELETE FROM stale_cte_rows WHERE id = 1 RETURNING value) SELECT * FROM changed",
    ] {
        let db = Db::create();
        let mut writer = db.create_session();
        let mut reader = db.create_session();
        writer
                .execute(
                    "CREATE TABLE stale_cte_rows (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO stale_cte_rows VALUES (1, 1)",
                )
                .unwrap();
        reader
            .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .unwrap();
        reader.query("SELECT * FROM stale_cte_rows", &[]).unwrap();
        writer
            .execute("UPDATE stale_cte_rows SET value = 2 WHERE id = 1")
            .unwrap();

        assert_eq!(
            reader.execute(mutation).unwrap_err().sqlstate,
            SqlState::SerializationFailure,
            "{mutation}"
        );
        reader.execute("ROLLBACK").unwrap();
    }
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn validates_prepared_cte_updates_before_evaluating_later_rows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
                CREATE SEQUENCE prepared_update_values;
                CREATE TABLE prepared_update_parents (id BIGINT PRIMARY KEY);
                CREATE TABLE prepared_update_rows (
                    id BIGINT PRIMARY KEY,
                    parent_id BIGINT REFERENCES prepared_update_parents,
                    value BIGINT
                );
                INSERT INTO prepared_update_parents VALUES (1);
                INSERT INTO prepared_update_rows VALUES (1, 1, 0), (2, 1, 0), (3, 1, 0);
                "#,
        )
        .unwrap();

    assert_eq!(
            session
                .execute(
                    "WITH updated AS (UPDATE prepared_update_rows SET id = 3, value = 1 / (2 - id) WHERE id < 3 RETURNING id) SELECT * FROM updated",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
    assert_eq!(
            session
                .execute(
                    "WITH updated AS (UPDATE prepared_update_rows SET id = 3, value = nextval('prepared_update_values') WHERE id < 3 RETURNING id) SELECT * FROM updated",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
    assert_eq!(
        session
            .query("SELECT nextval('prepared_update_values')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE prepared_update_rows SET id = CASE WHEN id = 1 THEN 4 ELSE 1 END WHERE id < 3 RETURNING id) SELECT * FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(4)], vec![Value::Int8(1)]]
        );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluates_cte_sources_required_by_mutations_with_zero_limit() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (value INTEGER)")
        .unwrap();

    assert!(
            session
                .query(
                    "WITH source(value) AS (SELECT 1), inserted AS (INSERT INTO items SELECT value FROM source RETURNING value) SELECT * FROM inserted LIMIT 0",
                    &[],
                )
                .unwrap()
                .rows
                .is_empty()
        );
    assert_eq!(
        session.query("SELECT value FROM items", &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluates_data_modifying_cte_defaults_once_during_lock_discovery() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE SEQUENCE item_ids").unwrap();
    session.execute("CREATE SEQUENCE parent_ids").unwrap();
    session
        .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
        .unwrap();
    session.execute("INSERT INTO parents VALUES (1)").unwrap();
    session
            .execute(
                "CREATE TABLE items (id BIGINT DEFAULT nextval('item_ids'), parent_id INTEGER REFERENCES parents(id))",
            )
            .unwrap();

    assert_eq!(
            session
                .query(
                    "WITH inserted AS (INSERT INTO items (parent_id) VALUES (1) RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH inserted AS (INSERT INTO items (parent_id) VALUES (nextval('parent_ids')) RETURNING parent_id) SELECT parent_id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn substitutes_typed_subqueries_in_data_modifying_ctes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();
    session.execute("INSERT INTO items VALUES (1, 1)").unwrap();

    assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE items SET value = (SELECT 2) RETURNING id, value) SELECT id, value FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(2)]]
        );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn permits_nonrecursive_mutations_under_with_recursive() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();

    assert_eq!(
            session
                .query(
                    "WITH RECURSIVE inserted AS (INSERT INTO items VALUES (1) RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH RECURSIVE inserted AS (INSERT INTO items SELECT value FROM series WHERE value = 2 RETURNING id), series(value) AS (VALUES (1) UNION ALL SELECT value + 1 FROM series WHERE value < 2) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn composes_insert_update_and_delete_ctes_through_returning_rows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();

    assert_eq!(
            session
                .query(
                    "WITH inserted AS (INSERT INTO items VALUES (1, 10) RETURNING id, value) SELECT left_row.id, right_row.value FROM inserted AS left_row CROSS JOIN inserted AS right_row",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(10)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE items SET value = value + 5 RETURNING id, value) SELECT id, value FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(15)]]
        );
    assert_eq!(
            session
                .query(
                    "WITH removed AS (DELETE FROM items RETURNING id, value), copied AS (INSERT INTO items SELECT id + 1, value FROM removed RETURNING id, value) SELECT id, value FROM copied",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2), Value::Int4(15)]]
        );

    let statement = session
            .prepare(
                "WITH inserted AS (INSERT INTO items VALUES ($1, $2) RETURNING id, value) SELECT id, value FROM inserted",
            )
            .unwrap();
    assert_eq!(
        statement.get_parameter_types(),
        &[crate::value::BaseType::Int4, crate::value::BaseType::Int4]
    );
    assert_eq!(
        session
            .query_prepared(&statement, &[Value::Int4(3), Value::Int4(30)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(3), Value::Int4(30)]]
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn executes_unreferenced_mutations_and_rolls_back_a_failing_cte_statement() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();

    assert_eq!(
        session
            .query(
                "WITH unreferenced AS (INSERT INTO items VALUES (1)) SELECT 42",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int4(42)]]
    );
    assert_eq!(
        session
            .query(
                "WITH missing_rows AS (INSERT INTO items VALUES (2)) SELECT * FROM missing_rows",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
            session
                .query(
                    "WITH first_insert AS (INSERT INTO items VALUES (3) RETURNING id), failing_insert AS (INSERT INTO items VALUES (1) RETURNING id) SELECT * FROM first_insert",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
    assert_eq!(
        session
            .query("SELECT id FROM items ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
fn rechecks_limited_row_locks_after_controlled_waits() {
    for repeatable in [false, true] {
        for changes_row in [false, true] {
            for commits in [false, true] {
                let db = Db::create_builder()
                    .set_lock_timeout(Duration::from_secs(3))
                    .build();
                let mut holder = db.create_session();
                let mut worker = db.create_session();
                holder.execute("CREATE TABLE lock_waits(id INT PRIMARY KEY, amount INT); INSERT INTO lock_waits VALUES(1,1),(2,2)").unwrap();
                worker
                    .execute(if repeatable {
                        "BEGIN ISOLATION LEVEL REPEATABLE READ"
                    } else {
                        "BEGIN"
                    })
                    .unwrap();
                worker.query("SELECT * FROM lock_waits", &[]).unwrap();
                holder.execute("BEGIN").unwrap();
                holder
                    .execute(if changes_row {
                        "UPDATE lock_waits SET amount=5 WHERE id=1"
                    } else {
                        "SELECT id FROM lock_waits WHERE id=1 FOR UPDATE"
                    })
                    .unwrap();
                let waiting = thread::spawn(move || {
                    worker.query("SELECT id FROM lock_waits WHERE amount<5 ORDER BY id LIMIT 1 FOR NO KEY UPDATE", &[])
                });
                wait_until_blocked(&db);
                holder
                    .execute(if commits { "COMMIT" } else { "ROLLBACK" })
                    .unwrap();
                let result = waiting.join().unwrap();
                if repeatable && changes_row && commits {
                    assert_eq!(result.unwrap_err().sqlstate, SqlState::SerializationFailure);
                } else {
                    let id = if changes_row && commits { 2 } else { 1 };
                    assert_eq!(result.unwrap().rows, vec![vec![Value::Int4(id)]]);
                }
            }
        }
    }
}

#[test]
fn refreshes_derived_projections_and_join_predicates_after_waits() {
    for (query, expected) in [
        (
            "SELECT a.id,b.id FROM (refresh_rows a JOIN refresh_rows b ON a.v=b.v) WHERE a.id=1 FOR UPDATE OF a",
            vec![],
        ),
        (
            "SELECT a.id,b.id FROM refresh_rows a LEFT JOIN refresh_rows b ON a.v=b.v WHERE a.id=1 FOR UPDATE OF a",
            vec![vec![Value::Int4(1), Value::Null]],
        ),
        (
            "SELECT a.id,b.id FROM refresh_rows a LEFT JOIN refresh_rows b ON a.v=-b.v WHERE a.id=1 FOR UPDATE OF a",
            vec![vec![Value::Int4(1), Value::Null]],
        ),
        (
            "SELECT id,w FROM (SELECT id,v*2 w FROM refresh_rows) x ORDER BY w LIMIT 1 FOR UPDATE",
            vec![vec![Value::Int4(1), Value::Int4(198)]],
        ),
        (
            "SELECT id,w FROM (SELECT id,v*2 w FROM refresh_rows) x WHERE w<100 ORDER BY w LIMIT 1 FOR UPDATE",
            vec![vec![Value::Int4(2), Value::Int4(40)]],
        ),
        (
            "SELECT a.id,b.id FROM refresh_rows a JOIN refresh_rows b ON a.v=b.v WHERE a.id=1 FOR UPDATE OF a",
            vec![],
        ),
    ] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(3))
            .build();
        let mut holder = db.create_session();
        let mut waiter = db.create_session();
        holder.execute("CREATE TABLE refresh_rows(id INT PRIMARY KEY,v INT); INSERT INTO refresh_rows VALUES(1,10),(2,20)").unwrap();
        holder
            .execute("BEGIN; SELECT id FROM refresh_rows WHERE id=1 FOR UPDATE")
            .unwrap();
        let waiting = thread::spawn(move || waiter.query(query, &[]));
        wait_until_blocked(&db);
        holder
            .execute("UPDATE refresh_rows SET v=99 WHERE id=1; COMMIT")
            .unwrap();
        assert_eq!(waiting.join().unwrap().unwrap().rows, expected, "{query}");
    }
}

#[test]
fn grants_compatible_row_locks_while_an_update_waits() {
    for (held, requested) in [
        ("KEY SHARE", "KEY SHARE"),
        ("KEY SHARE", "SHARE"),
        ("KEY SHARE", "NO KEY UPDATE"),
        ("SHARE", "KEY SHARE"),
        ("SHARE", "SHARE"),
        ("NO KEY UPDATE", "KEY SHARE"),
    ] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(3))
            .build();
        let mut holder = db.create_session();
        let mut waiter = db.create_session();
        let mut compatible = db.create_session();
        holder
            .execute("CREATE TABLE queued_locks(id INT); INSERT INTO queued_locks VALUES(1)")
            .unwrap();
        holder
            .execute(&format!("BEGIN; SELECT id FROM queued_locks FOR {held}"))
            .unwrap();
        let waiting =
            thread::spawn(move || waiter.query("SELECT id FROM queued_locks FOR UPDATE", &[]));
        wait_until_blocked(&db);
        assert_eq!(
            compatible
                .query(
                    &format!("SELECT id FROM queued_locks FOR {requested} NOWAIT"),
                    &[]
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        holder.execute("COMMIT").unwrap();
        assert_eq!(
            waiting.join().unwrap().unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }
}

#[test]
fn wakes_waiters_after_savepoint_rollback_and_error() {
    for fail in [false, true] {
        let db = Db::create();
        let mut holder = db.create_session();
        holder.execute("CREATE TABLE savepoint_wait(id INT PRIMARY KEY, value INT); INSERT INTO savepoint_wait VALUES(1, 10)").unwrap();
        holder
            .execute("BEGIN; SAVEPOINT s; UPDATE savepoint_wait SET value = 20")
            .unwrap();
        let mut waiter = db.create_session();
        let (sender, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            sender
                .send(waiter.execute("UPDATE savepoint_wait SET value = value + 1"))
                .unwrap();
        });
        wait_until_blocked(&db);
        if fail {
            assert_eq!(
                holder.execute("SELECT 1 / 0").unwrap_err().sqlstate,
                SqlState::DivisionByZero
            );
        } else {
            holder.execute("ROLLBACK TO s").unwrap();
        }
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        thread.join().unwrap();
        holder.execute("ROLLBACK TO s; COMMIT").unwrap();
        assert_eq!(
            holder
                .query("SELECT value FROM savepoint_wait", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(11)]]
        );
    }
}

#[test]
fn wakes_relation_waiter_after_savepoint_rollback() {
    let db = Db::create();
    let mut holder = db.create_session();
    holder
        .execute("CREATE TABLE savepoint_relation(id INT)")
        .unwrap();
    holder
        .execute("BEGIN; SAVEPOINT s; LOCK TABLE savepoint_relation IN ACCESS EXCLUSIVE MODE")
        .unwrap();
    let mut waiter = db.create_session();
    let (sender, receiver) = mpsc::channel();
    let thread = thread::spawn(move || {
        sender
            .send(waiter.execute("INSERT INTO savepoint_relation VALUES(1)"))
            .unwrap();
    });
    wait_until_relation_blocked(&db);
    holder.execute("ROLLBACK TO s").unwrap();
    receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    holder.execute("COMMIT").unwrap();
}

#[test]
fn restores_timeout_settings_and_prepared_recovery_at_savepoints() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("BEGIN; SET LOCAL lock_timeout = '2s'; SAVEPOINT s; SET lock_timeout = '3s'; SET LOCAL statement_timeout = '4s'").unwrap();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(3));
    assert_eq!(session.settings.statement_timeout, Duration::from_secs(4));
    session.query("SELECT 1 / 0", &[]).unwrap_err();
    session.execute_params("ROLLBACK TO s", &[]).unwrap();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(2));
    assert_eq!(session.settings.statement_timeout, Duration::ZERO);
    session.execute("COMMIT").unwrap();
    assert_eq!(session.settings.lock_timeout, Duration::from_secs(1));
}

#[test]
fn tracks_serializable_unique_key_gaps_and_snapshot_visibility() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    writer
        .execute("CREATE TABLE serializable_keys (id INT PRIMARY KEY)")
        .unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    writer.execute("BEGIN").unwrap();
    let Some(SessionTransactionState::Active(reader_transaction)) = reader.transaction else {
        panic!("reader transaction is active")
    };
    let Some(SessionTransactionState::Active(writer_transaction)) = writer.transaction else {
        panic!("writer transaction is active")
    };
    assert!(
        reader
            .query("SELECT id FROM serializable_keys WHERE id = 5", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    writer
        .execute("INSERT INTO serializable_keys VALUES (6)")
        .unwrap();
    assert!(
        !db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(reader_transaction.xid, writer_transaction.xid)
    );
    writer
        .execute("INSERT INTO serializable_keys VALUES (5)")
        .unwrap();
    assert!(
        db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(reader_transaction.xid, writer_transaction.xid)
    );
    writer.execute("COMMIT").unwrap();
    assert!(
        reader
            .query("SELECT id FROM serializable_keys WHERE id = 5", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    reader.execute("COMMIT").unwrap();
    assert!(
        !db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(reader_transaction.xid, writer_transaction.xid)
    );
}

#[test]
fn removes_rolled_back_serializable_writes_but_keeps_reads() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut writer = db.create_session();
    writer
        .execute("CREATE TABLE serializable_savepoints (id INT PRIMARY KEY)")
        .unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    writer.execute("BEGIN; SAVEPOINT before_insert").unwrap();
    let Some(SessionTransactionState::Active(reader_transaction)) = reader.transaction else {
        panic!("reader transaction is active")
    };
    let Some(SessionTransactionState::Active(writer_transaction)) = writer.transaction else {
        panic!("writer transaction is active")
    };
    reader
        .query("SELECT id FROM serializable_savepoints WHERE id = 7", &[])
        .unwrap();
    writer
        .execute("INSERT INTO serializable_savepoints VALUES (7)")
        .unwrap();
    assert!(
        db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(reader_transaction.xid, writer_transaction.xid)
    );
    writer.execute("ROLLBACK TO before_insert").unwrap();
    assert!(
        !db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(reader_transaction.xid, writer_transaction.xid)
    );
    writer
        .execute("INSERT INTO serializable_savepoints VALUES (7)")
        .unwrap();
    assert!(
        db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(reader_transaction.xid, writer_transaction.xid)
    );
    writer.execute("ROLLBACK").unwrap();
    reader.execute("COMMIT").unwrap();
}

#[test]
fn keeps_serializable_read_write_transactions_on_their_first_snapshot() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE serializable_snapshot (id INT PRIMARY KEY, value INT); INSERT INTO serializable_snapshot VALUES (1, 10)")
        .unwrap();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    assert_eq!(
        first
            .query("SELECT value FROM serializable_snapshot WHERE id = 1", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(10)]]
    );
    second
        .execute("INSERT INTO serializable_snapshot VALUES (2, 20)")
        .unwrap();
    first
        .execute("UPDATE serializable_snapshot SET value = 11 WHERE id = 1")
        .unwrap();
    assert!(
        first
            .query("SELECT id FROM serializable_snapshot WHERE id = 2", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    first.execute("COMMIT").unwrap();
}

#[test]
fn discovers_writes_started_before_a_serializable_snapshot() {
    let db = Db::create();
    let mut writer = db.create_session();
    let mut reader = db.create_session();
    writer
        .execute("CREATE TABLE serializable_prior_write (id INT PRIMARY KEY)")
        .unwrap();
    writer
        .execute("BEGIN; INSERT INTO serializable_prior_write VALUES (5)")
        .unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    let Some(SessionTransactionState::Active(reader_transaction)) = reader.transaction else {
        panic!("reader transaction is active")
    };
    let Some(SessionTransactionState::Active(writer_transaction)) = writer.transaction else {
        panic!("writer transaction is active")
    };
    assert!(
        reader
            .query("SELECT id FROM serializable_prior_write WHERE id = 5", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(reader_transaction.xid, writer_transaction.xid)
    );
    writer.execute("COMMIT").unwrap();
    assert!(
        reader
            .query("SELECT id FROM serializable_prior_write WHERE id = 5", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    reader.execute("COMMIT").unwrap();
}

#[test]
fn records_both_edges_of_a_write_skew_schedule() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TABLE serializable_skew (id INT PRIMARY KEY, value INT); INSERT INTO serializable_skew VALUES (1, 0), (2, 0)").unwrap();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    let Some(SessionTransactionState::Active(first_transaction)) = first.transaction else {
        panic!("first transaction is active")
    };
    let Some(SessionTransactionState::Active(second_transaction)) = second.transaction else {
        panic!("second transaction is active")
    };
    first
        .query("SELECT value FROM serializable_skew WHERE id = 2", &[])
        .unwrap();
    second
        .query("SELECT value FROM serializable_skew WHERE id = 1", &[])
        .unwrap();
    first
        .execute("UPDATE serializable_skew SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("UPDATE serializable_skew SET value = 1 WHERE id = 2")
        .unwrap();
    let state = db.state.lock().unwrap();
    let graph = state.serializable.lock().unwrap();
    assert!(graph.has_edge(first_transaction.xid, second_transaction.xid));
    assert!(graph.has_edge(second_transaction.xid, first_transaction.xid));
    drop(graph);
    drop(state);
    first.execute("ROLLBACK").unwrap();
    second.execute("ROLLBACK").unwrap();
}

#[test]
fn rejects_write_skew_and_preserves_the_winning_commit() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TABLE skew_commit (id INT PRIMARY KEY, value INT); INSERT INTO skew_commit VALUES (1, 0), (2, 0)").unwrap();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    first
        .query("SELECT value FROM skew_commit WHERE id = 2", &[])
        .unwrap();
    second
        .query("SELECT value FROM skew_commit WHERE id = 1", &[])
        .unwrap();
    first
        .execute("UPDATE skew_commit SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("UPDATE skew_commit SET value = 1 WHERE id = 2")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
    assert_eq!(
        first
            .query("SELECT value FROM skew_commit ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(0)]]
    );
}

#[test]
fn rejects_phantom_key_insert_and_allows_independent_writes() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute("CREATE TABLE phantom_keys (id INT PRIMARY KEY)")
        .unwrap();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    assert!(
        first
            .query("SELECT id FROM phantom_keys WHERE id = 2", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        second
            .query("SELECT id FROM phantom_keys WHERE id = 1", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    first
        .execute("INSERT INTO phantom_keys VALUES (1)")
        .unwrap();
    second
        .execute("INSERT INTO phantom_keys VALUES (2)")
        .unwrap();
    second.execute("COMMIT").unwrap();
    assert_eq!(
        first.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    first
        .query("SELECT id FROM phantom_keys WHERE id = 9", &[])
        .unwrap();
    second
        .query("SELECT id FROM phantom_keys WHERE id = 8", &[])
        .unwrap();
    first
        .execute("INSERT INTO phantom_keys VALUES (8)")
        .unwrap();
    second
        .execute("INSERT INTO phantom_keys VALUES (9)")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    first
        .query("SELECT id FROM phantom_keys WHERE id = 100", &[])
        .unwrap();
    second
        .query("SELECT id FROM phantom_keys WHERE id = 200", &[])
        .unwrap();
    first
        .execute("INSERT INTO phantom_keys VALUES (101)")
        .unwrap();
    second
        .execute("INSERT INTO phantom_keys VALUES (201)")
        .unwrap();
    first.execute("COMMIT").unwrap();
    second.execute("COMMIT").unwrap();
}

#[test]
fn randomized_write_skew_schedules_have_serializable_commits() {
    let mut seed = 0x5eed_u64;
    for _ in 0..64 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let first_reads_first = seed & 1 == 0;
        let first_writes_first = seed & 2 == 0;
        let first_commits_first = seed & 4 == 0;
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first.execute("CREATE TABLE schedule_rows(id INT PRIMARY KEY, value INT); INSERT INTO schedule_rows VALUES (1, 0), (2, 0)").unwrap();
        first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
        second
            .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .unwrap();
        let read_first = "SELECT value FROM schedule_rows WHERE id = 2";
        let read_second = "SELECT value FROM schedule_rows WHERE id = 1";
        if first_reads_first {
            first.query(read_first, &[]).unwrap();
            second.query(read_second, &[]).unwrap();
        } else {
            second.query(read_second, &[]).unwrap();
            first.query(read_first, &[]).unwrap();
        }
        let write_first = "UPDATE schedule_rows SET value = 1 WHERE id = 1";
        let write_second = "UPDATE schedule_rows SET value = 1 WHERE id = 2";
        if first_writes_first {
            first.execute(write_first).unwrap();
            second.execute(write_second).unwrap();
        } else {
            second.execute(write_second).unwrap();
            first.execute(write_first).unwrap();
        }
        let outcomes = if first_commits_first {
            [first.execute("COMMIT"), second.execute("COMMIT")]
        } else {
            [second.execute("COMMIT"), first.execute("COMMIT")]
        };
        assert!(outcomes[0].is_ok());
        assert_eq!(
            outcomes[1].as_ref().unwrap_err().sqlstate,
            SqlState::SerializationFailure
        );
        let values = first
            .query("SELECT value FROM schedule_rows ORDER BY id", &[])
            .unwrap()
            .rows;
        let serial_result = if first_commits_first {
            vec![vec![Value::Int4(1)], vec![Value::Int4(0)]]
        } else {
            vec![vec![Value::Int4(0)], vec![Value::Int4(1)]]
        };
        assert_eq!(values, serial_result);
    }
}

#[test]
fn detects_insert_phantoms_through_supported_query_shapes() {
    for query in [
        "SELECT id FROM watched WHERE id > 0",
        "SELECT count(*) FROM watched WHERE id > 0",
        "WITH matching AS (SELECT id FROM watched WHERE id > 0) SELECT count(*) FROM matching",
        "SELECT count(*) FROM watched w JOIN guard_rows g ON w.id = g.id",
        "SELECT count(*) FROM watched_view",
    ] {
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first.execute("CREATE TABLE watched(id INT PRIMARY KEY); CREATE TABLE guard_rows(id INT PRIMARY KEY, value INT); INSERT INTO guard_rows VALUES (1, 0); CREATE VIEW watched_view AS SELECT id FROM watched").unwrap();
        first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
        second
            .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .unwrap();
        first.query(query, &[]).unwrap();
        second
            .query("SELECT value FROM guard_rows WHERE id = 1", &[])
            .unwrap();
        second.execute("INSERT INTO watched VALUES (1)").unwrap();
        first
            .execute("UPDATE guard_rows SET value = 1 WHERE id = 1")
            .unwrap();
        second.execute("COMMIT").unwrap();
        assert_eq!(
            first.execute("COMMIT").unwrap_err().sqlstate,
            SqlState::SerializationFailure,
            "query: {query}"
        );
    }
}

#[test]
fn rejects_read_only_anomaly_only_when_the_outgoing_writer_precedes_its_snapshot() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut pivot = db.create_session();
    let mut outgoing = db.create_session();
    reader.execute("CREATE TABLE anomaly_rows(id INT PRIMARY KEY, value INT); INSERT INTO anomaly_rows VALUES (1, 0), (2, 0)").unwrap();
    pivot.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    pivot
        .query("SELECT value FROM anomaly_rows WHERE id = 2", &[])
        .unwrap();
    outgoing
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    outgoing
        .execute("UPDATE anomaly_rows SET value = 1 WHERE id = 2")
        .unwrap();
    outgoing.execute("COMMIT").unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    reader
        .query("SELECT value FROM anomaly_rows WHERE id = 2", &[])
        .unwrap();
    pivot
        .execute("UPDATE anomaly_rows SET value = 1 WHERE id = 1")
        .unwrap();
    pivot.execute("COMMIT").unwrap();
    assert_eq!(
        reader
            .query("SELECT value FROM anomaly_rows WHERE id = 1", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::SerializationFailure
    );
    assert_eq!(
        reader.query("SELECT 1", &[]).unwrap_err().sqlstate,
        SqlState::InFailedSqlTransaction
    );
    reader.execute("ROLLBACK").unwrap();
}

#[test]
fn allows_read_only_snapshot_taken_before_the_outgoing_writer_commits() {
    let db = Db::create();
    let mut reader = db.create_session();
    let mut pivot = db.create_session();
    let mut outgoing = db.create_session();
    reader.execute("CREATE TABLE safe_reader_rows(id INT PRIMARY KEY, value INT); INSERT INTO safe_reader_rows VALUES (1, 0), (2, 0)").unwrap();
    pivot.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    reader
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    pivot
        .query("SELECT value FROM safe_reader_rows WHERE id = 2", &[])
        .unwrap();
    reader
        .query("SELECT value FROM safe_reader_rows WHERE id = 1", &[])
        .unwrap();
    outgoing
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    outgoing
        .execute("UPDATE safe_reader_rows SET value = 1 WHERE id = 2")
        .unwrap();
    outgoing.execute("COMMIT").unwrap();
    pivot
        .execute("UPDATE safe_reader_rows SET value = 1 WHERE id = 1")
        .unwrap();
    pivot.execute("COMMIT").unwrap();
    reader
        .query("SELECT value FROM safe_reader_rows WHERE id = 1", &[])
        .unwrap();
    reader.execute("COMMIT").unwrap();
}

#[test]
fn detects_delete_phantoms_and_allows_do_nothing() {
    for use_conflict in [false, true] {
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first.execute("CREATE TABLE conflict_rows(id INT PRIMARY KEY, value INT); INSERT INTO conflict_rows VALUES (1, 0); CREATE TABLE conflict_guard(id INT PRIMARY KEY, value INT); INSERT INTO conflict_guard VALUES (1, 0)").unwrap();
        first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
        second
            .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .unwrap();
        if use_conflict {
            first
                .execute("INSERT INTO conflict_rows VALUES (1, 9) ON CONFLICT (id) DO NOTHING")
                .unwrap();
        } else {
            first
                .query("SELECT count(*) FROM conflict_rows WHERE id > 0", &[])
                .unwrap();
        }
        second
            .query("SELECT value FROM conflict_guard WHERE id = 1", &[])
            .unwrap();
        if use_conflict {
            first
                .execute("UPDATE conflict_guard SET value = 1 WHERE id = 1")
                .unwrap();
            first.execute("COMMIT").unwrap();
            second
                .execute("UPDATE conflict_rows SET value = 2 WHERE id = 1")
                .unwrap();
            second.execute("COMMIT").unwrap();
        } else {
            second
                .execute("DELETE FROM conflict_rows WHERE id = 1")
                .unwrap();
            first
                .execute("UPDATE conflict_guard SET value = 1 WHERE id = 1")
                .unwrap();
            second.execute("COMMIT").unwrap();
            assert_eq!(
                first.execute("COMMIT").unwrap_err().sqlstate,
                SqlState::SerializationFailure
            );
        }
    }
}

#[test]
fn serialization_failure_releases_locks_and_keeps_sequence_allocation() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TABLE failure_cleanup(id INT PRIMARY KEY, value INT); INSERT INTO failure_cleanup VALUES (1, 0), (2, 0); CREATE SEQUENCE failure_sequence").unwrap();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second
        .execute("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .unwrap();
    first
        .query("SELECT value FROM failure_cleanup WHERE id = 2", &[])
        .unwrap();
    second
        .query("SELECT value FROM failure_cleanup WHERE id = 1", &[])
        .unwrap();
    second
        .query("SELECT nextval('failure_sequence')", &[])
        .unwrap();
    first
        .execute("UPDATE failure_cleanup SET value = 1 WHERE id = 1")
        .unwrap();
    second
        .execute("UPDATE failure_cleanup SET value = 1 WHERE id = 2")
        .unwrap();
    first.execute("COMMIT").unwrap();
    assert_eq!(
        second.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::SerializationFailure
    );
    assert_eq!(
        second
            .query("SELECT nextval('failure_sequence')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int8(2)]]
    );
    second
        .execute("UPDATE failure_cleanup SET value = 2 WHERE id = 2")
        .unwrap();
}

#[test]
fn checks_deferred_foreign_keys_against_the_serializable_snapshot() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TABLE serializable_parent (id INT PRIMARY KEY); CREATE TABLE serializable_child (id INT PRIMARY KEY, parent_id INT REFERENCES serializable_parent DEFERRABLE INITIALLY DEFERRED)").unwrap();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    first
        .query("SELECT id FROM serializable_parent", &[])
        .unwrap();
    first
        .execute("INSERT INTO serializable_child VALUES (1, 7)")
        .unwrap();
    second
        .execute("INSERT INTO serializable_parent VALUES (7)")
        .unwrap();
    assert_eq!(
        first.execute("COMMIT").unwrap_err().sqlstate,
        SqlState::ForeignKeyViolation
    );
}

#[test]
fn does_not_add_predicate_read_for_do_nothing() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE TABLE serializable_conflict (id INT PRIMARY KEY, value INT); INSERT INTO serializable_conflict VALUES (1, 0)").unwrap();
    first.execute("BEGIN ISOLATION LEVEL SERIALIZABLE").unwrap();
    second.execute("BEGIN").unwrap();
    let Some(SessionTransactionState::Active(first_transaction)) = first.transaction else {
        panic!("first transaction is active")
    };
    let Some(SessionTransactionState::Active(second_transaction)) = second.transaction else {
        panic!("second transaction is active")
    };
    first
        .execute("INSERT INTO serializable_conflict VALUES (1, 9) ON CONFLICT (id) DO NOTHING")
        .unwrap();
    first.execute("COMMIT").unwrap();
    second
        .execute("UPDATE serializable_conflict SET value = 2 WHERE id = 1")
        .unwrap();
    assert!(
        !db.state
            .lock()
            .unwrap()
            .serializable
            .lock()
            .unwrap()
            .has_edge(first_transaction.xid, second_transaction.xid)
    );
    second.execute("ROLLBACK").unwrap();
}
