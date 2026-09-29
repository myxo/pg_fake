use pg_fake::{Db, value::Value};

#[test]
fn memoizes_repeated_safe_lateral_queries_but_keeps_volatile_calls_per_row() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE lateral_parents (id integer, parent_id integer); \
             CREATE TABLE lateral_children (id integer, parent_id integer); \
             INSERT INTO lateral_parents VALUES (1, 10), (2, 10), (3, 20), (4, 99); \
             INSERT INTO lateral_children VALUES (1, 10), (2, 10), (3, 20), (4, 20)",
        )
        .unwrap();

    let result = session
        .query(
            "SELECT p.id, x.id \
             FROM lateral_parents p \
             LEFT JOIN LATERAL ( \
               SELECT c.id FROM lateral_children c \
               WHERE c.parent_id = p.parent_id AND c.id > $1 \
               ORDER BY c.id DESC LIMIT 1 \
             ) x ON true ORDER BY p.id",
            &[Value::Int4(1)],
        )
        .unwrap();

    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(2)],
            vec![Value::Int4(3), Value::Int4(4)],
            vec![Value::Int4(4), Value::Null],
        ]
    );

    let result = session
        .query(
            "SELECT p.id, x.id FROM lateral_parents p \
             LEFT JOIN LATERAL ( \
               SELECT c.id FROM lateral_children c WHERE c.parent_id = p.parent_id \
             ) x ON x.id = 1 ORDER BY p.id",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(2), Value::Int4(1)],
            vec![Value::Int4(3), Value::Null],
            vec![Value::Int4(4), Value::Null],
        ]
    );

    session.execute("CREATE SEQUENCE lateral_sequence").unwrap();
    let result = session
        .query(
            "SELECT p.id, x.value FROM lateral_parents p \
             CROSS JOIN LATERAL ( \
               SELECT nextval('lateral_sequence') AS value WHERE p.parent_id = 10 \
             ) x ORDER BY p.id",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Int8(1)],
            vec![Value::Int4(2), Value::Int8(2)],
        ]
    );
}

#[test]
fn keeps_distinct_keys_and_nested_correlations_separate() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE lateral_parents (id integer, parent_id integer); \
             CREATE TABLE lateral_children (id integer, parent_id integer); \
             CREATE TABLE lateral_allowed (child_id integer, parent_id integer); \
             INSERT INTO lateral_parents VALUES (1, 10), (2, 20), (3, 30); \
             INSERT INTO lateral_children VALUES (1, 10), (2, 10), (3, 20), (4, 30); \
             INSERT INTO lateral_allowed VALUES (2, 10), (3, 20), (4, 30)",
        )
        .unwrap();

    let result = session
        .query(
            "SELECT p.id, x.id FROM lateral_parents p \
             LEFT JOIN LATERAL ( \
               SELECT c.id FROM lateral_children c \
               WHERE c.parent_id = p.parent_id AND EXISTS ( \
                 SELECT 1 FROM lateral_allowed a \
                 WHERE a.child_id = c.id AND a.parent_id = p.parent_id \
               ) ORDER BY c.id DESC LIMIT 1 \
             ) x ON true ORDER BY p.id",
            &[],
        )
        .unwrap();

    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(1), Value::Int4(2)],
            vec![Value::Int4(2), Value::Int4(3)],
            vec![Value::Int4(3), Value::Int4(4)],
        ]
    );
}
