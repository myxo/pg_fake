use pg_fake::{
    api::Db,
    error::SqlState,
    value::{BaseType, Value},
};

fn query_rows(session: &mut pg_fake::api::Session, sql: &str) -> Vec<Vec<Value>> {
    session
        .query(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
        .rows
}

#[test]
fn preserves_json_text_through_literals_defaults_parameters_and_returning() {
    assert_eq!(BaseType::Json.map_to_oid(), 114);
    assert_eq!(BaseType::resolve_oid(114), Some(BaseType::Json));

    let db = Db::create();
    let mut session = db.create_session();
    let literal = r#"{ "z" : 1e+02, "z" : -0.00, "nested" : [true, null, "Привет 🌍"] }"#;
    assert_eq!(
        query_rows(&mut session, &format!("SELECT '{literal}'::json")),
        vec![vec![Value::Json(literal.into())]],
    );
    assert_eq!(
        query_rows(&mut session, "SELECT JSON '{\"typed\": true}'"),
        vec![vec![Value::Json(r#"{"typed": true}"#.into())]],
    );
    assert_eq!(
        query_rows(&mut session, "SELECT '{}'::pg_catalog.json"),
        vec![vec![Value::Json("{}".into())]],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT ' {\"a\":1} '::json UNION ALL SELECT '[2]'",
        ),
        vec![
            vec![Value::Json(r#" {"a":1} "#.into())],
            vec![Value::Json("[2]".into())],
        ],
    );
    assert_eq!(
        query_rows(&mut session, "SELECT NULL UNION ALL SELECT '{}'::json"),
        vec![vec![Value::Null], vec![Value::Json("{}".into())]],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "(SELECT DISTINCT ON (position) '{}' AS payload, 1 AS position ORDER BY position) UNION ALL SELECT '{}'::json, 1",
        ),
        vec![
            vec![Value::Json("{}".into()), Value::Int4(1)],
            vec![Value::Json("{}".into()), Value::Int4(1)],
        ],
    );
    assert_eq!(
        session
            .query("SELECT $$''$$::json", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation,
    );
    let nested = format!("{}0{}", "[".repeat(512), "]".repeat(512));
    assert_eq!(
        query_rows(&mut session, &format!("SELECT '{nested}'::json")),
        vec![vec![Value::Json(nested)]],
    );
    for literal in [
        r#"{"$serde_json::private::Number":null}"#,
        r#"{"$serde_json::private::Number":"not a number"}"#,
        r#"{"$serde_json::private::Number":1,"next":true}"#,
    ] {
        assert_eq!(
            query_rows(&mut session, &format!("SELECT '{literal}'::json")),
            vec![vec![Value::Json(literal.into())]],
        );
    }

    session
        .execute(
            r#"CREATE TABLE json_documents (
                 id INTEGER,
                 payload JSON NOT NULL CHECK (payload IS NOT NULL),
                 fallback JSON DEFAULT '{ "default" : [1, 2, 3] }'
               )"#,
        )
        .unwrap();
    session
        .execute("INSERT INTO json_documents (id, payload, fallback) SELECT 0, '{}', NULL")
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT fallback FROM json_documents WHERE id = 0",
        ),
        vec![vec![Value::Null]],
    );
    let parameter = r#"{"outer":{"inner":[1,{"n":999999999999999999999999999999999999999999}]}}"#;
    let result = session
        .query(
            "INSERT INTO json_documents (id, payload) VALUES (1, $1) RETURNING payload, fallback",
            &[Value::Json(parameter.into())],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Json(parameter.into()),
            Value::Json(r#"{ "default" : [1, 2, 3] }"#.into()),
        ]],
    );
    assert_eq!(result.columns[0].type_oid, 114);
    assert_eq!(result.columns[1].type_oid, 114);

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT payload::text FROM json_documents WHERE id = 1",
        ),
        vec![vec![Value::Text(parameter.into())]],
    );
}

#[test]
fn rejects_malformed_json_atomically() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE json_documents (id INTEGER, payload JSON)")
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO json_documents VALUES (4, '{}'::text)")
            .unwrap_err()
            .sqlstate,
        SqlState::DatatypeMismatch,
    );
    assert_eq!(
        session
            .execute("INSERT INTO json_documents VALUES (1, '{\"ok\": true}'), (2, '[1,]')")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation,
    );
    assert!(query_rows(&mut session, "SELECT id FROM json_documents").is_empty());
    assert_eq!(
        session
            .execute("CREATE TABLE invalid_default (payload JSON DEFAULT '{')")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation,
    );
    assert_eq!(
        session
            .query(
                "INSERT INTO json_documents VALUES (3, $1)",
                &[Value::Json("{".into())],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation,
    );
    assert_eq!(
        session
            .query("SELECT coalesce('{}'::json, '['::json)", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation,
    );
    for sql in [
        "SELECT true OR ('['::json IS NULL)",
        "SELECT '[' WHERE false UNION ALL SELECT '{}'::json",
        "SELECT '{}'::json UNION ALL SELECT '[' WHERE false",
        "(SELECT '[' LIMIT 0) UNION ALL SELECT '{}'::json",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::InvalidTextRepresentation,
            "{sql}",
        );
    }
}

#[test]
fn rejects_json_equality_ordering_and_btree_keys() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE json_documents (payload JSON)")
        .unwrap();

    for sql in [
        "SELECT '{}'::json = '{}'::json",
        "SELECT '{}'::json IS DISTINCT FROM '{}'::json",
        "SELECT '{}'::json IN ('{}'::json)",
        "SELECT payload FROM json_documents ORDER BY payload",
        "SELECT payload FROM json_documents GROUP BY payload",
        "SELECT DISTINCT payload FROM json_documents",
        "SELECT count(DISTINCT payload) FROM json_documents",
        "SELECT '{}'::json UNION SELECT '{}'::json",
        "VALUES ('{}'::json) ORDER BY 1",
        "VALUES ('{}'::json), ('[]'::json) ORDER BY 1",
        "SELECT least('{}'::json)",
        "SELECT * FROM (VALUES ('{}'::json)) AS left_side(payload) JOIN (VALUES ('{}'::json)) AS right_side(payload) USING (payload)",
        "WITH RECURSIVE documents(payload) AS (SELECT '{}'::json UNION SELECT payload FROM documents WHERE false) SELECT * FROM documents",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::UndefinedFunction,
            "{sql}",
        );
    }
    session
        .query("SELECT '{}'::json UNION ALL SELECT '{}'::json", &[])
        .unwrap();

    for sql in [
        "CREATE INDEX json_documents_payload_idx ON json_documents (payload)",
        "ALTER TABLE json_documents ADD UNIQUE (payload)",
        "CREATE TABLE unique_json (payload JSON UNIQUE)",
    ] {
        assert_eq!(
            session.execute(sql).unwrap_err().sqlstate,
            SqlState::UndefinedObject,
            "{sql}",
        );
    }
    session
        .execute("CREATE TABLE json_parent (payload JSON)")
        .unwrap();
    assert_eq!(
        session
            .execute("CREATE TABLE json_child (payload JSON REFERENCES json_parent(payload))")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidForeignKey,
    );
}
