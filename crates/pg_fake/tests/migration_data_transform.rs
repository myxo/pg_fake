use pg_fake::{Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::Session, sql: &str) -> Vec<Vec<Value>> {
    session
        .query(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
        .rows
}

#[test]
fn executes_required_window_expressions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE window_values (id INTEGER, sort_key INTEGER, payload JSONB); \
             INSERT INTO window_values VALUES \
               (3, 1, '{\"a\": 1}'), (1, 1, '{\"a\": 1.0}'), \
               (2, NULL, NULL), (4, NULL, NULL)",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, row_number() OVER (ORDER BY id) FROM window_values ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int8(1)],
            vec![Value::Int4(2), Value::Int8(2)],
            vec![Value::Int4(3), Value::Int8(3)],
            vec![Value::Int4(4), Value::Int8(4)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, ntile(value) OVER (ORDER BY id) \
             FROM (VALUES (1, 1), (2, 2), (3, 3)) AS tiles(id, value) ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(2), Value::Int4(1)],
            vec![Value::Int4(3), Value::Int4(1)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT ntile('2') OVER (ORDER BY id) FROM window_values ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1)],
            vec![Value::Int4(1)],
            vec![Value::Int4(2)],
            vec![Value::Int4(2)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, count(*) OVER (PARTITION BY payload) FROM window_values ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int8(2)],
            vec![Value::Int4(2), Value::Int8(2)],
            vec![Value::Int4(3), Value::Int8(2)],
            vec![Value::Int4(4), Value::Int8(2)],
        ],
    );
    let metadata = session
        .query(
            "SELECT row_number() OVER (ORDER BY id), \
                    count(*) OVER (PARTITION BY payload) \
             FROM window_values LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(metadata.columns[0].type_oid, 20);
    assert_eq!(metadata.columns[1].type_oid, 20);
    assert_eq!(metadata.columns[0].name, "row_number");
    assert_eq!(metadata.columns[1].name, "count");
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, row_number() OVER (ORDER BY sort_key NULLS FIRST) \
             FROM window_values ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int8(4)],
            vec![Value::Int4(2), Value::Int8(1)],
            vec![Value::Int4(3), Value::Int8(3)],
            vec![Value::Int4(4), Value::Int8(2)],
        ],
    );
    assert!(
        query_rows(
            &mut session,
            "SELECT id, row_number() OVER (ORDER BY id), \
                    count(*) OVER (PARTITION BY payload) \
             FROM window_values WHERE false",
        )
        .is_empty()
    );
}

#[test]
fn executes_named_and_ranking_windows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE ranking_values (id INTEGER, category TEXT, value INTEGER); \
             INSERT INTO ranking_values VALUES \
               (1, 'a', 10), (2, 'a', 10), (3, 'a', 20), \
               (4, 'b', NULL), (5, 'b', 5)",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, row_number() OVER ordered, rank() OVER ordered, \
                    dense_rank() OVER ordered, percent_rank() OVER ordered, \
                    cume_dist() OVER ordered, ntile(2) OVER ordered \
             FROM ranking_values \
             WINDOW base AS (PARTITION BY category), \
                    ordered AS (base ORDER BY value NULLS FIRST) \
             ORDER BY id",
        ),
        vec![
            vec![
                Value::Int4(1),
                Value::Int8(1),
                Value::Int8(1),
                Value::Int8(1),
                Value::Float8(0.0),
                Value::Float8(2.0 / 3.0),
                Value::Int4(1),
            ],
            vec![
                Value::Int4(2),
                Value::Int8(2),
                Value::Int8(1),
                Value::Int8(1),
                Value::Float8(0.0),
                Value::Float8(2.0 / 3.0),
                Value::Int4(1),
            ],
            vec![
                Value::Int4(3),
                Value::Int8(3),
                Value::Int8(3),
                Value::Int8(2),
                Value::Float8(1.0),
                Value::Float8(1.0),
                Value::Int4(2),
            ],
            vec![
                Value::Int4(4),
                Value::Int8(1),
                Value::Int8(1),
                Value::Int8(1),
                Value::Float8(0.0),
                Value::Float8(0.5),
                Value::Int4(1),
            ],
            vec![
                Value::Int4(5),
                Value::Int8(2),
                Value::Int8(2),
                Value::Int8(2),
                Value::Float8(1.0),
                Value::Float8(1.0),
                Value::Int4(2),
            ],
        ],
    );
    let metadata = session
        .query(
            "SELECT ntile(2) OVER (), cume_dist() OVER () FROM ranking_values LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(metadata.columns[0].type_oid, 23);
    assert_eq!(metadata.columns[1].type_oid, 701);
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT rank() OVER () + rank() OVER () FROM ranking_values",
        ),
        vec![vec![Value::Int8(2)]; 5],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT category, rank() OVER (ORDER BY category) \
             FROM ranking_values GROUP BY category HAVING count(*) > 0 ORDER BY category",
        ),
        vec![
            vec![Value::Text("a".into()), Value::Int8(1)],
            vec![Value::Text("b".into()), Value::Int8(2)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT category, rank() OVER (ORDER BY count(*)) \
             FROM ranking_values GROUP BY category ORDER BY category",
        ),
        vec![
            vec![Value::Text("a".into()), Value::Int8(2)],
            vec![Value::Text("b".into()), Value::Int8(1)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT category, count(*) OVER (PARTITION BY count(*)) \
             FROM ranking_values GROUP BY category ORDER BY category",
        ),
        vec![
            vec![Value::Text("a".into()), Value::Int8(1)],
            vec![Value::Text("b".into()), Value::Int8(1)],
        ],
    );
    session.execute("CREATE SEQUENCE ranking_tiles").unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT ntile(nextval('ranking_tiles')::integer) OVER (), \
                    ntile(nextval('ranking_tiles')::integer) OVER () \
             FROM (VALUES (1), (2), (3)) AS tiles(value)",
        ),
        vec![
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(1), Value::Int4(1)],
            vec![Value::Int4(1), Value::Int4(2)],
        ],
    );
    for sql in [
        "SELECT rank() OVER missing FROM ranking_values",
        "SELECT rank() OVER child FROM ranking_values WINDOW base AS (PARTITION BY category), child AS (base PARTITION BY value)",
        "SELECT rank() OVER a FROM ranking_values WINDOW a AS (b), b AS (a)",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::WindowingError
        );
    }
    for sql in [
        "SELECT ntile(0) OVER () FROM ranking_values",
        "SELECT ntile(NULL) OVER () FROM ranking_values",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            if sql.contains("NULL") {
                SqlState::NullValueNotAllowed
            } else {
                SqlState::InvalidParameterValue
            }
        );
    }
    for sql in [
        "SELECT id FROM ranking_values WHERE rank() OVER () > 0",
        "SELECT rank() OVER (ORDER BY row_number() OVER ()) FROM ranking_values",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::WindowingError
        );
    }
}

#[test]
fn executes_offset_and_value_windows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE offset_values (id INTEGER, category TEXT, value INTEGER); \
             INSERT INTO offset_values VALUES \
               (1, 'a', 10), (2, 'a', 10), (3, 'a', 30), \
               (4, 'b', NULL), (5, 'b', 5)",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, lag(value, 1, -1) OVER ordered, lead(value, -1, -2) OVER ordered, \
                    first_value(value) OVER ordered, last_value(value) OVER ordered, \
                    nth_value(value, 2) OVER ordered \
             FROM offset_values \
             WINDOW ordered AS (PARTITION BY category ORDER BY value NULLS FIRST) \
             ORDER BY id",
        ),
        vec![
            vec![
                Value::Int4(1),
                Value::Int4(-1),
                Value::Int4(-2),
                Value::Int4(10),
                Value::Int4(10),
                Value::Int4(10)
            ],
            vec![
                Value::Int4(2),
                Value::Int4(10),
                Value::Int4(10),
                Value::Int4(10),
                Value::Int4(10),
                Value::Int4(10)
            ],
            vec![
                Value::Int4(3),
                Value::Int4(10),
                Value::Int4(10),
                Value::Int4(10),
                Value::Int4(30),
                Value::Int4(10)
            ],
            vec![
                Value::Int4(4),
                Value::Int4(-1),
                Value::Int4(-2),
                Value::Null,
                Value::Null,
                Value::Null
            ],
            vec![
                Value::Int4(5),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Int4(5),
                Value::Int4(5)
            ],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT first_value(value) OVER (PARTITION BY category), \
                    last_value(value) OVER (PARTITION BY category), \
                    nth_value(value, NULL) OVER (PARTITION BY category) \
             FROM offset_values ORDER BY id",
        ),
        vec![
            vec![Value::Int4(10), Value::Int4(30), Value::Null],
            vec![Value::Int4(10), Value::Int4(30), Value::Null],
            vec![Value::Int4(10), Value::Int4(30), Value::Null],
            vec![Value::Null, Value::Int4(5), Value::Null],
            vec![Value::Null, Value::Int4(5), Value::Null],
        ],
    );
    let prepared = session
        .prepare("SELECT lag(value, $1, $2) OVER (ORDER BY id) FROM offset_values ORDER BY id")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&prepared, &[Value::Int4(2), Value::Int4(-1)])
            .unwrap()
            .rows,
        vec![
            vec![Value::Int4(-1)],
            vec![Value::Int4(-1)],
            vec![Value::Int4(10)],
            vec![Value::Int4(10)],
            vec![Value::Int4(30)],
        ],
    );
    let metadata = session
        .query(
            "SELECT lead(value, 1, 1::bigint) OVER (ORDER BY id), \
                    nth_value(value, 1) OVER (ORDER BY id) \
             FROM offset_values LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        metadata
            .columns
            .iter()
            .map(|column| column.type_oid)
            .collect::<Vec<_>>(),
        vec![20, 23],
    );
    for sql in [
        "SELECT lag(value, 1, 1, 1) OVER () FROM offset_values",
        "SELECT first_value(value, 1) OVER () FROM offset_values",
        "SELECT nth_value(value) OVER () FROM offset_values",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::UndefinedFunction,
        );
    }
    assert_eq!(
        session
            .query("SELECT nth_value(value, 0) OVER () FROM offset_values", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::ArraySubscriptError,
    );
}

#[test]
fn executes_required_aggregates_and_predicates() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE aggregate_values (id INTEGER, label TEXT); \
             INSERT INTO aggregate_values VALUES (2, ' b '), (1, 'a'), (3, NULL)",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*), max(id), string_agg(btrim(label), ',' ORDER BY id) FROM aggregate_values",
        ),
        vec![vec![
            Value::Int8(3),
            Value::Int4(3),
            Value::Text("a,b".into()),
        ]],
    );
    let metadata = session
        .query(
            "SELECT count(*), max(id), string_agg(label, ',' ORDER BY id), btrim(' x ') \
             FROM aggregate_values",
            &[],
        )
        .unwrap();
    assert_eq!(
        metadata
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.type_oid))
            .collect::<Vec<_>>(),
        vec![
            ("count", 20),
            ("max", 23),
            ("string_agg", 25),
            ("btrim", 25),
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT NULL IS DISTINCT FROM NULL, NULL IS NOT DISTINCT FROM NULL, \
                    2 IN (1, 2, NULL), EXISTS (SELECT 1 WHERE false), \
                    NOT EXISTS (SELECT 1 WHERE false), 'ABC-12' ~ '^[A-Z]{3}-([0-9]{2})$'",
        ),
        vec![vec![
            Value::Bool(false),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(true),
            Value::Bool(true),
        ]],
    );
    let metadata = session
        .query(
            "SELECT extract(epoch FROM '1970-01-01 00:00:05.6+00'::timestamptz), \
                    CURRENT_TIMESTAMP + INTERVAL '7 days'",
            &[],
        )
        .unwrap();
    assert_eq!(metadata.columns[0].type_oid, 1700);
    assert_eq!(metadata.columns[1].type_oid, 1184);
    assert_eq!(
        session
            .query("SELECT 'x' ~ '[unterminated'", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidRegularExpression,
    );
    assert_eq!(
        session
            .query("SELECT 'x' ~ 'x{256}'", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidRegularExpression,
    );
    assert_eq!(
        session.query("SELECT 1 ~ '1'", &[]).unwrap_err().sqlstate,
        SqlState::UndefinedFunction,
    );
    assert_eq!(
        session.query("SELECT count()", &[]).unwrap_err().sqlstate,
        SqlState::WrongObjectType,
    );
    assert_eq!(
        session
            .query(
                "SELECT count() OVER (PARTITION BY id) FROM aggregate_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::WrongObjectType,
    );
    assert_eq!(
        session
            .query(
                "SELECT string_agg(DISTINCT label, ',' ORDER BY label) FROM aggregate_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported,
    );
}

#[test]
fn executes_required_scalar_and_dml_composition() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source_values (id INTEGER, amount TEXT, payload JSONB); \
             CREATE TABLE destination_values (id INTEGER PRIMARY KEY, amount BIGINT); \
             INSERT INTO source_values VALUES \
               (2, ' 12 ', '{\"kind\":\"amount\"}'), \
               (1, '7', '{\"kind\":\"amount\"}')",
        )
        .unwrap();

    session
        .execute(
            "WITH selected AS MATERIALIZED ( \
                 SELECT id, btrim(amount)::numeric::bigint AS amount \
                 FROM source_values \
                 WHERE jsonb_typeof(payload) = 'object' \
             ) \
             INSERT INTO destination_values \
             SELECT id, amount FROM selected ORDER BY id \
             ON CONFLICT (id) DO NOTHING",
        )
        .unwrap();
    session
        .execute(
            "UPDATE destination_values AS destination \
             SET amount = source.amount * 2 \
             FROM (SELECT id, amount FROM destination_values) AS source \
             WHERE destination.id = source.id",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, \
                    CASE WHEN amount > 20 THEN amount ELSE coalesce(NULL::bigint, amount + 1) END, \
                    (SELECT max(candidate.amount) FROM destination_values candidate \
                     WHERE candidate.id <= destination.id LIMIT 1) \
             FROM destination_values destination ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int8(15), Value::Int8(14)],
            vec![Value::Int4(2), Value::Int8(24), Value::Int8(24)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT extract(epoch FROM '1970-01-01 00:00:05.6+00'::timestamptz), \
                    extract(epoch FROM '1969-12-31 23:59:59.6+00'::timestamptz), \
                    extract(epoch FROM '1970-01-01 00:00:05.6+00'::timestamptz)::bigint, \
                    (CURRENT_TIMESTAMP + INTERVAL '7 days') = \
                    (CURRENT_TIMESTAMP + INTERVAL '168 hours')",
        ),
        vec![vec![
            Value::Numeric("5.6".parse().unwrap()),
            Value::Numeric("-0.4".parse().unwrap()),
            Value::Int8(6),
            Value::Bool(true),
        ]],
    );
    assert_eq!(
        session
            .query("SELECT INTERVAL '2562047789:00:00'", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::IntervalFieldOverflow,
    );
    assert_eq!(
        session
            .query(
                "SELECT '2000-01-01 00:00:00'::timestamp - INTERVAL '-2147483648 days'",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DatetimeFieldOverflow,
    );
}
