use pg_fake::{api::Db, error::SqlState, value::Value};

#[test]
fn preserves_set_operand_error_and_sequence_order() {
    for (query, next_value) in [
        (
            "SELECT nextval('set_effects') UNION ALL SELECT 'invalid'",
            1,
        ),
        (
            "SELECT nextval('set_effects') UNION ALL (SELECT 1 UNION ALL SELECT 'invalid')",
            2,
        ),
    ] {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE SEQUENCE set_effects").unwrap();
        assert_eq!(
            session.query(query, &[]).unwrap_err().sqlstate,
            SqlState::InvalidTextRepresentation
        );
        assert_eq!(
            session
                .query("SELECT nextval('set_effects')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(next_value)]]
        );
    }
}

#[test]
fn preserves_nested_set_operand_types_and_query_clauses() {
    let db = Db::create();
    let mut session = db.create_session();
    let query = session.prepare("(SELECT $1::BIGINT AS value UNION ALL SELECT 2 ORDER BY value DESC LIMIT 1) UNION ALL (SELECT 3::BIGINT AS value)").unwrap();
    let result = session.query_prepared(&query, &[Value::Int8(1)]).unwrap();
    assert_eq!(result.columns[0].name, "value");
    assert_eq!(
        result.rows,
        vec![vec![Value::Int8(2)], vec![Value::Int8(3)]]
    );
}
