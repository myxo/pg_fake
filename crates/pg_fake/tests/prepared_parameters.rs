use pg_fake::{
    api::Db,
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
