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

fn assert_differential(script: &str, row_order: RowOrder) {
    static TABLE_NUMBER: AtomicU64 = AtomicU64::new(1);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

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
