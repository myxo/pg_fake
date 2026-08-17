use std::{collections::BTreeSet, sync::Arc, thread};

use pg_fake::{
    api::Db,
    error::SqlState,
    value::{BaseType, Value},
};

fn get_int8(session: &mut pg_fake::api::Session, sql: &str) -> i64 {
    let result = session.query(sql, &[]).unwrap();
    assert_eq!(result.columns[0].type_oid, BaseType::Int8.map_to_oid());
    let [Value::Int8(value)] = result.rows[0].as_slice() else {
        panic!("query must return one bigint")
    };
    *value
}

#[test]
fn creates_sequences_with_options_and_cycles_at_bounds() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE SEQUENCE tickets AS smallint INCREMENT BY 3 MINVALUE 10 MAXVALUE 16 START WITH 13 CACHE 8 CYCLE")
        .unwrap();

    assert_eq!(get_int8(&mut session, "SELECT nextval('tickets')"), 13);
    assert_eq!(get_int8(&mut session, "SELECT nextval('tickets')"), 16);
    assert_eq!(get_int8(&mut session, "SELECT nextval('tickets')"), 10);
    assert_eq!(get_int8(&mut session, "SELECT nextval('tickets')"), 13);

    session
        .execute("CREATE SEQUENCE descending AS integer INCREMENT BY -2")
        .unwrap();
    assert_eq!(get_int8(&mut session, "SELECT nextval('descending')"), -1);
    assert_eq!(get_int8(&mut session, "SELECT nextval('descending')"), -3);
}

#[test]
fn tracks_currval_and_lastval_per_session() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first.execute("CREATE SEQUENCE sessions START 40").unwrap();

    assert_eq!(
        second
            .query("SELECT currval('sessions')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::ObjectNotInPrerequisiteState
    );
    assert_eq!(get_int8(&mut first, "SELECT nextval('sessions')"), 40);
    assert_eq!(get_int8(&mut second, "SELECT nextval('sessions')"), 41);
    assert_eq!(get_int8(&mut first, "SELECT currval('sessions')"), 40);
    assert_eq!(get_int8(&mut first, "SELECT lastval()"), 40);
    assert_eq!(get_int8(&mut second, "SELECT currval('sessions')"), 41);
    assert_eq!(get_int8(&mut second, "SELECT lastval()"), 41);
}

#[test]
fn applies_both_setval_forms() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE SEQUENCE configured MINVALUE 1 MAXVALUE 100")
        .unwrap();

    assert_eq!(
        get_int8(&mut session, "SELECT setval('configured', 20)"),
        20
    );
    assert_eq!(
        session.query("SELECT lastval()", &[]).unwrap_err().sqlstate,
        SqlState::ObjectNotInPrerequisiteState
    );
    assert_eq!(get_int8(&mut session, "SELECT currval('configured')"), 20);
    assert_eq!(get_int8(&mut session, "SELECT nextval('configured')"), 21);
    assert_eq!(
        get_int8(&mut session, "SELECT setval('configured', 50, false)"),
        50
    );
    assert_eq!(get_int8(&mut session, "SELECT currval('configured')"), 21);
    assert_eq!(get_int8(&mut session, "SELECT nextval('configured')"), 50);
}

#[test]
fn keeps_consumed_values_after_errors_and_rollbacks() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE SEQUENCE durable").unwrap();

    assert_eq!(
        session
            .query("SELECT nextval('durable'), 1 / 0", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::DivisionByZero
    );
    assert_eq!(get_int8(&mut session, "SELECT nextval('durable')"), 2);
    session.execute("BEGIN").unwrap();
    assert_eq!(get_int8(&mut session, "SELECT nextval('durable')"), 3);
    session.execute("ROLLBACK").unwrap();
    assert_eq!(get_int8(&mut session, "SELECT nextval('durable')"), 4);
}

#[test]
fn distinguishes_drop_recreate_and_relation_kinds() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE SEQUENCE recycled").unwrap();
    assert_eq!(get_int8(&mut session, "SELECT nextval('recycled')"), 1);
    session.execute("DROP SEQUENCE recycled").unwrap();
    session
        .execute("CREATE SEQUENCE recycled START 100")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT currval('recycled')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::ObjectNotInPrerequisiteState
    );
    assert_eq!(get_int8(&mut session, "SELECT nextval('recycled')"), 100);

    session
        .execute("CREATE TABLE ordinary (id INTEGER)")
        .unwrap();
    assert_eq!(
        session
            .execute("DROP SEQUENCE ordinary")
            .unwrap_err()
            .sqlstate,
        SqlState::WrongObjectType
    );
    assert_eq!(
        session
            .execute("CREATE SEQUENCE ordinary")
            .unwrap_err()
            .sqlstate,
        SqlState::DuplicateTable
    );
}

#[test]
fn reports_option_and_allocation_errors() {
    let db = Db::create();
    let mut session = db.create_session();
    for sql in [
        "CREATE SEQUENCE invalid_increment INCREMENT 0",
        "CREATE SEQUENCE invalid_bounds MINVALUE 10 MAXVALUE 5",
        "CREATE SEQUENCE invalid_start MINVALUE 1 MAXVALUE 5 START 6",
        "CREATE SEQUENCE invalid_cache CACHE 0",
        "CREATE SEQUENCE invalid_type AS numeric",
    ] {
        assert_eq!(
            session.execute(sql).unwrap_err().sqlstate,
            SqlState::InvalidParameterValue,
            "{sql}"
        );
    }
    session
        .execute("CREATE SEQUENCE bounded MAXVALUE 2")
        .unwrap();
    assert_eq!(get_int8(&mut session, "SELECT nextval('bounded')"), 1);
    assert_eq!(get_int8(&mut session, "SELECT nextval('bounded')"), 2);
    assert_eq!(
        session
            .query("SELECT nextval('bounded')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::SequenceGeneratorLimitExceeded
    );
    assert_eq!(
        session
            .query("SELECT setval('bounded', 3)", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::NumericValueOutOfRange
    );
}

#[test]
fn reuses_prepared_sequence_calls() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE SEQUENCE prepared").unwrap();
    let statement = session.prepare("SELECT nextval($1)").unwrap();
    assert_eq!(statement.get_parameter_types(), &[BaseType::Text]);

    for expected in 1..=3 {
        let result = session
            .query_prepared(&statement, &[Value::Text("prepared".into())])
            .unwrap();
        assert_eq!(result.rows, vec![vec![Value::Int8(expected)]]);
    }
}

#[test]
fn allocates_distinct_values_across_concurrent_sessions() {
    let db = Arc::new(Db::create());
    db.create_session()
        .execute("CREATE SEQUENCE concurrent")
        .unwrap();
    let workers = (0..4)
        .map(|_| {
            let db = db.clone();
            thread::spawn(move || {
                let mut session = db.create_session();
                (0..50)
                    .map(|_| get_int8(&mut session, "SELECT nextval('concurrent')"))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let values = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .collect::<BTreeSet<_>>();

    assert_eq!(values, (1..=200).collect());
}

#[test]
fn rolls_back_implicit_batch_ddl_but_not_sequence_values() {
    let db = Db::create();
    let mut session = db.create_session();
    assert_eq!(
        session
            .execute("CREATE SEQUENCE transient; SELECT 1 / 0")
            .unwrap_err()
            .sqlstate,
        SqlState::DivisionByZero
    );
    assert_eq!(
        session
            .query("SELECT nextval('transient')", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );

    session
        .execute("CREATE SEQUENCE restored START 10")
        .unwrap();
    assert_eq!(get_int8(&mut session, "SELECT nextval('restored')"), 10);
    assert_eq!(
        session
            .execute("DROP SEQUENCE restored; SELECT 1 / 0")
            .unwrap_err()
            .sqlstate,
        SqlState::DivisionByZero
    );
    assert_eq!(get_int8(&mut session, "SELECT nextval('restored')"), 11);
}

#[test]
fn rejects_sequence_ddl_in_explicit_transactions() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("BEGIN").unwrap();
    assert_eq!(
        session
            .execute("CREATE SEQUENCE blocked")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    session.execute("ROLLBACK").unwrap();
    session.execute("CREATE SEQUENCE existing").unwrap();
    session.execute("BEGIN").unwrap();
    assert_eq!(
        session
            .execute("DROP SEQUENCE existing")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(get_int8(&mut session, "SELECT nextval('existing')"), 1);
}

#[test]
fn evaluates_sequence_defaults_once_and_keeps_failed_insert_gaps() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE SEQUENCE defaults").unwrap();
    session
        .execute(
            "CREATE TABLE generated (id BIGINT DEFAULT nextval('defaults'), marker INTEGER UNIQUE)",
        )
        .unwrap();
    let first = session
        .query(
            "INSERT INTO generated (marker) VALUES (1) RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(first.rows, vec![vec![Value::Int8(1)]]);
    assert_eq!(
        session
            .execute("INSERT INTO generated (marker) VALUES (1)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    let third = session
        .query(
            "INSERT INTO generated (marker) VALUES (2) RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(third.rows, vec![vec![Value::Int8(3)]]);
}
