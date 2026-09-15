use pg_fake::{
    Db,
    error::SqlState,
    value::{BaseType, Value},
};

#[test]
fn stores_parses_and_formats_supported_arrays() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE array_values (id INTEGER PRIMARY KEY, numbers BIGINT[] NOT NULL DEFAULT ARRAY[1::BIGINT, 2], identifiers UUID[], labels TEXT[])",
        )
        .unwrap();
    session
        .execute(
            r#"INSERT INTO array_values (id, identifiers, labels) VALUES (1, ARRAY['a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7'::UUID, NULL], '{plain,"comma,value",NULL}')"#,
        )
        .unwrap();
    let result = session
        .query(
            "SELECT numbers, identifiers, labels, ARRAY[]::BIGINT[], '{-3,NULL,7}'::BIGINT[] FROM array_values",
            &[],
        )
        .unwrap();
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.type_oid)
            .collect::<Vec<_>>(),
        vec![1016, 2951, 1009, 1016, 1016]
    );
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Array {
                elem_type: BaseType::Int8,
                values: vec![Value::Int8(1), Value::Int8(2)],
            },
            Value::Array {
                elem_type: BaseType::Uuid,
                values: vec![
                    Value::Uuid(
                        uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7").unwrap(),
                    ),
                    Value::Null,
                ],
            },
            Value::Array {
                elem_type: BaseType::Text,
                values: vec![
                    Value::Text("plain".into()),
                    Value::Text("comma,value".into()),
                    Value::Null,
                ],
            },
            Value::Array {
                elem_type: BaseType::Int8,
                values: Vec::new(),
            },
            Value::Array {
                elem_type: BaseType::Int8,
                values: vec![Value::Int8(-3), Value::Null, Value::Int8(7)],
            },
        ]]
    );
    assert_eq!(
        session
            .execute(
                "INSERT INTO array_values (id, identifiers) VALUES (2, ARRAY['a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7', NULL])",
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DatatypeMismatch
    );
}

#[test]
fn rejects_unsupported_array_shapes_and_malformed_input() {
    let db = Db::create();
    let mut session = db.create_session();
    for sql in [
        "SELECT ARRAY[1, 2]",
        "SELECT ARRAY[[1::BIGINT]]",
        "SELECT '[0:1]={1,2}'::BIGINT[]",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported,
            "{sql}"
        );
    }
    assert_eq!(
        session.query("SELECT ARRAY[]", &[]).unwrap_err().sqlstate,
        SqlState::IndeterminateDatatype
    );
    assert_eq!(
        session
            .query("SELECT '{1,,2}'::BIGINT[]", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    for sql in [
        "SELECT 1::BIGINT = ANY(ARRAY[1::BIGINT])",
        "SELECT '00000000-0000-4000-8000-000000000001'::UUID = ALL(ARRAY[]::UUID[])",
        "SELECT '00000000-0000-4000-8000-000000000001'::UUID > ANY(ARRAY[]::UUID[])",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported,
            "{sql}"
        );
    }
}

#[test]
fn aggregates_subscripts_and_compares_required_arrays() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE array_queries (id INTEGER PRIMARY KEY, issued_at BIGINT, identifier UUID); \
             INSERT INTO array_queries VALUES \
                (1, 10, '00000000-0000-4000-8000-000000000001'), \
                (2, 20, '00000000-0000-4000-8000-000000000002'), \
                (3, 30, '00000000-0000-4000-8000-000000000003'), \
                (4, NULL, NULL)",
        )
        .unwrap();
    let result = session
        .query(
            "SELECT max(issued_at), \
                    (array_agg(issued_at ORDER BY issued_at DESC) \
                        FILTER (WHERE issued_at <= 30))[2], \
                    array_agg(issued_at ORDER BY id) \
             FROM array_queries",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Int8(30),
            Value::Int8(20),
            Value::Array {
                elem_type: BaseType::Int8,
                values: vec![
                    Value::Int8(10),
                    Value::Int8(20),
                    Value::Int8(30),
                    Value::Null,
                ],
            },
        ]]
    );
    let result = session
        .query(
            "SELECT identifier = ANY(ARRAY[\
                        '00000000-0000-4000-8000-000000000001'::UUID, \
                        '00000000-0000-4000-8000-000000000003'::UUID]), \
                    identifier <> ALL(ARRAY[]::UUID[]) \
             FROM array_queries ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Bool(true), Value::Bool(true)],
            vec![Value::Bool(false), Value::Bool(true)],
            vec![Value::Bool(true), Value::Bool(true)],
            vec![Value::Null, Value::Bool(true)],
        ]
    );
}
