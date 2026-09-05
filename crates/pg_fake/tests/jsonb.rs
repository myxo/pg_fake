use pg_fake::{
    api::Db,
    error::SqlState,
    jsonb::Jsonb,
    value::{BaseType, Value},
};

#[test]
fn normalizes_jsonb_and_preserves_numeric_equality_in_hash_keys() {
    use std::collections::HashSet;

    let left = Jsonb::parse(r#"{"long":1,"a":0,"a":1.00}"#).unwrap();
    let right = Jsonb::parse(r#"{"a":1e0,"long":1.0}"#).unwrap();
    assert_eq!(left.get_postgres_text(), r#"{"a": 1.00, "long": 1}"#);
    assert_eq!(left, right);
    assert!(HashSet::from([left]).contains(&right));
    assert_eq!(BaseType::Jsonb.map_to_oid(), 3802);
    assert_eq!(BaseType::resolve_oid(3802), Some(BaseType::Jsonb));

    let nested = format!("{}1.00{}", "[".repeat(512), "]".repeat(512));
    assert_eq!(Jsonb::parse(&nested).unwrap().get_postgres_text(), nested);
    for document in [
        r#"{"$serde_json::private::Number":null}"#,
        r#"{"$serde_json::private::Number":"not a number"}"#,
    ] {
        let value = Jsonb::parse(document).unwrap();
        assert!(value.get_postgres_text().starts_with('{'));
    }
}

#[test]
fn executes_jsonb_parameters_constraints_and_rollback() {
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE documents (id INT PRIMARY KEY, payload JSONB NOT NULL UNIQUE, fallback JSONB DEFAULT '{}')").unwrap();
    let payload =
        Value::Jsonb(Jsonb::parse(r#"{"amount":{"value":"12.50","currency":"USD"}}"#).unwrap());
    let prepared = session
        .prepare("INSERT INTO documents (id, payload) VALUES ($1, $2) RETURNING payload, fallback")
        .unwrap();
    let rows = session
        .query_prepared(&prepared, &[Value::Int4(1), payload.clone()])
        .unwrap();
    assert_eq!(rows.columns[0].type_oid, 3802);
    assert_eq!(rows.rows[0][0], payload);
    assert_eq!(rows.rows[0][1], Value::Jsonb(Jsonb::parse("{}").unwrap()));
    session
        .execute("INSERT INTO documents (id, payload) VALUES (2, '1.00')")
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO documents (id, payload) VALUES (3, '1e0')")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    assert_eq!(
        session
            .execute("INSERT INTO documents (id, payload) VALUES (3, NULL)")
            .unwrap_err()
            .sqlstate,
        SqlState::NotNullViolation
    );
    assert_eq!(
        session
            .execute("INSERT INTO documents (id, payload) VALUES (3, '2'), (4, '[1,]')")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
    session
        .execute("BEGIN; UPDATE documents SET payload = 'false' WHERE id = 1; ROLLBACK")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT payload FROM documents WHERE id = 1", &[])
            .unwrap()
            .rows,
        vec![vec![payload]]
    );
}
