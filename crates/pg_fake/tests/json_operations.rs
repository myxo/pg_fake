use pg_fake::{api::Db, error::SqlState, jsonb::Jsonb, value::Value};

#[test]
fn executes_prepared_json_paths_and_lateral_expansion() {
    let db = Db::create();
    let mut session = db.create_session();
    let document =
        Value::Jsonb(Jsonb::parse(r#"{"amount":{"value":"12.50"},"tags":["a",null]}"#).unwrap());
    let indexed = session
        .query(
            "SELECT $1::json ->> $2::integer",
            &[Value::Json("[1,2]".into()), Value::Int4(1)],
        )
        .unwrap();
    assert_eq!(indexed.rows, vec![vec![Value::Text("2".into())]]);
    assert_eq!(
        session.prepare("SELECT $1 ->> 'a'").unwrap_err().sqlstate,
        SqlState::AmbiguousFunction
    );
    let path = Value::TextArray(vec![Some("amount".into()), Some("value".into())]);
    let query = session
        .prepare("SELECT ($1::jsonb #>> $2)::numeric::bigint AS amount")
        .unwrap();
    let result = session
        .query_prepared(&query, &[document.clone(), path])
        .unwrap();
    assert_eq!(result.columns[0].type_oid, 20);
    assert_eq!(result.rows, vec![vec![Value::Int8(13)]]);
    let query = session
        .prepare("SELECT * FROM jsonb_each($1) e ORDER BY key")
        .unwrap();
    let result = session.query_prepared(&query, &[document]).unwrap();
    assert_eq!(
        result
            .columns
            .iter()
            .map(|c| c.type_oid)
            .collect::<Vec<_>>(),
        vec![25, 3802]
    );
    assert_eq!(result.rows.len(), 2);
    session.execute("CREATE TABLE documents (id int, payload jsonb); INSERT INTO documents VALUES (1,'{}'),(2,NULL),(3,'{\"x\":1}')").unwrap();
    let result = session.query("SELECT d.id,e.key FROM documents d LEFT JOIN jsonb_each(d.payload) e ON true ORDER BY d.id", &[]).unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Null],
            vec![Value::Int4(2), Value::Null],
            vec![Value::Int4(3), Value::Text("x".into())]
        ]
    );
    for sql in [
        "SELECT jsonb_each('{}')",
        "SELECT 1 WHERE jsonb_array_elements('[1]') IS NULL",
        "SELECT json_build_array(jsonb_object_keys('{}'))",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported,
            "{sql}"
        );
    }
}

#[test]
fn validates_json_functions_without_rows_and_preserves_atomic_writes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE documents (id int, payload jsonb)")
        .unwrap();
    for sql in [
        "SELECT jsonb_array_length(1) FROM documents",
        "SELECT payload -> true FROM documents",
        "SELECT * FROM documents d, jsonb_each(d.id) e",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::UndefinedFunction,
            "{sql}"
        );
    }
    session
        .execute("INSERT INTO documents VALUES (1,'[]'),(2,'{}')")
        .unwrap();
    assert!(
        session
            .execute("UPDATE documents SET payload = jsonb_set(payload,'{0}','1')")
            .is_ok()
    );
    let before = session
        .query("SELECT * FROM documents ORDER BY id", &[])
        .unwrap();
    assert_eq!(
        session
            .execute(
                "UPDATE documents SET payload = jsonb_build_array(jsonb_array_length(payload))"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidParameterValue
    );
    assert_eq!(
        session
            .query("SELECT * FROM documents ORDER BY id", &[])
            .unwrap(),
        before
    );
}
