use std::{
    env,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use pg_fake::{
    api::{Db, StatementResult},
    parser::{self, Statement},
    value::{BaseType, Value},
};
use postgres::{Client, NoTls, SimpleQueryMessage};
use testcontainers::{Container, ImageExt, runners::SyncRunner};
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

struct PostgresServer {
    url: String,
    _container: Option<Container<Postgres>>,
}

fn start_postgres_server() -> PostgresServer {
    let configured_url = dotenvy::var("PG_FAKE_DATABASE_URL").ok();
    if configured_url.is_none() && env::var_os("DOCKER_HOST").is_none() {
        let socket = PathBuf::from(env::var_os("HOME").expect("HOME must be set"))
            .join(".colima/default/docker.sock");
        if socket.exists() {
            unsafe { env::set_var("DOCKER_HOST", format!("unix://{}", socket.display())) };
        }
    }
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
    PostgresServer {
        url,
        _container: container,
    }
}

fn returns_rows(statement: &Statement) -> bool {
    match statement {
        Statement::Query(_) => true,
        Statement::Insert(insert) => insert.returning.is_some(),
        Statement::Update(update) => update.returning.is_some(),
        Statement::Delete(delete) => delete.returning.is_some(),
        _ => false,
    }
}

fn assert_differential(script: &str, row_order: RowOrder) {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let server = start_postgres_server();

    let table_name = format!(
        "pg_fake_differential_{}_{}",
        std::process::id(),
        TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
    );
    let script = script.replace("__TABLE__", &table_name);
    let mut postgres = Client::connect(&server.url, NoTls).expect("must connect to PostgreSQL");
    let db = Db::create();
    let mut fake = db.create_session();

    for statement in parser::parse(&script).unwrap() {
        let sql = statement.to_string();
        let expected = execute_on_postgres(&mut postgres, &statement, &sql);
        let actual = execute_on_fake(&mut fake, &statement, &sql);
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
    let server = start_postgres_server();

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
    let mut postgres_first =
        Client::connect(&server.url, NoTls).expect("must connect to PostgreSQL");
    let mut postgres_second =
        Client::connect(&server.url, NoTls).expect("must connect to PostgreSQL");
    let db = Db::create();
    let mut fake_first = db.create_session();
    let mut fake_second = db.create_session();

    for (session, statement, sql) in operations {
        let (postgres, fake) = match session {
            SessionName::First => (&mut postgres_first, &mut fake_first),
            SessionName::Second => (&mut postgres_second, &mut fake_second),
        };
        let expected = execute_on_postgres(postgres, &statement, &sql);
        let actual = execute_on_fake(fake, &statement, &sql);
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

fn execute_on_postgres(client: &mut Client, statement: &Statement, sql: &str) -> Outcome {
    match client.simple_query(sql) {
        Ok(messages) if returns_rows(statement) => Outcome::Rows(
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
        Ok(messages) => Outcome::Affected(
            messages
                .iter()
                .filter_map(|message| match message {
                    SimpleQueryMessage::CommandComplete(rows) => Some(*rows),
                    _ => None,
                })
                .last()
                .expect("non-query statements must complete"),
        ),
        Err(error) => Outcome::Error(
            error
                .code()
                .expect("PostgreSQL execution errors must have a SQLSTATE")
                .code()
                .into(),
        ),
    }
}

fn execute_on_fake(
    session: &mut pg_fake::api::Session,
    statement: &Statement,
    sql: &str,
) -> Outcome {
    if returns_rows(statement) {
        match session.query(sql, &[]) {
            Ok(result) => Outcome::Rows(
                result
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|value| match value {
                                Value::Null => None,
                                value => Some(value.format_postgres_text()),
                            })
                            .collect()
                    })
                    .collect(),
            ),
            Err(error) => Outcome::Error(error.sqlstate.get_code().into()),
        }
    } else {
        match session.execute(sql) {
            Ok(results) => match results.as_slice() {
                [StatementResult::Affected(rows)] => Outcome::Affected(*rows),
                _ => panic!("single non-query statement must return an affected-row result"),
            },
            Err(error) => Outcome::Error(error.sqlstate.get_code().into()),
        }
    }
}

#[test]
fn matches_explicit_transactions_across_sessions() {
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
fn matches_sequence_options_functions_and_errors() {
    assert_differential(
        "CREATE SEQUENCE __TABLE__ AS smallint INCREMENT BY 3 MINVALUE 10 MAXVALUE 16 START WITH 13 CACHE 8 CYCLE; \
         SELECT nextval('__TABLE__'); \
         SELECT nextval('__TABLE__'); \
         SELECT nextval('__TABLE__'); \
         SELECT currval('__TABLE__'); \
         SELECT lastval(); \
         SELECT setval('__TABLE__', 13); \
         SELECT currval('__TABLE__'); \
         SELECT nextval('__TABLE__'); \
         SELECT setval('__TABLE__', 10, false); \
         SELECT currval('__TABLE__'); \
         SELECT nextval('__TABLE__'); \
         SELECT setval('__TABLE__', 20); \
         DROP SEQUENCE __TABLE__; \
         SELECT lastval()",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_sequence_session_state_and_rollback_consumption() {
    assert_session_differential(
        &[
            (SessionName::First, "CREATE SEQUENCE __TABLE__ START 20"),
            (SessionName::First, "SELECT nextval('__TABLE__')"),
            (SessionName::Second, "SELECT currval('__TABLE__')"),
            (SessionName::Second, "SELECT nextval('__TABLE__')"),
            (SessionName::First, "SELECT currval('__TABLE__')"),
            (SessionName::Second, "SELECT currval('__TABLE__')"),
            (SessionName::First, "BEGIN"),
            (SessionName::First, "SELECT nextval('__TABLE__')"),
            (SessionName::First, "ROLLBACK"),
            (SessionName::Second, "SELECT nextval('__TABLE__')"),
            (SessionName::First, "SELECT setval('__TABLE__', 50, true)"),
            (SessionName::First, "SELECT currval('__TABLE__')"),
            (SessionName::Second, "SELECT currval('__TABLE__')"),
            (SessionName::First, "DROP SEQUENCE __TABLE__"),
            (SessionName::First, "CREATE SEQUENCE __TABLE__ START 100"),
            (SessionName::First, "SELECT currval('__TABLE__')"),
            (SessionName::First, "SELECT nextval('__TABLE__')"),
        ],
        RowOrder::Ordered,
    );
}

#[test]
fn matches_sequence_validation_sqlstates() {
    assert_differential(
        "CREATE SEQUENCE __TABLE___zero INCREMENT 0; \
         CREATE SEQUENCE __TABLE___bounds MINVALUE 10 MAXVALUE 5; \
         CREATE SEQUENCE __TABLE___start MINVALUE 1 MAXVALUE 5 START 6; \
         CREATE SEQUENCE __TABLE___cache CACHE 0; \
         CREATE SEQUENCE __TABLE___type AS numeric; \
         CREATE SEQUENCE __TABLE___limited MAXVALUE 2; \
         SELECT nextval('__TABLE___limited'); \
         SELECT nextval('__TABLE___limited'); \
         SELECT nextval('__TABLE___limited'); \
         SELECT setval('__TABLE___limited', 3); \
         CREATE SEQUENCE __TABLE___limited; \
         CREATE TABLE __TABLE___ordinary (id INTEGER); \
         DROP SEQUENCE __TABLE___ordinary; \
         SELECT nextval('__TABLE___ordinary')",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_sequence_defaults_and_failed_insert_gaps() {
    assert_differential(
        "CREATE SEQUENCE __TABLE___sequence; \
         CREATE TABLE __TABLE__ (id BIGINT DEFAULT nextval('__TABLE___sequence'), marker INTEGER UNIQUE); \
         INSERT INTO __TABLE__ (marker) VALUES (1) RETURNING id; \
         INSERT INTO __TABLE__ (marker) VALUES (1); \
         INSERT INTO __TABLE__ (marker) VALUES (2) RETURNING id; \
         SELECT currval('__TABLE___sequence')",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_isolation_levels_across_sessions() {
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
fn matches_lock_timeout_and_row_lock_clauses() {
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
fn matches_set_operations() {
    assert_differential(
        "SELECT 1 AS value UNION SELECT 1 UNION SELECT 2 ORDER BY value; \
         SELECT 1 AS value UNION ALL SELECT 1 UNION ALL SELECT 2 ORDER BY value; \
         VALUES (1), (1), (2), (NULL) INTERSECT VALUES (1), (1), (NULL) ORDER BY 1 NULLS FIRST; \
         VALUES (1), (1), (2), (NULL) INTERSECT ALL VALUES (1), (NULL) ORDER BY 1 NULLS FIRST; \
         VALUES (1), (1), (2), (NULL) EXCEPT VALUES (1), (NULL) ORDER BY 1; \
         VALUES (1), (1), (2), (NULL) EXCEPT ALL VALUES (1), (NULL) ORDER BY 1; \
         CREATE TABLE __TABLE___left (id INTEGER); \
         CREATE TABLE __TABLE___right (id BIGINT); \
         INSERT INTO __TABLE___left VALUES (1), (3); \
         INSERT INTO __TABLE___right VALUES (2), (3); \
         SELECT id FROM __TABLE___left UNION SELECT id FROM __TABLE___right ORDER BY id LIMIT 2 OFFSET 1; \
         (SELECT 1 AS value UNION SELECT 2) INTERSECT SELECT 2 ORDER BY value; \
         SELECT total FROM (SELECT sum(id) AS total FROM __TABLE___left UNION SELECT 4) AS source ORDER BY total; \
         SELECT 1 UNION SELECT 1, 2; \
         SELECT 1 UNION SELECT true",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_non_recursive_ctes() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (1), (2); \
         WITH source(value) AS (SELECT id FROM __TABLE__), doubled(value) AS (SELECT value * 2 FROM source) SELECT source.value, doubled.value FROM source JOIN doubled ON doubled.value = source.value * 2 ORDER BY source.value; \
         CREATE SEQUENCE __TABLE___sequence; \
         WITH sampled(value) AS (SELECT nextval('__TABLE___sequence')) SELECT left_sample.value = right_sample.value FROM sampled AS left_sample CROSS JOIN sampled AS right_sample; \
         WITH values_cte(value) AS (SELECT 1) SELECT (WITH values_cte(value) AS (SELECT 2) SELECT value FROM values_cte) FROM values_cte; \
         WITH empty_values(value) AS (SELECT id FROM __TABLE__ WHERE false) SELECT value FROM empty_values",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_recursive_ctes() {
    assert_differential(
        "WITH RECURSIVE series(value) AS (VALUES (1) UNION ALL SELECT value + 1 FROM series WHERE value < 5) SELECT value FROM series ORDER BY value; \
         CREATE TABLE __TABLE___edges (parent INTEGER, child INTEGER); \
         INSERT INTO __TABLE___edges VALUES (1, 2), (1, 3), (2, 4), (3, 5); \
         WITH RECURSIVE walk(value) AS (VALUES (1) UNION ALL SELECT edges.child FROM walk JOIN __TABLE___edges AS edges ON edges.parent = walk.value) SELECT value FROM walk ORDER BY value; \
         CREATE TABLE __TABLE___cycle (parent INTEGER, child INTEGER); \
         INSERT INTO __TABLE___cycle VALUES (1, 2), (2, 1); \
         WITH RECURSIVE walk(value) AS (VALUES (1) UNION SELECT edges.child FROM walk JOIN __TABLE___cycle AS edges ON edges.parent = walk.value) SELECT value FROM walk ORDER BY value; \
         WITH RECURSIVE empty(value) AS (SELECT 1 WHERE false UNION ALL SELECT value + 1 FROM empty WHERE value < 3) SELECT value FROM empty; \
         WITH RECURSIVE values_cte(value) AS (VALUES (1)), series(value) AS (SELECT value FROM values_cte UNION ALL SELECT value + 1 FROM series WHERE value < 3) SELECT value FROM series ORDER BY value; \
         WITH RECURSIVE first_cte(value) AS (SELECT value FROM later_cte), later_cte(value) AS (VALUES (7)) SELECT value FROM first_cte; \
         WITH RECURSIVE nullable(value) AS (VALUES (NULL::INTEGER) UNION SELECT value + 1 FROM nullable WHERE value < 3) SELECT value FROM nullable; \
         WITH RECURSIVE series(value) AS (VALUES (1::BIGINT) UNION ALL SELECT value + 1 FROM series WHERE value < 3) SELECT value FROM series ORDER BY value; \
         SELECT value FROM (WITH RECURSIVE series(value) AS (VALUES (1) UNION ALL SELECT value + 1 FROM series WHERE value < 3) SELECT value FROM series) AS nested ORDER BY value; \
         WITH RECURSIVE series(value) AS (VALUES (1) UNION ALL SELECT series.value + 1 FROM series LEFT JOIN (VALUES (true)) AS preserved(flag) ON true WHERE series.value < 3) SELECT value FROM series ORDER BY value",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_recursive_cte_errors() {
    assert_differential(
        "WITH RECURSIVE series(value) AS (SELECT value + 1 FROM series UNION ALL VALUES (1)) SELECT value FROM series; \
         WITH RECURSIVE series(value) AS (VALUES (1) UNION ALL SELECT left_series.value + 1 FROM series AS left_series CROSS JOIN series AS right_series) SELECT value FROM series; \
         WITH RECURSIVE first_cte(value) AS (SELECT value FROM second_cte), second_cte(value) AS (SELECT value FROM first_cte) SELECT value FROM first_cte; \
         WITH RECURSIVE series(value) AS (VALUES (1::SMALLINT) UNION ALL SELECT value + 1 FROM series WHERE value < 2) SELECT value FROM series; \
         WITH RECURSIVE series(value) AS (VALUES (1) UNION ALL SELECT (SELECT value FROM series) WHERE false) SELECT value FROM series; \
         WITH RECURSIVE series(value) AS (VALUES (1) UNION ALL SELECT nullable.value FROM (VALUES (1)) AS source(value) LEFT JOIN series AS nullable ON true) SELECT value FROM series",
        RowOrder::Ordered,
    );
}

#[test]
fn executes_parameterized_recursive_ctes() {
    let db = Db::create();
    let mut session = db.create_session();
    let statement = session
        .prepare(
            "WITH RECURSIVE series(value) AS (VALUES ($1::INTEGER) UNION ALL SELECT value + 1 FROM series WHERE value < $2::INTEGER) SELECT value FROM series ORDER BY value",
        )
        .unwrap();
    let result = session
        .query_prepared(&statement, &[Value::Int4(2), Value::Int4(4)])
        .unwrap();
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Int4(2)],
            vec![Value::Int4(3)],
            vec![Value::Int4(4)],
        ]
    );
    assert_eq!(result.columns[0].name, "value");
    assert_eq!(result.columns[0].type_oid, BaseType::Int4.map_to_oid());
}

#[test]
fn executes_parameterized_set_operations() {
    let db = Db::create();
    let mut session = db.create_session();
    let statement = session
        .prepare("SELECT $1 AS value UNION SELECT 2")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&statement, &[Value::Int2(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)], vec![Value::Int4(2)]],
    );
}

#[test]
fn keeps_left_set_operation_column_metadata() {
    let db = Db::create();
    let mut session = db.create_session();
    let result = session
        .query(
            "SELECT 1 AS left_name UNION SELECT 2::BIGINT AS right_name",
            &[],
        )
        .unwrap();
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![("left_name", BaseType::Int8.map_to_oid(), -1,)]
    );
}

#[test]
fn matches_foreign_keys_and_referential_actions() {
    assert_differential(
        "CREATE TABLE __TABLE___parents (first_id INTEGER, second_id INTEGER, PRIMARY KEY (first_id, second_id)); \
         CREATE TABLE __TABLE___children (id INTEGER PRIMARY KEY, first_id INTEGER, second_id INTEGER, FOREIGN KEY (first_id, second_id) REFERENCES __TABLE___parents (first_id, second_id) ON DELETE CASCADE ON UPDATE CASCADE); \
         INSERT INTO __TABLE___parents VALUES (1, 2); \
         INSERT INTO __TABLE___children VALUES (1, 1, 2); \
         INSERT INTO __TABLE___children VALUES (2, 1, 3); \
         UPDATE __TABLE___parents SET first_id = 3 WHERE first_id = 1; \
         SELECT first_id, second_id FROM __TABLE___children ORDER BY id; \
         DELETE FROM __TABLE___parents WHERE first_id = 3; \
         SELECT * FROM __TABLE___children",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_parameter_and_prepared_reuse() {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let server = start_postgres_server();
    let table = format!(
        "pg_fake_differential_{}_{}",
        std::process::id(),
        TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
    );
    let mut postgres = Client::connect(&server.url, NoTls).expect("must connect to PostgreSQL");
    let db = Db::create();
    let mut fake = db.create_session();
    let create = format!("CREATE TABLE {table} (id INTEGER, name TEXT, amount SMALLINT)");
    postgres.batch_execute(&create).unwrap();
    fake.execute(&create).unwrap();

    let insert_sql = format!("INSERT INTO {table} VALUES ($1, $2, $3)");
    let postgres_insert = postgres.prepare(&insert_sql).unwrap();
    let fake_insert = fake.prepare(&insert_sql).unwrap();
    for (id, name, amount) in [(1_i32, "first", 10_i16), (2, "second", 20)] {
        assert_eq!(
            fake.execute_prepared(
                &fake_insert,
                &[
                    Value::Int4(id),
                    Value::Text(name.into()),
                    Value::Int2(amount),
                ],
            )
            .unwrap(),
            postgres
                .execute(&postgres_insert, &[&id, &name, &amount])
                .unwrap()
        );
    }

    let select_sql = format!("SELECT name, amount FROM {table} WHERE id = $1");
    let postgres_select = postgres.prepare(&select_sql).unwrap();
    let fake_select = fake.prepare(&select_sql).unwrap();
    for id in [1_i32, 2] {
        let expected = postgres
            .query(&postgres_select, &[&id])
            .unwrap()
            .into_iter()
            .map(|row| (row.get::<_, String>(0), row.get::<_, i16>(1)))
            .collect::<Vec<_>>();
        let actual = fake
            .query_prepared(&fake_select, &[Value::Int4(id)])
            .unwrap()
            .rows
            .into_iter()
            .map(|row| match row.as_slice() {
                [Value::Text(name), Value::Int2(amount)] => (name.clone(), *amount),
                _ => panic!("unexpected fake row"),
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);

        let postgres_inline = postgres
            .query(
                &format!("SELECT name, amount FROM {table} WHERE id = {id}"),
                &[],
            )
            .unwrap()
            .into_iter()
            .map(|row| (row.get::<_, String>(0), row.get::<_, i16>(1)))
            .collect::<Vec<_>>();
        let fake_inline = fake
            .query(
                &format!("SELECT name, amount FROM {table} WHERE id = {id}"),
                &[],
            )
            .unwrap()
            .rows
            .into_iter()
            .map(|row| match row.as_slice() {
                [Value::Text(name), Value::Int2(amount)] => (name.clone(), *amount),
                _ => panic!("unexpected fake row"),
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, postgres_inline);
        assert_eq!(actual, fake_inline);
    }
}

#[test]
fn matches_multi_statement_batches_and_metadata() {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let server = start_postgres_server();
    let suffix = TABLE_NUMBER.fetch_add(1, Ordering::Relaxed);
    let batch_table = format!("pg_fake_batch_{}_{}", std::process::id(), suffix);
    let types_table = format!("pg_fake_types_{}_{}", std::process::id(), suffix);
    let failed_table = format!("pg_fake_failed_{}_{}", std::process::id(), suffix);
    let mut postgres = Client::connect(&server.url, NoTls).expect("must connect to PostgreSQL");
    let db = Db::create();
    let mut fake = db.create_session();

    let batch = format!(
        "CREATE TABLE {batch_table} (id INTEGER, name TEXT); \
         INSERT INTO {batch_table} VALUES (1, 'one'), (2, 'two'); \
         UPDATE {batch_table} SET name = upper(name) WHERE id = 2; \
         SELECT id, name FROM {batch_table} ORDER BY id"
    );
    let postgres_messages = postgres.simple_query(&batch).unwrap();
    let postgres_counts = postgres_messages
        .iter()
        .filter_map(|message| match message {
            SimpleQueryMessage::CommandComplete(rows) => Some(*rows),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(postgres_counts, vec![0, 2, 1, 2]);
    let fake_results = fake.execute(&batch).unwrap();
    assert_eq!(fake_results.len(), 4);
    assert_eq!(fake_results[0], StatementResult::Affected(0));
    assert_eq!(fake_results[1], StatementResult::Affected(2));
    assert_eq!(fake_results[2], StatementResult::Affected(1));
    let StatementResult::Query(fake_query) = &fake_results[3] else {
        panic!("last batch result must be a query");
    };
    assert_eq!(
        fake_query.rows,
        vec![
            vec![Value::Int4(1), Value::Text("one".into())],
            vec![Value::Int4(2), Value::Text("TWO".into())],
        ]
    );

    let create_types = format!(
        "CREATE TABLE {types_table} (
            flag BOOLEAN, small_value SMALLINT, int_value INTEGER,
            big_value BIGINT, real_value REAL, double_value DOUBLE PRECISION,
            numeric_value NUMERIC(5, 2), text_value TEXT,
            varying_value VARCHAR(3), fixed_value CHAR(2), bytes BYTEA
        )"
    );
    postgres.batch_execute(&create_types).unwrap();
    fake.execute(&create_types).unwrap();
    let expected_metadata = postgres
        .query(
            "SELECT atttypid::int4, atttypmod
             FROM pg_attribute
             WHERE attrelid = $1::text::regclass AND attnum > 0 AND NOT attisdropped
             ORDER BY attnum",
            &[&types_table],
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.get::<_, i32>(0) as u32, row.get::<_, i32>(1)))
        .collect::<Vec<_>>();
    let actual_metadata = fake
        .query(&format!("SELECT * FROM {types_table}"), &[])
        .unwrap()
        .columns
        .into_iter()
        .map(|column| (column.type_oid, column.typmod))
        .collect::<Vec<_>>();
    assert_eq!(actual_metadata, expected_metadata);

    let failed_batch = format!(
        "CREATE TABLE {failed_table} (id INTEGER); \
         INSERT INTO {failed_table} VALUES (1); \
         INSERT INTO {failed_table} VALUES ('bad')"
    );
    let postgres_error = postgres.simple_query(&failed_batch).unwrap_err();
    let fake_error = fake.execute(&failed_batch).unwrap_err();
    assert_eq!(
        fake_error.sqlstate.get_code(),
        postgres_error.code().unwrap().code()
    );
    let relation: Option<String> = postgres
        .query_one("SELECT to_regclass($1)::text", &[&failed_table])
        .unwrap()
        .get(0);
    assert_eq!(relation, None);
    assert_eq!(
        fake.query(&format!("SELECT * FROM {failed_table}"), &[])
            .unwrap_err()
            .sqlstate
            .get_code(),
        "42P01"
    );
}

#[test]
fn matches_case_boundaries_and_function_errors() {
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
fn matches_global_aggregate_results() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             small_value SMALLINT, int_value INTEGER, big_value BIGINT,
             numeric_value NUMERIC(8, 2), real_value REAL,
             double_value DOUBLE PRECISION, flag BOOLEAN, label TEXT,
             bytes BYTEA, happened_on DATE, elapsed INTERVAL
         ); \
         INSERT INTO __TABLE__ VALUES
             (1, 2, 3, 4.50, 1.25, 2.5, TRUE, 'b', '\\x02', '2024-01-02', '1 day'),
             (2, 3, 4, 5.50, 2.25, 3.5, FALSE, 'a', '\\x01', '2024-01-01', '2 days'),
             (NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL); \
         SELECT count(*), count(int_value), sum(small_value), sum(int_value),
                sum(big_value), sum(numeric_value), sum(real_value), sum(double_value)
         FROM __TABLE__; \
         SELECT avg(small_value), avg(int_value), avg(big_value), avg(numeric_value),
                avg(real_value), avg(double_value), sum(elapsed), avg(elapsed)
         FROM __TABLE__; \
         SELECT min(label), max(label) FROM (VALUES ('a'), ('MiXeD')) AS labels(label); \
         SELECT lower('AB  '::CHAR(4)), upper('ab  '::CHAR(4)), length('ab  '::CHAR(4)); \
         SELECT min(int_value), max(int_value), min(label), max(label),
                min(bytes), max(bytes), min(happened_on), max(happened_on),
                bool_and(flag), bool_or(flag)
         FROM __TABLE__; \
         SELECT count(*) + count(int_value), coalesce(sum(int_value), 0),
                max(int_value) - min(int_value)
         FROM __TABLE__ ORDER BY count(*); \
         SELECT avg(-0.0::REAL)",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_empty_and_filtered_global_aggregates() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, flag BOOLEAN); \
         SELECT count(*), count(id), sum(id), avg(id), min(id), max(id),
                bool_and(flag), bool_or(flag) FROM __TABLE__; \
         INSERT INTO __TABLE__ VALUES (1, TRUE), (NULL, NULL); \
         SELECT count(*), count(id), sum(id), avg(id), min(id), max(id),
                bool_and(flag), bool_or(flag) FROM __TABLE__ WHERE id > 10; \
         SELECT count(*) FROM __TABLE__ LIMIT 0; \
         SELECT count(*) FROM __TABLE__ OFFSET 1",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_aggregates_with_scalar_subqueries() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (1), (2), (3); \
         SELECT (SELECT sum(inner_row.id) FROM __TABLE__ AS inner_row
                 WHERE inner_row.id <= outer_row.id)
         FROM __TABLE__ AS outer_row ORDER BY outer_row.id; \
         SELECT sum((SELECT inner_row.id FROM __TABLE__ AS inner_row
                     WHERE inner_row.id = outer_row.id))
         FROM __TABLE__ AS outer_row",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_grouping_having_and_output_references() {
    assert_differential(
        "CREATE TABLE __TABLE__ (category INTEGER, value INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 10), (1, 20), (2, NULL), (NULL, 5), (NULL, 5); \
         SELECT category, count(*), sum(value) FROM __TABLE__ \
         GROUP BY category ORDER BY category NULLS FIRST; \
         SELECT category + 1 AS shifted, count(*) AS amount FROM __TABLE__ \
         GROUP BY category ORDER BY amount DESC, shifted NULLS FIRST; \
         SELECT category + 1 AS shifted, count(*) FROM __TABLE__ \
         GROUP BY shifted HAVING count(*) > 1 ORDER BY 1 NULLS FIRST; \
         SELECT category, count(*) FROM __TABLE__ GROUP BY 1 ORDER BY 1 NULLS FIRST",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_grouped_empty_input_distinct_and_filter() {
    assert_differential(
        "CREATE TABLE __TABLE__ (category INTEGER, value INTEGER, keep BOOLEAN); \
         SELECT category, count(*) FROM __TABLE__ GROUP BY category; \
         SELECT count(*) FROM __TABLE__ HAVING count(*) = 0; \
         SELECT count(*) FROM __TABLE__ HAVING FALSE; \
         INSERT INTO __TABLE__ VALUES \
             (1, 10, TRUE), (1, 10, FALSE), (1, 20, TRUE), (2, NULL, TRUE); \
         SELECT category, count(DISTINCT value), sum(DISTINCT value), \
                count(*) FILTER (WHERE keep), sum(value) FILTER (WHERE keep) \
         FROM __TABLE__ GROUP BY category ORDER BY category",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_grouping_errors_and_correlated_having() {
    assert_differential(
        "CREATE TABLE __TABLE__ (category INTEGER, value INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 10), (1, 20), (2, 30); \
         SELECT category, value, count(*) FROM __TABLE__ GROUP BY category; \
         SELECT category FROM __TABLE__ GROUP BY sum(value); \
         SELECT category, count(*) FROM __TABLE__ GROUP BY category ORDER BY value; \
         SELECT category, count(*) FROM __TABLE__ GROUP BY category HAVING value > 0; \
         SELECT category, count(*) FROM __TABLE__ outer_rows GROUP BY category \
         HAVING EXISTS (SELECT 1 FROM __TABLE__ inner_rows \
                        WHERE inner_rows.category = outer_rows.category \
                          AND inner_rows.value > 15) ORDER BY category; \
         SELECT category, count(*) FROM __TABLE__ outer_rows GROUP BY category \
         HAVING EXISTS (SELECT 1 FROM __TABLE__ inner_rows \
                        WHERE inner_rows.value = outer_rows.value)",
        RowOrder::Ordered,
    );

    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER PRIMARY KEY, value INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 10), (2, 20); \
         SELECT id, value, count(*) FROM __TABLE__ GROUP BY id ORDER BY id",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_select_distinct_results_ordering_and_limits() {
    assert_differential(
        "CREATE TABLE __TABLE__ (category INTEGER, label TEXT); \
         INSERT INTO __TABLE__ VALUES \
             (1, 'a'), (1, 'a'), (1, 'b'), (2, 'a'), (NULL, 'n'), (NULL, 'n'); \
         SELECT DISTINCT category, label FROM __TABLE__ \
         ORDER BY category NULLS FIRST, label LIMIT 4 OFFSET 1; \
         SELECT DISTINCT category + 1 AS shifted FROM __TABLE__ \
         ORDER BY shifted NULLS FIRST; \
         SELECT DISTINCT category + 1 FROM __TABLE__ \
         ORDER BY category + 1 NULLS LAST; \
         SELECT DISTINCT category + 1 FROM __TABLE__ AS rows \
         ORDER BY rows.category + 1 NULLS LAST; \
         SELECT DISTINCT count(*) FROM __TABLE__ GROUP BY category ORDER BY count(*); \
         SELECT ALL category FROM __TABLE__ ORDER BY category NULLS FIRST, label",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_select_distinct_on_results() {
    assert_differential(
        "CREATE TABLE __TABLE__ (category INTEGER, kind INTEGER, score INTEGER, label TEXT); \
         INSERT INTO __TABLE__ VALUES \
             (1, 1, 10, 'low'), (1, 1, 20, 'high'), (1, 2, 15, 'other'), \
             (2, 1, 30, 'two'), (NULL, 1, 5, 'null-low'), (NULL, 1, 8, 'null-high'); \
         SELECT DISTINCT ON (category) category, score, label FROM __TABLE__ \
         ORDER BY category NULLS FIRST, score DESC; \
         SELECT DISTINCT ON (category) label FROM __TABLE__ \
         ORDER BY category NULLS LAST, score DESC LIMIT 2; \
         SELECT DISTINCT ON (category, kind) category, kind, score FROM __TABLE__ \
         ORDER BY kind, category NULLS FIRST, score DESC; \
         SELECT DISTINCT ON (count(*)) count(*), category FROM __TABLE__ \
         GROUP BY category ORDER BY count(*), category NULLS FIRST",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_select_distinct_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (category INTEGER, score INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 10), (1, 20), (2, 30); \
         SELECT DISTINCT category FROM __TABLE__ ORDER BY score; \
         SELECT DISTINCT ON (category) category, score FROM __TABLE__ \
         ORDER BY score, category; \
         SELECT DISTINCT category FROM __TABLE__ FOR UPDATE; \
         SELECT DISTINCT ON (category) category FROM __TABLE__ FOR UPDATE",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_aggregate_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, flag BOOLEAN); \
         INSERT INTO __TABLE__ VALUES (1, TRUE), (2, FALSE); \
         SELECT id, count(*) FROM __TABLE__; \
         SELECT sum(count(*)) FROM __TABLE__; \
         SELECT id FROM __TABLE__ WHERE count(*) > 0; \
         SELECT sum(id) FROM __TABLE__ ORDER BY id; \
         SELECT count(*) FROM __TABLE__ FOR UPDATE; \
         SELECT sum(flag), bool_and(id), min(flag), max(flag) FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn matches_aggregate_overflow_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (real_value REAL, double_value DOUBLE PRECISION); \
         INSERT INTO __TABLE__ VALUES (3e38, 1e308), (3e38, 1e308); \
         SELECT sum(real_value) FROM __TABLE__; \
         SELECT sum(double_value) FROM __TABLE__",
        RowOrder::Unordered,
    );
}

#[test]
fn reports_aggregate_result_metadata() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE aggregate_metadata (
                 small_value SMALLINT, int_value INTEGER, big_value BIGINT,
                 real_value REAL, label VARCHAR(10), flag BOOLEAN
             )",
        )
        .unwrap();

    let result = session
        .query(
            "SELECT count(*), sum(small_value), sum(int_value), sum(big_value),
                    sum(real_value), avg(int_value), avg(real_value), min(label), bool_and(flag)
             FROM aggregate_metadata",
            &[],
        )
        .unwrap();

    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![
            ("count", 20, -1),
            ("sum", 20, -1),
            ("sum", 20, -1),
            ("sum", 1700, -1),
            ("sum", 700, -1),
            ("avg", 1700, -1),
            ("avg", 701, -1),
            ("min", 25, -1),
            ("bool_and", 16, -1),
        ]
    );
}

#[test]
fn reports_distinct_result_metadata() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE distinct_metadata (id INTEGER, label VARCHAR(10))")
        .unwrap();

    let result = session
        .query(
            "SELECT DISTINCT label, id FROM distinct_metadata ORDER BY label, id",
            &[],
        )
        .unwrap();

    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![("label", 1043, 14), ("id", 23, -1)]
    );
}

#[test]
fn matches_insert_update_and_delete_returning() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             id INTEGER PRIMARY KEY,
             label VARCHAR(8) DEFAULT 'new',
             amount SMALLINT
         ); \
         INSERT INTO __TABLE__ AS inserted (id, amount) VALUES (1, 10), (2, 20) \
         RETURNING inserted.*, inserted.amount + 1 AS next_amount; \
         INSERT INTO __TABLE__ DEFAULT VALUES RETURNING *; \
         UPDATE __TABLE__ AS updated SET label = upper(label), amount = amount + 2 \
         WHERE id <= 2 \
         RETURNING updated.*, updated.label AS copied_label; \
         UPDATE __TABLE__ SET amount = 0 WHERE FALSE RETURNING *; \
         DELETE FROM __TABLE__ AS deleted WHERE id = 2 \
         RETURNING deleted.*, deleted.amount * 2 AS doubled; \
         DELETE FROM __TABLE__ WHERE FALSE RETURNING *; \
         SELECT * FROM __TABLE__ ORDER BY id NULLS LAST",
        RowOrder::Unordered,
    );
}

#[test]
fn matches_query_sourced_and_joined_mutations() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             id INTEGER PRIMARY KEY,
             label TEXT NOT NULL,
             amount SMALLINT NOT NULL DEFAULT 1 CHECK (amount >= 0)
         ); \
         CREATE TABLE __TABLE___source (
             source_id INTEGER,
             target_id INTEGER,
             label TEXT,
             delta INTEGER
         ); \
         INSERT INTO __TABLE__ VALUES (1, 'old-one', 10), (2, 'old-two', 20); \
         INSERT INTO __TABLE___source VALUES
             (11, 1, 'joined-one', 5),
             (12, 1, 'joined-one', 5),
             (13, 2, 'joined-two', 3); \
         INSERT INTO __TABLE__ (id, label)
             SELECT source_id, label FROM __TABLE___source WHERE source_id <= 12
             RETURNING *; \
         INSERT INTO __TABLE__ (id, label)
             SELECT source_id + 100, label FROM __TABLE___source WHERE FALSE
             RETURNING *; \
         UPDATE __TABLE__ AS target
             SET amount = target.amount + source.delta, label = source.label
             FROM __TABLE___source AS source
             WHERE target.id = source.target_id
             RETURNING target.id, target.amount; \
         UPDATE __TABLE__ AS target
             SET amount = (SELECT target.amount + source.delta)
             FROM __TABLE___source AS source
             WHERE EXISTS (SELECT 1 WHERE target.id = source.target_id)
               AND target.id = 2
             RETURNING target.id, target.amount, source.delta; \
         UPDATE __TABLE__ AS target
             SET amount = target.amount + source.delta
             FROM __TABLE___source AS source
             WHERE target.id = source.source_id
             RETURNING target.*, source.target_id; \
         DELETE FROM __TABLE__ AS target
             USING __TABLE___source AS source
             WHERE EXISTS (SELECT 1 WHERE target.id = source.source_id)
             RETURNING target.*, source.target_id; \
         SELECT * FROM __TABLE__ ORDER BY id",
        RowOrder::Unordered,
    );
}

#[test]
fn preserves_joined_mutation_atomicity_and_transactions() {
    assert_differential(
        "CREATE TABLE __TABLE__ (
             id INTEGER PRIMARY KEY,
             value INTEGER NOT NULL CHECK (value >= 0)
         ); \
         CREATE TABLE __TABLE___source (id INTEGER, value INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 10), (2, 20); \
         INSERT INTO __TABLE___source VALUES (3, 30), (1, 40); \
         INSERT INTO __TABLE__ SELECT * FROM __TABLE___source RETURNING *; \
         SELECT * FROM __TABLE__ ORDER BY id; \
         BEGIN; \
         UPDATE __TABLE__ AS target SET value = source.value
             FROM __TABLE___source AS source WHERE target.id = source.id
             RETURNING target.*, source.value; \
         DELETE FROM __TABLE__ AS target USING __TABLE___source AS source
             WHERE target.id = source.id RETURNING target.*, source.value; \
         ROLLBACK; \
         SELECT * FROM __TABLE__ ORDER BY id; \
         UPDATE __TABLE__ AS target SET value = source.value
             FROM __TABLE___source AS source WHERE id = id; \
         SELECT * FROM __TABLE__ ORDER BY id",
        RowOrder::Unordered,
    );
}

#[test]
fn preserves_returning_statement_atomicity() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER PRIMARY KEY, value INTEGER UNIQUE); \
         INSERT INTO __TABLE__ VALUES (1, 10), (2, 20); \
         INSERT INTO __TABLE__ VALUES (3, 30), (1, 40) RETURNING *; \
         SELECT * FROM __TABLE__ ORDER BY id; \
         UPDATE __TABLE__ SET value = 10 RETURNING *; \
         SELECT * FROM __TABLE__ ORDER BY id; \
         DELETE FROM __TABLE__ RETURNING 1 / (id - id); \
         SELECT * FROM __TABLE__ ORDER BY id; \
         BEGIN; \
         INSERT INTO __TABLE__ VALUES (3, 30) RETURNING id; \
         ROLLBACK; \
         SELECT * FROM __TABLE__ ORDER BY id",
        RowOrder::Unordered,
    );
}

#[test]
fn reports_returning_metadata_and_prepared_results() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE returning_metadata (
                 id INTEGER PRIMARY KEY,
                 label VARCHAR(10) DEFAULT 'default'
             )",
        )
        .unwrap();

    let direct = session
        .execute(
            "INSERT INTO returning_metadata (id) VALUES (0)
             RETURNING id, label",
        )
        .unwrap();
    let [StatementResult::Query(direct)] = direct.as_slice() else {
        panic!("RETURNING must produce a native query result");
    };
    assert_eq!(
        direct.rows,
        vec![vec![Value::Int4(0), Value::Text("default".into())]]
    );

    let insert = session
        .prepare(
            "INSERT INTO returning_metadata (id, label) VALUES ($1, $2)
             RETURNING id, label, label AS copied, id + 1 AS next_id",
        )
        .unwrap();
    assert_eq!(
        insert.get_parameter_types(),
        &[BaseType::Int4, BaseType::Varchar]
    );
    assert_eq!(
        insert
            .get_result_columns()
            .iter()
            .map(|column| (column.name.as_str(), column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![
            ("id", 23, -1),
            ("label", 1043, 14),
            ("copied", 1043, 14),
            ("next_id", 23, -1),
        ]
    );
    let inserted = session
        .query_prepared(&insert, &[Value::Int4(1), Value::Text("first".into())])
        .unwrap();
    assert_eq!(
        inserted.rows,
        vec![vec![
            Value::Int4(1),
            Value::Text("first".into()),
            Value::Text("first".into()),
            Value::Int4(2),
        ]]
    );

    let update = session
        .prepare(
            "UPDATE returning_metadata SET label = $1 WHERE id = $2
             RETURNING returning_metadata.*",
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&update, &[Value::Text("second".into()), Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Text("second".into())]]
    );

    let delete = session
        .prepare("DELETE FROM returning_metadata WHERE id = $1 RETURNING *")
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&delete, &[Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Text("second".into())]]
    );
}

#[test]
fn reports_prepared_joined_mutation_types_and_results() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE prepared_mutation_target (id INTEGER PRIMARY KEY, label VARCHAR(10));
             CREATE TABLE prepared_mutation_source (id INTEGER, label VARCHAR(10));
             INSERT INTO prepared_mutation_source VALUES (1, 'source')",
        )
        .unwrap();

    let insert = session
        .prepare(
            "INSERT INTO prepared_mutation_target (id, label)
             SELECT $1, $2 WHERE $3 RETURNING id, label",
        )
        .unwrap();
    assert_eq!(
        insert.get_parameter_types(),
        &[BaseType::Int4, BaseType::Varchar, BaseType::Bool]
    );
    assert_eq!(
        session
            .query_prepared(
                &insert,
                &[
                    Value::Int4(1),
                    Value::Text("inserted".into()),
                    Value::Bool(true),
                ],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Text("inserted".into())]]
    );

    let correlated_insert = session
        .prepare(
            "INSERT INTO prepared_mutation_target (id, label)
             SELECT source.id + 1, (SELECT source.label)
             FROM prepared_mutation_source AS source WHERE source.id = $1
             RETURNING id, (SELECT label) AS label",
        )
        .unwrap();
    assert_eq!(correlated_insert.get_parameter_types(), &[BaseType::Int4]);
    assert_eq!(
        session
            .query_prepared(&correlated_insert, &[Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2), Value::Text("source".into())]]
    );

    let update = session
        .prepare(
            "UPDATE prepared_mutation_target AS target SET label = (SELECT source.label)
             FROM prepared_mutation_source AS source
             WHERE EXISTS (SELECT 1 WHERE target.id = source.id) AND source.id = $1
             RETURNING target.id, source.label",
        )
        .unwrap();
    assert_eq!(update.get_parameter_types(), &[BaseType::Int4]);
    assert_eq!(
        update
            .get_result_columns()
            .iter()
            .map(|column| (column.name.as_str(), column.type_oid, column.typmod))
            .collect::<Vec<_>>(),
        vec![("id", 23, -1), ("label", 1043, 14)]
    );
    assert_eq!(
        session
            .query_prepared(&update, &[Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Text("source".into())]]
    );

    let delete = session
        .prepare(
            "DELETE FROM prepared_mutation_target AS target
             USING prepared_mutation_source AS source
             WHERE EXISTS (SELECT 1 WHERE target.id = source.id) AND source.id = $1
             RETURNING target.id, source.label",
        )
        .unwrap();
    assert_eq!(
        session
            .query_prepared(&delete, &[Value::Int4(1)])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1), Value::Text("source".into())]]
    );
}

#[test]
fn matches_coercion_errors() {
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
fn matches_order_by_position_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (1); \
         SELECT id FROM __TABLE__ ORDER BY 0; \
         SELECT id FROM __TABLE__ ORDER BY 2",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_limit_and_offset() {
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
fn matches_limit_and_offset_without_order_by() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         INSERT INTO __TABLE__ VALUES (4), (1), (5), (2), (3); \
         SELECT id FROM __TABLE__ LIMIT 3; \
         SELECT id FROM __TABLE__ OFFSET 3",
        RowOrder::Unordered,
    );
}

#[test]
fn matches_negative_limit_and_offset_errors() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER); \
         SELECT id FROM __TABLE__ LIMIT -1; \
         SELECT id FROM __TABLE__ OFFSET -1",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_not_null_and_defaults() {
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
fn matches_not_null_and_default_errors() {
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
fn matches_primary_and_unique_constraints() {
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
fn matches_check_constraints() {
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
fn matches_unique_value_semantics() {
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
fn matches_delete_visibility_across_sessions() {
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

#[test]
fn matches_query_foundations_and_single_table_aliases() {
    assert_differential(
        "SELECT 2 + 1 AS result ORDER BY result LIMIT 1; \
         VALUES (2), ('1'), (3) ORDER BY column1 LIMIT 2 OFFSET 1; \
         CREATE TABLE __TABLE__ (id INTEGER DEFAULT 7, value TEXT); \
         INSERT INTO __TABLE__ DEFAULT VALUES; \
         INSERT INTO __TABLE__ VALUES (2, 'two'); \
         SELECT item.value AS label, item.* FROM __TABLE__ AS item WHERE item.id = 2 ORDER BY label; \
         SELECT label FROM __TABLE__ AS item(label, label); \
         SELECT missing FROM __TABLE__",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_derived_tables_and_uncorrelated_scalar_subqueries() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, value INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 10), (2, 20), (3, 30); \
         SELECT source.item_id FROM (SELECT id AS item_id FROM __TABLE__ WHERE id > 1) AS source ORDER BY source.item_id; \
         SELECT id FROM __TABLE__ WHERE value < (SELECT 25) ORDER BY (SELECT 100) - id; \
         UPDATE __TABLE__ SET value = (SELECT 99) WHERE id = (SELECT 1); \
         SELECT value FROM __TABLE__ WHERE id = 1; \
         SELECT (SELECT value FROM __TABLE__ WHERE id > 1)",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_uncorrelated_subquery_predicates() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, pair INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 1), (2, 2), (NULL, 3); \
         SELECT EXISTS (SELECT 1 FROM __TABLE__ WHERE id = 1), NOT EXISTS (SELECT 1 FROM __TABLE__ WHERE id = 9); \
         SELECT 1 IN (SELECT id FROM __TABLE__), 3 IN (SELECT id FROM __TABLE__), 3 NOT IN (SELECT id FROM __TABLE__); \
         SELECT id FROM __TABLE__ WHERE id = ANY (SELECT id FROM __TABLE__) ORDER BY id; \
         SELECT 3 > ALL (SELECT id FROM __TABLE__); \
         SELECT (1, 1) IN (SELECT id, pair FROM __TABLE__), (3, 3) IN (SELECT id, pair FROM __TABLE__); \
         UPDATE __TABLE__ SET pair = 20 WHERE id IN (SELECT id FROM __TABLE__ WHERE pair = 2); \
         SELECT pair FROM __TABLE__ WHERE id = 2; \
         SELECT 1 IN (SELECT id, pair FROM __TABLE__)",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_correlated_subqueries() {
    assert_differential(
        "CREATE TABLE __TABLE__ (id INTEGER, threshold INTEGER); \
         CREATE TABLE __TABLE___children (id INTEGER, parent_id INTEGER, value INTEGER); \
         INSERT INTO __TABLE__ VALUES (1, 15), (2, 5), (3, NULL); \
         INSERT INTO __TABLE___children VALUES (10, 1, 10), (11, 1, 20), (12, 2, NULL); \
         SELECT p.id, (SELECT c.value FROM __TABLE___children AS c WHERE c.parent_id = p.id ORDER BY c.id LIMIT 1) FROM __TABLE__ AS p ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p WHERE EXISTS (SELECT 1 FROM __TABLE___children AS c WHERE c.parent_id = p.id AND c.value > p.threshold) ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p WHERE EXISTS (SELECT 1 FROM __TABLE___children AS c WHERE c.parent_id = p.id) ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p WHERE p.id IN (SELECT c.parent_id FROM __TABLE___children AS c WHERE c.value > p.threshold) ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p WHERE p.threshold < ANY (SELECT c.value FROM __TABLE___children AS c WHERE c.parent_id = p.id) ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p WHERE p.threshold < ALL (SELECT c.value FROM __TABLE___children AS c WHERE c.parent_id = p.id) ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p WHERE EXISTS (SELECT 1 FROM __TABLE___children AS c WHERE c.parent_id = p.id AND EXISTS (SELECT 1 WHERE c.value > p.threshold)) ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p JOIN __TABLE___children AS c ON c.parent_id = p.id AND EXISTS (SELECT 1 WHERE c.value > p.threshold) ORDER BY p.id; \
         SELECT p.id FROM __TABLE__ AS p WHERE EXISTS (SELECT 1 FROM __TABLE___children AS p WHERE p.parent_id = p.id) ORDER BY p.id; \
         SELECT (SELECT c.value FROM __TABLE___children AS c WHERE c.parent_id = p.id) FROM __TABLE__ AS p WHERE p.id = 1",
        RowOrder::Ordered,
    );
}
