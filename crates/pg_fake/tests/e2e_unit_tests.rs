use std::{
    env,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use pg_fake::{
    api::Db,
    parser::{self, Statement},
    value::Value,
};
use postgres::{Client, NoTls, SimpleQueryMessage};
use testcontainers::{ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Affected(u64),
    Rows(Vec<Vec<Option<String>>>),
    Error(String),
}

#[derive(Clone, Copy)]
enum RowOrder {
    Unordered,
    Ordered,
}

#[derive(Clone, Copy)]
enum SessionName {
    First,
    Second,
}

static TABLE_NUMBER: AtomicU64 = AtomicU64::new(1);
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn assert_differential(script: &str, row_order: RowOrder) {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let configured_url = env::var("PG_FAKE_TEST_DATABASE_URL").ok();
    if configured_url.is_none() && env::var_os("DOCKER_HOST").is_none() {
        let socket = PathBuf::from(env::var_os("HOME").expect("HOME must be set"))
            .join(".colima/default/docker.sock");
        if socket.exists() {
            // The test mutex serializes all environment access in this process.
            unsafe { env::set_var("DOCKER_HOST", format!("unix://{}", socket.display())) };
        }
    }

    let table_name = format!(
        "pg_fake_differential_{}_{}",
        std::process::id(),
        TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
    );
    let script = script.replace("__TABLE__", &table_name);
    let container = configured_url.is_none().then(|| {
        Postgres::default()
            .with_tag("18")
            .start()
            .expect("must start PostgreSQL 18 container")
    });
    let url = configured_url.unwrap_or_else(|| {
        let container = container.as_ref().expect("container must be started");
        format!(
            "postgresql://postgres:postgres@{}:{}/postgres",
            container
                .get_host()
                .expect("container host must be available"),
            container
                .get_host_port_ipv4(5432)
                .expect("PostgreSQL port must be available")
        )
    });
    let mut postgres = Client::connect(&url, NoTls).expect("must connect to PostgreSQL");
    let db = Db::new();
    let mut fake = db.session();

    for statement in parser::parse(&script).unwrap() {
        let sql = statement.to_string();
        let expected = postgres_outcome(&mut postgres, &statement, &sql);
        let actual = fake_outcome(&mut fake, &statement, &sql);
        match (expected, actual) {
            (Outcome::Rows(mut expected), Outcome::Rows(mut actual)) => {
                if matches!(row_order, RowOrder::Unordered) {
                    expected.sort();
                    actual.sort();
                }
                assert_eq!(actual, expected, "{sql}");
            }
            (expected, actual) => assert_eq!(actual, expected, "{sql}"),
        }
    }
}

fn assert_session_differential(operations: &[(SessionName, &str)], row_order: RowOrder) {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let configured_url = env::var("PG_FAKE_TEST_DATABASE_URL").ok();
    if configured_url.is_none() && env::var_os("DOCKER_HOST").is_none() {
        let socket = PathBuf::from(env::var_os("HOME").expect("HOME must be set"))
            .join(".colima/default/docker.sock");
        if socket.exists() {
            unsafe { env::set_var("DOCKER_HOST", format!("unix://{}", socket.display())) };
        }
    }

    let table_name = format!(
        "pg_fake_differential_{}_{}",
        std::process::id(),
        TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
    );
    let operations = operations
        .iter()
        .map(|(session, sql)| {
            let sql = sql.replace("__TABLE__", &table_name);
            let mut statements = parser::parse(&sql).unwrap();
            assert_eq!(statements.len(), 1, "operation must contain one statement");
            (*session, statements.pop().unwrap(), sql)
        })
        .collect::<Vec<_>>();
    let container = configured_url.is_none().then(|| {
        Postgres::default()
            .with_tag("18")
            .start()
            .expect("must start PostgreSQL 18 container")
    });
    let url = configured_url.unwrap_or_else(|| {
        let container = container.as_ref().expect("container must be started");
        format!(
            "postgresql://postgres:postgres@{}:{}/postgres",
            container
                .get_host()
                .expect("container host must be available"),
            container
                .get_host_port_ipv4(5432)
                .expect("PostgreSQL port must be available")
        )
    });
    let mut postgres_first = Client::connect(&url, NoTls).expect("must connect to PostgreSQL");
    let mut postgres_second = Client::connect(&url, NoTls).expect("must connect to PostgreSQL");
    let db = Db::new();
    let mut fake_first = db.session();
    let mut fake_second = db.session();

    for (session, statement, sql) in operations {
        let (postgres, fake) = match session {
            SessionName::First => (&mut postgres_first, &mut fake_first),
            SessionName::Second => (&mut postgres_second, &mut fake_second),
        };
        let expected = postgres_outcome(postgres, &statement, &sql);
        let actual = fake_outcome(fake, &statement, &sql);
        match (expected, actual) {
            (Outcome::Rows(mut expected), Outcome::Rows(mut actual)) => {
                if matches!(row_order, RowOrder::Unordered) {
                    expected.sort();
                    actual.sort();
                }
                assert_eq!(actual, expected, "{sql}");
            }
            (expected, actual) => assert_eq!(actual, expected, "{sql}"),
        }
    }
}

fn postgres_outcome(client: &mut Client, statement: &Statement, sql: &str) -> Outcome {
    match client.simple_query(sql) {
        Ok(messages) => match statement {
            Statement::Query(_) => Outcome::Rows(
                messages
                    .iter()
                    .filter_map(|message| match message {
                        SimpleQueryMessage::Row(row) => Some(
                            (0..row.len())
                                .map(|index| row.get(index).map(str::to_owned))
                                .collect(),
                        ),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => Outcome::Affected(
                messages
                    .iter()
                    .filter_map(|message| match message {
                        SimpleQueryMessage::CommandComplete(rows) => Some(*rows),
                        _ => None,
                    })
                    .last()
                    .expect("non-query statements must complete"),
            ),
        },
        Err(error) => Outcome::Error(
            error
                .code()
                .expect("PostgreSQL execution errors must have a SQLSTATE")
                .code()
                .into(),
        ),
    }
}

fn fake_outcome(session: &mut pg_fake::api::Session, statement: &Statement, sql: &str) -> Outcome {
    match statement {
        Statement::Query(_) => match session.query(sql, &[]) {
            Ok(result) => Outcome::Rows(
                result
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|value| match value {
                                Value::Null => None,
                                value => Some(value.to_text()),
                            })
                            .collect()
                    })
                    .collect(),
            ),
            Err(error) => Outcome::Error(error.sqlstate.code().into()),
        },
        _ => match session.execute(sql) {
            Ok(rows) => Outcome::Affected(rows),
            Err(error) => Outcome::Error(error.sqlstate.code().into()),
        },
    }
}

#[test]
fn explicit_transactions_match_postgres_across_sessions() {
    assert_session_differential(
        &[
            (
                SessionName::First,
                "CREATE TABLE __TABLE__ (id INTEGER, amount INTEGER)",
            ),
            (SessionName::First, "INSERT INTO __TABLE__ VALUES (1, 1)"),
            (SessionName::First, "BEGIN"),
            (
                SessionName::First,
                "UPDATE __TABLE__ SET amount = amount + 1 WHERE id = 1",
            ),
            (SessionName::First, "SELECT * FROM __TABLE__"),
            (SessionName::Second, "SELECT * FROM __TABLE__"),
            (SessionName::First, "COMMIT"),
            (SessionName::Second, "SELECT * FROM __TABLE__"),
            (SessionName::First, "BEGIN"),
            (SessionName::First, "INSERT INTO __TABLE__ VALUES (2, 2)"),
            (SessionName::First, "ROLLBACK"),
            (SessionName::Second, "SELECT * FROM __TABLE__"),
            (SessionName::First, "BEGIN"),
            (SessionName::First, "INSERT INTO missing VALUES (1)"),
            (SessionName::First, "SELECT * FROM __TABLE__"),
            (SessionName::First, "ROLLBACK"),
            (SessionName::First, "SELECT * FROM __TABLE__"),
        ],
        RowOrder::Unordered,
    );
}

#[test]
fn create_insert_and_select_star() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, name TEXT);\
         INSERT INTO __TABLE__ VALUES (2, 'second'), (1, 'first');\
         SELECT * FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn projects_columns_and_nulls() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, name TEXT);\
         INSERT INTO __TABLE__ VALUES (1, NULL), (2, 'two');\
         SELECT name, id FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn compares_rows_in_order_when_requested() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, name TEXT);\
         INSERT INTO __TABLE__ VALUES (1, 'first'), (2, 'second');\
         SELECT * FROM __TABLE__",
        RowOrder::Ordered,
    );
}

#[test]
fn evaluates_arithmetic_and_comparison_projections() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, amount INTEGER, name TEXT, price NUMERIC);\
         INSERT INTO __TABLE__ VALUES (7, 3, 'seven', 2.5);\
         SELECT id + amount, id - amount, id * amount, id / amount, id % amount, id > amount, name = 'seven', price * 2.0 FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn evaluates_case_and_common_scalar_functions() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             id INTEGER,
             score INTEGER,
             label TEXT,
             delta INTEGER,
             amount NUMERIC
         ); \
         INSERT INTO __TABLE__ VALUES
             (1, 7, 'MiXeD', 3, 2.5),
             (2, 0, NULL, NULL, NULL),
             (3, NULL, 'third', 4, 1.5); \
         SELECT
             CASE
                 WHEN score > 5 THEN 'high'
                 WHEN score IS NULL THEN 'missing'
                 ELSE 'low'
             END,
             CASE id
                 WHEN 1 THEN 'one'
                 WHEN 2 THEN NULL
                 ELSE 'other'
             END,
             CASE WHEN score > 100 THEN score END,
             COALESCE(label, 'fallback'),
             NULLIF(score, 0),
             GREATEST(score, 5),
             LEAST(score, 5),
             length(label),
             lower(label),
             upper(label),
             abs(-delta),
             abs(-amount)
         FROM __TABLE__; \
         SELECT
             CASE WHEN id = 1 THEN 10 ELSE 1 / (id - 1) END,
             COALESCE(score, 1 / (score - 7))
         FROM __TABLE__
         WHERE id = 1",
        RowOrder::Ordered,
    );
}

#[test]
fn case_and_functions_match_postgres_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (1); \
         SELECT CASE WHEN id = 1 THEN id ELSE TRUE END FROM __TABLE__; \
         SELECT unknown_function(id) FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn coerces_phase_one_types_in_all_cast_contexts() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             small_value SMALLINT,
             int_value INTEGER,
             big_value BIGINT,
             numeric_value NUMERIC,
             real_value REAL,
             double_value DOUBLE PRECISION,
             short_label VARCHAR(4)
         ); \
         INSERT INTO __TABLE__ VALUES (1, 2, 3, 4, 5, 6, 'abcd'); \
         SELECT
             small_value + int_value,
             int_value + big_value,
             big_value + numeric_value,
             numeric_value + real_value,
             real_value + double_value,
             int_value = '2',
             CASE WHEN TRUE THEN int_value ELSE numeric_value END,
             COALESCE(NULL, int_value, numeric_value)
         FROM __TABLE__; \
         SELECT
             CAST('42' AS INTEGER),
             '3.5'::NUMERIC,
             CAST(2.6 AS INTEGER),
             CAST(1 AS TEXT),
             CAST(TRUE AS TEXT),
             1::BOOLEAN,
             TRUE::INTEGER,
             258::BYTEA,
             '\\x00000102'::BYTEA::INTEGER,
             CAST('abcdef' AS VARCHAR(3)),
             CAST(12.36 AS NUMERIC(4, 1))
         FROM __TABLE__; \
         UPDATE __TABLE__
         SET small_value = int_value, int_value = 2.6; \
         UPDATE __TABLE__ SET int_value = '7'; \
         SELECT small_value, int_value FROM __TABLE__",
        RowOrder::Ordered,
    );
}

#[test]
fn coercion_errors_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             small_value SMALLINT,
             short_label VARCHAR(3),
             fixed_numeric NUMERIC(4, 1)
         ); \
         INSERT INTO __TABLE__ VALUES ('bad', 'abc', 1); \
         INSERT INTO __TABLE__ VALUES (40000, 'abc', 1); \
         INSERT INTO __TABLE__ VALUES (1, 'toolong', 1); \
         INSERT INTO __TABLE__ VALUES (1, 'abc', 1234.5); \
         SELECT TRUE::BYTEA FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn updates_rows_with_expressions_and_where() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, amount INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 2), (3, 4), (5, 1); \
         UPDATE __TABLE__ SET id = id + amount, amount = id WHERE amount > 2; \
         SELECT * FROM __TABLE__; \
         UPDATE __TABLE__ SET amount = amount * 2; \
         SELECT * FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn deletes_rows_with_and_without_where() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, amount INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 2), (2, NULL), (3, 4); \
         DELETE FROM __TABLE__ WHERE amount > 2; \
         SELECT * FROM __TABLE__; \
         DELETE FROM __TABLE__; \
         SELECT * FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn delete_visibility_matches_postgres_across_sessions() {
    assert_session_differential(
        &[
            (SessionName::First, "CREATE TABLE __TABLE__ (id INTEGER)"),
            (SessionName::First, "INSERT INTO __TABLE__ VALUES (1), (2)"),
            (SessionName::First, "BEGIN"),
            (SessionName::First, "DELETE FROM __TABLE__ WHERE id = 1"),
            (SessionName::First, "SELECT * FROM __TABLE__"),
            (SessionName::Second, "SELECT * FROM __TABLE__"),
            (SessionName::First, "COMMIT"),
            (SessionName::Second, "SELECT * FROM __TABLE__"),
        ],
        RowOrder::Unordered,
    );
}

#[test]
fn filters_with_boolean_expressions() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, score INTEGER, active BOOLEAN, optional INTEGER); \
         INSERT INTO __TABLE__ VALUES \
             (1, 3, TRUE, NULL), (2, 10, TRUE, 5), \
             (3, 8, FALSE, 4), (4, NULL, TRUE, 1); \
         SELECT id, score FROM __TABLE__ \
         WHERE (score + id > 10 AND active) OR optional IS NULL",
        RowOrder::Ordered,
    );
}

#[test]
fn evaluates_null_and_three_valued_logic() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, a BOOLEAN, b BOOLEAN); \
         INSERT INTO __TABLE__ VALUES \
             (1, TRUE, TRUE), (2, TRUE, FALSE), (3, TRUE, NULL), \
             (4, FALSE, TRUE), (5, FALSE, FALSE), (6, FALSE, NULL), \
             (7, NULL, TRUE), (8, NULL, FALSE), (9, NULL, NULL); \
         SELECT \
             a AND b, a OR b, NOT a, \
             a IS TRUE, a IS FALSE, a IS UNKNOWN, a IS NULL, a IS NOT NULL, \
             a IS DISTINCT FROM b, a IS NOT DISTINCT FROM b, \
             id + NULL, id = NULL, \
             id IS DISTINCT FROM NULL, id IS NOT DISTINCT FROM NULL \
         FROM __TABLE__",
        RowOrder::Ordered,
    );
}

#[test]
fn reports_arithmetic_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER);\
         INSERT INTO __TABLE__ VALUES (2147483647);\
         SELECT id / 0 FROM __TABLE__;\
         SELECT id + 1 FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn compares_sqlstate_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER);\
         SELECT missing FROM __TABLE__",
        RowOrder::Unordered,
    );
}
