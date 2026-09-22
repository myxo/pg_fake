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

#[test]
fn supports_all_scalar_array_types_and_general_comparisons() {
    let db = Db::create();
    let mut session = db.create_session();
    let result = session
        .query(
            "SELECT ARRAY[true], ARRAY[1::smallint], ARRAY[1], ARRAY[1::bigint], \
                    ARRAY[1::oid], ARRAY[1::real], ARRAY[1::double precision], \
                    ARRAY[1::numeric], ARRAY['x'::text], ARRAY['x'::varchar], \
                    ARRAY['x'::char], ARRAY['\\x01'::bytea], \
                    ARRAY['00000000-0000-4000-8000-000000000001'::uuid], \
                    ARRAY['2024-01-02'::date], ARRAY['03:04:05'::time], \
                    ARRAY['2024-01-02 03:04:05'::timestamp], \
                    ARRAY['2024-01-02 03:04:05+00'::timestamptz], \
                    ARRAY['1 day'::interval], ARRAY['{}'::json], ARRAY['{}'::jsonb], \
                    ARRAY['0/10'::pg_lsn], ARRAY[1::oid::regclass]",
            &[],
        )
        .unwrap();
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.type_oid)
            .collect::<Vec<_>>(),
        vec![
            1000, 1005, 1007, 1016, 1028, 1021, 1022, 1231, 1009, 1015, 1014, 1001, 2951, 1182,
            1183, 1115, 1185, 1187, 199, 3807, 3221, 2210,
        ]
    );

    let result = session
        .query(
            "SELECT ARRAY[1, 2] = ARRAY[1, 2], ARRAY[1, 2] < ARRAY[1, 3], \
                    2 = ANY(ARRAY[1, 2, NULL]), 3 <> ALL(ARRAY[1, 2]), \
                    ARRAY[1, 2] || ARRAY[3], 0 || ARRAY[1, 2], ARRAY[1, 2] || 3, \
                    ARRAY[1, 1] @> ARRAY[1], ARRAY[1] <@ ARRAY[1, 2], \
                    ARRAY[1, 2] && ARRAY[2, 3]",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0],
        vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Array {
                elem_type: BaseType::Int4,
                values: vec![Value::Int4(1), Value::Int4(2), Value::Int4(3)],
            },
            Value::Array {
                elem_type: BaseType::Int4,
                values: vec![Value::Int4(0), Value::Int4(1), Value::Int4(2)],
            },
            Value::Array {
                elem_type: BaseType::Int4,
                values: vec![Value::Int4(1), Value::Int4(2), Value::Int4(3)],
            },
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
        ]
    );
}

#[test]
fn supports_array_functions_assignment_and_correlated_unnest() {
    let db = Db::create();
    let mut session = db.create_session();
    let result = session
        .query(
            "SELECT array_length(ARRAY[10, 20], 1), cardinality(ARRAY[]::int[]), \
                    array_lower(ARRAY[10, 20], 1), array_upper(ARRAY[10, 20], 1), \
                    array_append(ARRAY[10], 20), array_prepend(5, ARRAY[10]), \
                    array_cat(ARRAY[1], ARRAY[2, 3]), \
                    array_position(ARRAY[1, NULL, 2], NULL), \
                    array_positions(ARRAY[1, 2, 1], 1), \
                    array_remove(ARRAY[1, 2, 1], 1), \
                    array_replace(ARRAY[1, 2, 1], 1, 9)",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0][0..4],
        [
            Value::Int4(2),
            Value::Int4(0),
            Value::Int4(1),
            Value::Int4(2)
        ]
    );
    assert_eq!(result.rows[0][7], Value::Int4(2));
    assert_eq!(
        result.rows[0][8],
        Value::Array {
            elem_type: BaseType::Int4,
            values: vec![Value::Int4(1), Value::Int4(3)],
        }
    );

    session
        .execute(
            "CREATE TABLE array_expansion (id integer primary key, values integer[]); \
             INSERT INTO array_expansion VALUES (1, ARRAY[10, 20]), (2, ARRAY[]::integer[]), (3, NULL); \
             UPDATE array_expansion SET values[2] = 25 WHERE id = 1; \
             UPDATE array_expansion SET values[3] = 30 WHERE id = 1",
        )
        .unwrap();
    let result = session
        .query(
            "SELECT t.id, u.value, u.position \
             FROM array_expansion AS t \
             CROSS JOIN LATERAL unnest(t.values) WITH ORDINALITY AS u(value, position) \
             ORDER BY t.id, u.position",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Int4(10), Value::Int8(1)],
            vec![Value::Int4(1), Value::Int4(25), Value::Int8(2)],
            vec![Value::Int4(1), Value::Int4(30), Value::Int8(3)],
        ]
    );
    let result = session
        .query(
            "SELECT t.id, u.value FROM array_expansion AS t \
             LEFT JOIN LATERAL unnest(t.values) AS u(value) ON true \
             ORDER BY t.id, u.value",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 5);
    assert_eq!(result.rows[3], vec![Value::Int4(2), Value::Null]);
    assert_eq!(result.rows[4], vec![Value::Int4(3), Value::Null]);
}

#[test]
fn matches_array_typmods_context_and_edge_semantics() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "SET TIME ZONE 'Europe/Moscow'; \
             CREATE TABLE array_named_relation (id integer); \
             CREATE TABLE array_typmods (labels varchar(3)[], amounts numeric(4,1)[], moments timestamp(0)[]); \
             INSERT INTO array_typmods VALUES ('{abc}', '{1.0}', '{2024-01-01 00:00:00}')",
        )
        .unwrap();

    let result = session
        .query(
            "SELECT '{abcd}'::varchar(3)[], '{12.34}'::numeric(4,1)[], \
                    '{2024-01-02 03:04:05}'::timestamptz[]::text, \
                    '{array_named_relation}'::regclass[]::text, \
                    ARRAY[DATE '2024-01-01', TIMESTAMP '2024-01-02'], \
                    ARRAY['a'::char(1)] = ARRAY['a '::char(2)], \
                    array_cat(NULL::int[], NULL::int[]) IS NULL, \
                    ARRAY[1] || NULL, NULL || ARRAY[1]",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0][0],
        Value::Array {
            elem_type: BaseType::Varchar,
            values: vec![Value::Text("abc".into())],
        }
    );
    assert_eq!(
        result.rows[0][2],
        Value::Text("{\"2024-01-02 03:04:05+03\"}".into())
    );
    assert_eq!(
        result.rows[0][3],
        Value::Text("{array_named_relation}".into())
    );
    assert_eq!(result.rows[0][5], Value::Bool(true));
    assert_eq!(result.rows[0][6], Value::Bool(true));
    assert_eq!(result.rows[0][7], result.rows[0][8]);

    let inferred = session
        .query(
            "SELECT array_append(NULL, 1), array_prepend(1, NULL), \
                    array_cat(ARRAY[1], NULL), array_cat(NULL, ARRAY[1]), \
                    array_cat(ARRAY[1], '{2}'), array_position(NULL, 1), \
                    array_positions(NULL, 1), array_remove(NULL, 1), \
                    array_replace(NULL, 1, 2)",
            &[],
        )
        .unwrap();
    assert_eq!(inferred.rows[0][0].format_postgres_text(), "{1}");
    assert_eq!(inferred.rows[0][1].format_postgres_text(), "{1}");
    assert_eq!(inferred.rows[0][2].format_postgres_text(), "{1}");
    assert_eq!(inferred.rows[0][3].format_postgres_text(), "{1}");
    assert_eq!(inferred.rows[0][4].format_postgres_text(), "{1,2}");
    assert!(inferred.rows[0][5..].iter().all(Value::is_null));

    assert_eq!(
        session
            .query("SELECT ARRAY['a'] || 'b'", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    for sql in [
        "SELECT array_position(ARRAY[]::json[], '{}'::json)",
        "SELECT array_positions(ARRAY[]::json[], '{}'::json)",
        "SELECT array_remove(ARRAY[]::json[], '{}'::json)",
        "SELECT array_replace(ARRAY[]::json[], '{}'::json, '{}'::json)",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::UndefinedFunction,
            "{sql}"
        );
    }

    assert_eq!(
        session
            .execute("UPDATE array_typmods SET labels[1] = 'abcd'")
            .unwrap_err()
            .sqlstate,
        SqlState::StringDataRightTruncation
    );
    session
        .execute(
            "UPDATE array_typmods SET amounts[1] = 12.34, \
                 moments[1] = TIMESTAMP '2024-01-01 00:00:00.6'",
        )
        .unwrap();
    let result = session
        .query("SELECT amounts, moments FROM array_typmods", &[])
        .unwrap();
    assert_eq!(result.rows[0][0].format_postgres_text(), "{12.3}");
    assert_eq!(
        result.rows[0][1].format_postgres_text(),
        "{\"2024-01-01 00:00:01\"}"
    );

    session
        .execute("CREATE TABLE array_lower_bound (values integer[]); INSERT INTO array_lower_bound VALUES (NULL), ('{}')")
        .unwrap();
    assert_eq!(
        session
            .execute("UPDATE array_lower_bound SET values[3] = 1")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
}
