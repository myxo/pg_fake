use pg_fake::{
    api::{Db, StatementResult},
    error::SqlState,
    value::{BaseType, Value},
};

#[test]
fn preserves_typed_parameter_annotations_and_nulls() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE typed_read (id INTEGER PRIMARY KEY); INSERT INTO typed_read VALUES (1)",
        )
        .unwrap();
    let lookup = session
        .prepare_with_parameter_types(
            "SELECT id FROM typed_read WHERE id = CAST($1 AS INTEGER)",
            &[Some(BaseType::Int4)],
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&lookup, &[Value::Int2(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    assert!(
        session
            .query_prepared(&lookup, &[Value::Null])
            .unwrap()
            .rows
            .is_empty()
    );
    let projection = session
        .prepare_with_parameter_types(
            "SELECT CAST(CAST($1 AS INTEGER) AS INTEGER) FROM typed_read",
            &[Some(BaseType::Int4)],
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&projection, &[Value::Null])
            .unwrap()
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        session
            .query_prepared(&projection, &[Value::Int4(7)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(7)]]
    );
}

#[test]
fn preserves_converting_and_length_limited_parameter_casts() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE cast_source (id INTEGER); INSERT INTO cast_source VALUES (1)")
        .unwrap();
    let narrowing = session
        .prepare_with_parameter_types(
            "SELECT CAST($1 AS SMALLINT) FROM cast_source",
            &[Some(BaseType::Int4)],
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&narrowing, &[Value::Int4(7)])
            .unwrap()
            .rows,
        vec![vec![Value::Int2(7)]]
    );
    assert_eq!(
        session
            .query_prepared(&narrowing, &[Value::Int4(40000)])
            .unwrap_err()
            .sqlstate,
        SqlState::NumericValueOutOfRange
    );
    let limited = session
        .prepare_with_parameter_types(
            "SELECT CAST($1 AS VARCHAR(2)) FROM cast_source",
            &[Some(BaseType::Text)],
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&limited, &[Value::Text("abc".into())])
            .unwrap()
            .rows,
        vec![vec![Value::Text("ab".into())]]
    );
}

#[test]
fn preserves_prepared_aggregate_metadata_and_empty_lookup_results() {
    let mut session = Db::create().create_session();
    session
        .execute("CREATE TABLE aggregate_reads (id INTEGER PRIMARY KEY, value INTEGER)")
        .unwrap();
    let sql = "SELECT count(*) AS n, sum(value) AS total, avg(value) AS mean FROM aggregate_reads";
    let prepared = session.prepare(sql).unwrap();
    let empty = session.query_prepared(&prepared, &[]).unwrap();
    assert_eq!(
        empty.rows,
        vec![vec![Value::Int8(0), Value::Null, Value::Null]]
    );
    assert_eq!(
        empty
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![("n", 20, -1), ("total", 20, -1), ("mean", 1700, -1)]
    );
    assert_eq!(
        session.execute(sql).unwrap(),
        vec![StatementResult::Query(empty)]
    );
    session
        .execute("INSERT INTO aggregate_reads VALUES (1, 7), (2, NULL)")
        .unwrap();
    let lookup = session.prepare(&format!("{sql} WHERE id = $1")).unwrap();
    for (id, count) in [(Value::Int4(99), 0), (Value::Int4(2), 1), (Value::Null, 0)] {
        assert_eq!(
            session.query_prepared(&lookup, &[id]).unwrap().rows,
            vec![vec![Value::Int8(count), Value::Null, Value::Null]]
        );
    }
    assert_eq!(
        session
            .query_prepared(&lookup, &[Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![
            Value::Int8(1),
            Value::Int8(7),
            Value::Numeric("7".parse().unwrap())
        ]]
    );
    let parameters = session
        .prepare_with_parameter_types(
            "SELECT sum($1), avg($1) FROM aggregate_reads WHERE id = $2",
            &[Some(BaseType::Int8), Some(BaseType::Int4)],
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&parameters, &[Value::Int4(9), Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![
            Value::Numeric("9".parse().unwrap()),
            Value::Numeric("9".parse().unwrap())
        ]]
    );
    assert_eq!(
        session
            .query_prepared(&parameters, &[Value::Null, Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Null, Value::Null]]
    );
}

#[test]
fn preserves_compiled_aggregate_error_precedence() {
    let mut session = Db::create().create_session();
    session.execute("CREATE TABLE aggregate_errors (id INTEGER, value REAL, flag BOOLEAN);         INSERT INTO aggregate_errors VALUES (1, 3e38, TRUE), (2, 3e38, FALSE), (3, 3e38, NULL)").unwrap();
    for (sql, expected) in [
        (
            "SELECT sum(value), sum(1 / (3 - id)) FROM aggregate_errors",
            SqlState::DivisionByZero,
        ),
        (
            "SELECT sum(value) FROM aggregate_errors",
            SqlState::NumericValueOutOfRange,
        ),
    ] {
        let prepared = session.prepare(sql).unwrap();
        assert_eq!(
            session.query_prepared(&prepared, &[]).unwrap_err().sqlstate,
            expected
        );
        assert_eq!(session.execute(sql).unwrap_err().sqlstate, expected);
    }
    assert_eq!(
        session
            .execute("SELECT sum(flag) FROM aggregate_errors WHERE 1")
            .unwrap_err()
            .sqlstate,
        SqlState::DatatypeMismatch
    );
}
