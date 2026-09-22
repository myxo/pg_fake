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
        SqlState::InvalidArgumentForNthValue,
    );
    for sql in [
        "SELECT lag(value) IGNORE NULLS OVER () FROM offset_values",
        "SELECT lag(value) RESPECT NULLS OVER () FROM offset_values",
        "SELECT nth_value(value, 1) FROM FIRST OVER () FROM offset_values",
        "SELECT nth_value(value, 1) FROM LAST OVER () FROM offset_values",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::SyntaxError,
        );
    }
    for (sql, sqlstate) in [
        (
            "SELECT lag(value, NULL, 'x') OVER () FROM offset_values",
            SqlState::InvalidTextRepresentation,
        ),
        (
            "SELECT lag(value, 0, 'x') OVER () FROM offset_values",
            SqlState::InvalidTextRepresentation,
        ),
        (
            "SELECT lag(value, 1, 'x') OVER () FROM offset_values WHERE false",
            SqlState::InvalidTextRepresentation,
        ),
        (
            "SELECT nth_value(value, '2147483648') OVER () FROM offset_values WHERE false",
            SqlState::NumericValueOutOfRange,
        ),
        (
            "SELECT lag(value, 1, 'x'::integer) OVER () FROM offset_values WHERE false",
            SqlState::InvalidTextRepresentation,
        ),
        (
            "SELECT nth_value(value, '2147483648'::integer) OVER () FROM offset_values WHERE false",
            SqlState::NumericValueOutOfRange,
        ),
    ] {
        assert_eq!(session.query(sql, &[]).unwrap_err().sqlstate, sqlstate);
    }

    session
        .execute(
            "CREATE TABLE offset_times (id INTEGER, value TIMESTAMPTZ); \
             INSERT INTO offset_times VALUES (1, '2024-01-01 00:00:00+00'); \
             SET TIME ZONE 'UTC'",
        )
        .unwrap();
    let prepared = session
        .prepare(
            "SELECT lag(value, 1, '2020-01-01 00:00') OVER (ORDER BY id) \
             FROM offset_times",
        )
        .unwrap();
    let expected =
        query_rows(&mut session, "SELECT '2020-01-01 00:00:00+00'::timestamptz")[0][0].clone();
    session.execute("SET TIME ZONE '+03:00'").unwrap();
    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows,
        vec![vec![expected]],
    );
}

#[test]
fn executes_aggregate_window_frames() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE frame_values (id INTEGER, category TEXT, value INTEGER, flag BOOLEAN, label TEXT); \
             INSERT INTO frame_values VALUES \
               (1, 'a', 10, true, 'x'), (2, 'a', 10, false, 'y'), \
               (3, 'a', 30, true, 'z'), (4, 'a', NULL, NULL, NULL), \
               (5, 'b', 5, true, 'q')",
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, count(*) OVER (ORDER BY id RANGE BETWEEN 2147483647 FOLLOWING AND UNBOUNDED FOLLOWING) \
             FROM frame_values ORDER BY id",
        ),
        vec![
            vec![Value::Int4(1), Value::Int8(0)],
            vec![Value::Int4(2), Value::Int8(0)],
            vec![Value::Int4(3), Value::Int8(0)],
            vec![Value::Int4(4), Value::Int8(0)],
            vec![Value::Int4(5), Value::Int8(0)],
        ],
    );
    session
        .execute("CREATE SEQUENCE frame_offset_sequence")
        .unwrap();
    let _ = query_rows(
        &mut session,
        "SELECT count(*) OVER (ORDER BY id ROWS nextval('frame_offset_sequence') PRECEDING) \
         FROM frame_values",
    );
    assert_eq!(
        query_rows(&mut session, "SELECT currval('frame_offset_sequence')"),
        vec![vec![Value::Int8(1)]],
    );

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, sum(value) OVER ordered, \
                    sum(value) OVER (PARTITION BY category ORDER BY value NULLS LAST \
                      ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING), \
                    sum(value) OVER (PARTITION BY category ORDER BY value NULLS LAST \
                      RANGE BETWEEN 5 PRECEDING AND 5 FOLLOWING), \
                    sum(value) OVER (PARTITION BY category ORDER BY value NULLS LAST \
                      GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) \
             FROM frame_values \
             WINDOW ordered AS (PARTITION BY category ORDER BY value NULLS LAST) \
             ORDER BY id",
        ),
        vec![
            vec![
                Value::Int4(1),
                Value::Int8(20),
                Value::Int8(20),
                Value::Int8(20),
                Value::Int8(20)
            ],
            vec![
                Value::Int4(2),
                Value::Int8(20),
                Value::Int8(50),
                Value::Int8(20),
                Value::Int8(20)
            ],
            vec![
                Value::Int4(3),
                Value::Int8(50),
                Value::Int8(40),
                Value::Int8(30),
                Value::Int8(50)
            ],
            vec![
                Value::Int4(4),
                Value::Int8(50),
                Value::Int8(30),
                Value::Null,
                Value::Int8(30)
            ],
            vec![
                Value::Int4(5),
                Value::Int8(5),
                Value::Int8(5),
                Value::Int8(5),
                Value::Int8(5)
            ],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, count(*) OVER whole, count(value) OVER whole, \
                    min(value) OVER whole, max(value) OVER whole, \
                    bool_and(flag) OVER whole, bool_or(flag) OVER whole, \
                    string_agg(label, ',') OVER whole, \
                    sum(value) FILTER (WHERE flag) OVER whole \
             FROM frame_values \
             WINDOW whole AS (PARTITION BY category) ORDER BY id",
        ),
        vec![
            vec![
                Value::Int4(1),
                Value::Int8(4),
                Value::Int8(3),
                Value::Int4(10),
                Value::Int4(30),
                Value::Bool(false),
                Value::Bool(true),
                Value::Text("x,y,z".into()),
                Value::Int8(40)
            ],
            vec![
                Value::Int4(2),
                Value::Int8(4),
                Value::Int8(3),
                Value::Int4(10),
                Value::Int4(30),
                Value::Bool(false),
                Value::Bool(true),
                Value::Text("x,y,z".into()),
                Value::Int8(40)
            ],
            vec![
                Value::Int4(3),
                Value::Int8(4),
                Value::Int8(3),
                Value::Int4(10),
                Value::Int4(30),
                Value::Bool(false),
                Value::Bool(true),
                Value::Text("x,y,z".into()),
                Value::Int8(40)
            ],
            vec![
                Value::Int4(4),
                Value::Int8(4),
                Value::Int8(3),
                Value::Int4(10),
                Value::Int4(30),
                Value::Bool(false),
                Value::Bool(true),
                Value::Text("x,y,z".into()),
                Value::Int8(40)
            ],
            vec![
                Value::Int4(5),
                Value::Int8(1),
                Value::Int8(1),
                Value::Int4(5),
                Value::Int4(5),
                Value::Bool(true),
                Value::Bool(true),
                Value::Text("q".into()),
                Value::Int8(5)
            ],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, first_value(value) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND 1 FOLLOWING), \
                    last_value(value) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND 1 FOLLOWING), \
                    nth_value(value, 2) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND 1 FOLLOWING) \
             FROM frame_values ORDER BY id",
        ),
        vec![
            vec![
                Value::Int4(1),
                Value::Int4(10),
                Value::Int4(10),
                Value::Int4(10)
            ],
            vec![
                Value::Int4(2),
                Value::Int4(10),
                Value::Int4(30),
                Value::Int4(30)
            ],
            vec![Value::Int4(3), Value::Int4(30), Value::Null, Value::Null],
            vec![Value::Int4(4), Value::Null, Value::Int4(5), Value::Int4(5)],
            vec![Value::Int4(5), Value::Int4(5), Value::Int4(5), Value::Null],
        ],
    );
    let prepared = session
        .prepare(
            "SELECT sum(value) OVER (ORDER BY id ROWS BETWEEN $1 PRECEDING AND $2 FOLLOWING) \
             FROM frame_values ORDER BY id",
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&prepared, &[Value::Int8(1), Value::Int8(0)])
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(10)],
            vec![Value::Int8(20)],
            vec![Value::Int8(40)],
            vec![Value::Int8(30)],
            vec![Value::Int8(5)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT category, sum(count(*)) OVER (ORDER BY category) \
             FROM frame_values GROUP BY category HAVING count(*) > 0 ORDER BY category",
        ),
        vec![
            vec![Value::Text("a".into()), Value::Numeric(4.into())],
            vec![Value::Text("b".into()), Value::Numeric(5.into())],
        ],
    );

    for sql in [
        "SELECT sum(value) OVER (ROWS UNBOUNDED FOLLOWING) FROM frame_values",
        "SELECT row_number() OVER (ROWS UNBOUNDED FOLLOWING) FROM frame_values",
        "SELECT lag(value) OVER (ROWS UNBOUNDED FOLLOWING) FROM frame_values",
        "SELECT first_value(value) OVER (ROWS UNBOUNDED FOLLOWING) FROM frame_values",
        "SELECT sum(value) OVER (ROWS BETWEEN CURRENT ROW AND 1 PRECEDING) FROM frame_values",
        "SELECT sum(value) OVER (RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM frame_values",
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            SqlState::WindowingError,
        );
    }
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ORDER BY id ROWS -1 PRECEDING) FROM frame_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ORDER BY id ROWS id PRECEDING) FROM frame_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidColumnReference,
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ORDER BY id ROWS count(*) PRECEDING) FROM frame_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::GroupingError,
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ORDER BY id ROWS row_number() OVER () PRECEDING) FROM frame_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::WindowingError,
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ORDER BY id::double precision RANGE CAST('NaN' AS double precision) PRECEDING) FROM frame_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY x RANGE CAST('Infinity' AS float8) PRECEDING) \
             FROM (VALUES (1::float8), (2), ('Infinity')) AS values(x)",
        ),
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(3)],
        ],
    );
    assert_eq!(
        session
            .query(
                "SELECT count(*) OVER (ORDER BY id::float8 RANGE CAST('-Infinity' AS float8) PRECEDING) FROM frame_values",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ROWS -1 PRECEDING) FROM frame_values WHERE false",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ORDER BY id GROUPS NULL PRECEDING) FROM frame_values WHERE false",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::NullValueNotAllowed,
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(value) OVER (ROWS ('-1') PRECEDING) FROM frame_values WHERE false",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    assert!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY id RANGE -1 PRECEDING) FROM frame_values WHERE false",
        )
        .is_empty()
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY value RANGE -1 PRECEDING) \
             FROM (VALUES (NULL::integer)) AS values(value)",
        ),
        vec![vec![Value::Int8(1)]],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY id ROWS '1' PRECEDING), \
                    count(*) OVER (ORDER BY id RANGE '1' PRECEDING) \
             FROM frame_values ORDER BY id",
        ),
        vec![
            vec![Value::Int8(1), Value::Int8(1)],
            vec![Value::Int8(2), Value::Int8(2)],
            vec![Value::Int8(2), Value::Int8(2)],
            vec![Value::Int8(2), Value::Int8(2)],
            vec![Value::Int8(2), Value::Int8(2)],
        ],
    );
}

#[test]
fn executes_temporal_range_frames() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE temporal_frames (id INTEGER, d DATE, tm TIME, ts TIMESTAMP, tz TIMESTAMPTZ, iv INTERVAL); \
             INSERT INTO temporal_frames VALUES \
               (1, '2024-01-01', '00:00', '2024-01-01 00:00', '2024-01-01 00:00:00+00', INTERVAL '1 hour'), \
               (2, '2024-01-02', '00:10', '2024-01-01 00:30', '2024-01-01 00:30:00+00', INTERVAL '90 minutes'), \
               (3, '2024-02-01', '23:50', '2024-02-01 00:00', '2024-02-01 00:00:00+00', INTERVAL '3 hours')",
        )
        .unwrap();

    for sql in [
        "SELECT sum(id) OVER (ORDER BY d RANGE INTERVAL '1 day' PRECEDING) FROM temporal_frames ORDER BY id",
        "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '30 minutes' PRECEDING) FROM temporal_frames ORDER BY id",
        "SELECT sum(id) OVER (ORDER BY ts RANGE INTERVAL '1 hour' PRECEDING) FROM temporal_frames ORDER BY id",
        "SELECT sum(id) OVER (ORDER BY tz RANGE INTERVAL '1 hour' PRECEDING) FROM temporal_frames ORDER BY id",
        "SELECT sum(id) OVER (ORDER BY iv RANGE INTERVAL '1 hour' PRECEDING) FROM temporal_frames ORDER BY id",
        "SELECT sum(id) OVER (ORDER BY ts RANGE INTERVAL '1 month -1 day' PRECEDING) FROM temporal_frames ORDER BY id",
        "SELECT sum(id) OVER (ORDER BY ts RANGE INTERVAL '-1 month 40 days' PRECEDING) FROM temporal_frames ORDER BY id",
    ] {
        let _ = query_rows(&mut session, sql);
    }
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '30 minutes' PRECEDING) \
             FROM temporal_frames ORDER BY tm",
        ),
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(2)],
            vec![Value::Int8(1)],
        ],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '1 day' PRECEDING) \
             FROM (VALUES ('01:00'::time), ('02:00')) AS values(tm)",
        ),
        vec![vec![Value::Int8(1)], vec![Value::Int8(1)]],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '-1 day 1 hour' PRECEDING) \
             FROM (VALUES ('01:00'::time), ('02:00')) AS values(tm)",
        ),
        vec![vec![Value::Int8(1)], vec![Value::Int8(2)]],
    );
    assert_eq!(
        session
            .query(
                "SELECT count(*) OVER (ORDER BY tm RANGE INTERVAL '1 day -1 hour' PRECEDING) \
                 FROM (VALUES ('01:00'::time), ('02:00')) AS values(tm)",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    assert_eq!(
        session
            .query(
                "SELECT count(*) OVER (ORDER BY ts RANGE INTERVAL '-1 day' PRECEDING) \
                 FROM (VALUES ('infinity'::timestamp)) AS values(ts)",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY d RANGE BETWEEN CURRENT ROW AND INTERVAL '1 day' FOLLOWING) \
             FROM (VALUES ('2024-01-01'::date), ('infinity'::date)) AS values(d)",
        ),
        vec![vec![Value::Int8(1)], vec![Value::Int8(1)]],
    );
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT count(*) OVER (ORDER BY ts RANGE '1 day' PRECEDING) \
             FROM (VALUES ('2024-01-01'::timestamp), ('2024-01-02'::timestamp)) AS values(ts)",
        ),
        vec![vec![Value::Int8(1)], vec![Value::Int8(2)]],
    );
    assert_eq!(
        session
            .query(
                "SELECT count(*) OVER (ORDER BY ts RANGE '-1 day' PRECEDING) \
                 FROM (VALUES ('infinity'::timestamp)) AS values(ts)",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidPrecedingOrFollowingSize,
    );
    let prepared = session
        .prepare(
            "SELECT sum(id) OVER (ORDER BY d RANGE $1 PRECEDING) \
             FROM temporal_frames ORDER BY id",
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(
                &prepared,
                &[Value::Interval(pg_fake::value::PgInterval {
                    months: 0,
                    days: 1,
                    micros: 0,
                })],
            )
            .unwrap()
            .rows,
        vec![
            vec![Value::Int8(1)],
            vec![Value::Int8(3)],
            vec![Value::Int8(3)],
        ],
    );
    assert_eq!(
        session
            .query(
                "SELECT sum(id) OVER (ORDER BY id ROWS (SELECT id) PRECEDING) FROM temporal_frames",
                &[],
            )
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidColumnReference,
    );
    let _ = query_rows(
        &mut session,
        "SELECT sum(id) OVER (ORDER BY id ROWS (SELECT 1) PRECEDING) FROM temporal_frames",
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
