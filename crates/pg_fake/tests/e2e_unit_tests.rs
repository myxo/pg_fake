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
    let configured_url = env::var("PG_FAKE_DATABASE_URL").ok();
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
    let configured_url = env::var("PG_FAKE_DATABASE_URL").ok();
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
fn isolation_levels_match_postgres_across_sessions() {
    assert_session_differential(
        &[
            (SessionName::First, "CREATE TABLE __TABLE__ (id INTEGER)"),
            (SessionName::First, "INSERT INTO __TABLE__ VALUES (1)"),
            (SessionName::First, "BEGIN ISOLATION LEVEL READ COMMITTED"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::Second, "INSERT INTO __TABLE__ VALUES (2)"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::First, "COMMIT"),
            (SessionName::First, "BEGIN ISOLATION LEVEL REPEATABLE READ"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::Second, "INSERT INTO __TABLE__ VALUES (3)"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::First, "COMMIT"),
            (
                SessionName::First,
                "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
            ),
            (SessionName::First, "BEGIN"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::Second, "INSERT INTO __TABLE__ VALUES (4)"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::First, "COMMIT"),
            (SessionName::First, "BEGIN ISOLATION LEVEL READ COMMITTED"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::Second, "INSERT INTO __TABLE__ VALUES (5)"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::First, "COMMIT"),
            (SessionName::First, "BEGIN"),
            (
                SessionName::First,
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
            ),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::Second, "INSERT INTO __TABLE__ VALUES (6)"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::First, "COMMIT"),
            (SessionName::First, "BEGIN"),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (
                SessionName::First,
                "SET TRANSACTION ISOLATION LEVEL READ COMMITTED",
            ),
            (SessionName::First, "SELECT * FROM __TABLE__ ORDER BY id"),
            (SessionName::First, "ROLLBACK"),
        ],
        RowOrder::Ordered,
    );
}

#[test]
fn lock_timeout_and_row_lock_clauses_match_postgres() {
    assert_differential(
        "SET lock_timeout = 250; \
         SET lock_timeout = '100ms'; \
         SET lock_timeout = '2s'; \
         SET lock_timeout = 0; \
         SET lock_timeout = 'invalid'; \
         CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (1), (2); \
         BEGIN; \
         SELECT * FROM __TABLE__ ORDER BY id FOR UPDATE; \
         ROLLBACK; \
         BEGIN; \
         SELECT * FROM __TABLE__ ORDER BY id FOR SHARE; \
         ROLLBACK",
        RowOrder::Ordered,
    );
}

#[test]
fn case_boundaries_and_function_errors_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (1); \
         SELECT CASE id WHEN -2147483648 THEN 'minimum' ELSE 'other' END FROM __TABLE__; \
         SELECT CASE WHEN id = 1 THEN id ELSE TRUE END FROM __TABLE__; \
         SELECT unknown_function(id) FROM __TABLE__",
        RowOrder::Unordered,
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
fn order_by_position_errors_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (1); \
         SELECT id FROM __TABLE__ ORDER BY 0; \
         SELECT id FROM __TABLE__ ORDER BY 2",
        RowOrder::Ordered,
    );
}

#[test]
fn limit_and_offset_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (4), (1), (5), (2), (3); \
         SELECT id FROM __TABLE__ ORDER BY id LIMIT 2; \
         SELECT id FROM __TABLE__ ORDER BY id OFFSET 2; \
         SELECT id FROM __TABLE__ ORDER BY id LIMIT 2 OFFSET 1; \
         SELECT id FROM __TABLE__ ORDER BY id LIMIT NULL OFFSET NULL",
        RowOrder::Ordered,
    );
}

#[test]
fn limit_and_offset_without_order_by_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (4), (1), (5), (2), (3); \
         SELECT id FROM __TABLE__ LIMIT 3; \
         SELECT id FROM __TABLE__ OFFSET 3",
        RowOrder::Unordered,
    );
}

#[test]
fn negative_limit_and_offset_errors_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         SELECT id FROM __TABLE__ LIMIT -1; \
         SELECT id FROM __TABLE__ OFFSET -1",
        RowOrder::Ordered,
    );
}

#[test]
fn not_null_and_defaults_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             id INTEGER NOT NULL DEFAULT 10,
             amount INTEGER NOT NULL DEFAULT 2 + 3,
             label TEXT DEFAULT upper('mixed'),
             optional INTEGER
         ); \
         INSERT INTO __TABLE__ DEFAULT VALUES; \
         INSERT INTO __TABLE__ (id, label) VALUES (1, DEFAULT), (2, NULL); \
         INSERT INTO __TABLE__ (id, amount) VALUES (3, DEFAULT); \
         UPDATE __TABLE__ SET amount = DEFAULT, label = DEFAULT WHERE id = 2; \
         SELECT id, amount, label, optional FROM __TABLE__ ORDER BY id",
        RowOrder::Ordered,
    );
}

#[test]
fn not_null_and_default_errors_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER NOT NULL, optional INTEGER); \
         INSERT INTO __TABLE__ (optional) VALUES (1); \
         INSERT INTO __TABLE__ VALUES (1, NULL); \
         UPDATE __TABLE__ SET id = DEFAULT; \
         CREATE TABLE invalid_default (a INTEGER, b INTEGER DEFAULT a)",
        RowOrder::Unordered,
    );
}

#[test]
fn primary_and_unique_constraints_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             id INTEGER PRIMARY KEY,
             tenant INTEGER,
             email TEXT,
             UNIQUE (tenant, email)
         ); \
         INSERT INTO __TABLE__ VALUES (1, 1, 'a'), (2, 1, 'b'); \
         INSERT INTO __TABLE__ VALUES (1, 2, 'c'); \
         INSERT INTO __TABLE__ VALUES (3, 1, 'a'); \
         UPDATE __TABLE__ SET id = 1 WHERE id = 2; \
         UPDATE __TABLE__ SET id = 3, email = 'c' WHERE id = 2; \
         INSERT INTO __TABLE__ VALUES (NULL, 2, 'd'); \
         INSERT INTO __TABLE__ VALUES (4, NULL, 'a'), (5, NULL, 'a'); \
         DELETE FROM __TABLE__ WHERE id = 1; \
         INSERT INTO __TABLE__ VALUES (1, 1, 'a'); \
         SELECT * FROM __TABLE__ ORDER BY id",
        RowOrder::Ordered,
    );
}

#[test]
fn check_constraints_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             id INTEGER CHECK (id > 0),
             lower_bound INTEGER,
             upper_bound INTEGER,
             CHECK (lower_bound < upper_bound)
         ); \
         INSERT INTO __TABLE__ VALUES (1, 1, 2), (NULL, NULL, NULL); \
         INSERT INTO __TABLE__ VALUES (-1, 1, 2); \
         INSERT INTO __TABLE__ VALUES (2, 3, 2); \
         UPDATE __TABLE__ SET id = -1 WHERE id = 1; \
         UPDATE __TABLE__ SET lower_bound = NULL WHERE id = 1; \
         SELECT * FROM __TABLE__ ORDER BY id",
        RowOrder::Ordered,
    );
}

#[test]
fn unique_value_semantics_match_postgres() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             float_value DOUBLE PRECISION UNIQUE,
             numeric_value NUMERIC UNIQUE,
             char_value CHAR(3) UNIQUE
         ); \
         INSERT INTO __TABLE__ VALUES ('NaN', 1.0, 'x'); \
         INSERT INTO __TABLE__ (float_value) VALUES ('NaN'); \
         INSERT INTO __TABLE__ (numeric_value) VALUES (1.00); \
         INSERT INTO __TABLE__ (char_value) VALUES ('x  '); \
         INSERT INTO __TABLE__ VALUES (NULL, NULL, NULL); \
         INSERT INTO __TABLE__ VALUES (NULL, NULL, NULL)",
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
