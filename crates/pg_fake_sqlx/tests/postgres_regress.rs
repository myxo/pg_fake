use std::{fs, path::PathBuf, sync::Mutex};

use pg_fake::parser::{self, Statement};
use pg_fake_sqlx::{Db, PgFake, PgFakeConnection};
use sqlx::{
    AssertSqlSafe, ColumnIndex, Connection, Database, Decode, Executor, IntoArguments, Row, Type,
    ValueRef,
};
use sqlx_postgres::{PgConnection, Postgres};
use tokio::runtime::Runtime;
use url::Url;

mod common;

#[path = "postgres_regress/phase2_manifest.rs"]
mod phase2_manifest;

use common::start_postgres_server;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Affected(u64),
    Rows(Vec<Vec<Option<String>>>),
    Error(String),
}

static TEST_LOCK: Mutex<()> = Mutex::new(());

const MINIMUM_PASSED_STATEMENTS: usize = 463;
const REVIEWED_SKIPPED_SCRIPTS: usize = 141;

enum TestConnection<'connection> {
    Fake(&'connection mut PgFakeConnection),
    Postgres(&'connection mut PgConnection),
}

#[derive(Clone, Copy)]
enum ExecutionMode {
    Prepared,
    Raw,
}

impl TestConnection<'_> {
    fn execute(&mut self, runtime: &Runtime, statement: &Statement, sql: &str) -> Outcome {
        match self {
            Self::Fake(connection) => runtime.block_on(execute_sqlx::<PgFake>(
                connection,
                statement,
                sql,
                ExecutionMode::Prepared,
                |result| result.rows_affected(),
            )),
            Self::Postgres(connection) => runtime.block_on(execute_sqlx::<Postgres>(
                connection,
                statement,
                sql,
                ExecutionMode::Raw,
                |result| result.rows_affected(),
            )),
        }
    }
}

async fn execute_sqlx<DB>(
    connection: &mut DB::Connection,
    statement: &Statement,
    sql: &str,
    mode: ExecutionMode,
    rows_affected: impl FnOnce(DB::QueryResult) -> u64,
) -> Outcome
where
    DB: Database,
    for<'connection> &'connection mut DB::Connection: Executor<'connection, Database = DB>,
    for<'row> String: Decode<'row, DB> + Type<DB>,
    DB::Arguments: IntoArguments<DB>,
    usize: ColumnIndex<DB::Row>,
{
    match statement {
        Statement::Query(_) => match match mode {
            ExecutionMode::Prepared => {
                sqlx::query(AssertSqlSafe(sql))
                    .fetch_all(&mut *connection)
                    .await
            }
            ExecutionMode::Raw => {
                sqlx::raw_sql(AssertSqlSafe(sql))
                    .fetch_all(&mut *connection)
                    .await
            }
        } {
            Ok(rows) => Outcome::Rows(
                rows.iter()
                    .map(|row| {
                        (0..row.len())
                            .map(|index| {
                                let value = row.try_get_raw(index).unwrap();
                                if value.is_null() {
                                    None
                                } else {
                                    Some(row.try_get_unchecked::<String, _>(index).unwrap())
                                }
                            })
                            .collect()
                    })
                    .collect(),
            ),
            Err(error) => make_error_outcome(error),
        },
        _ => match match mode {
            ExecutionMode::Prepared => {
                sqlx::query(AssertSqlSafe(sql))
                    .execute(&mut *connection)
                    .await
            }
            ExecutionMode::Raw => {
                sqlx::raw_sql(AssertSqlSafe(sql))
                    .execute(&mut *connection)
                    .await
            }
        } {
            Ok(result) => Outcome::Affected(rows_affected(result)),
            Err(error) => make_error_outcome(error),
        },
    }
}

fn make_error_outcome(error: sqlx::Error) -> Outcome {
    Outcome::Error(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .map(|code| code.into_owned())
            .unwrap_or_else(|| error.to_string()),
    )
}

fn database_url(url: &str, database: &str) -> String {
    let mut url = Url::parse(url).expect("PostgreSQL URL must parse");
    url.set_path(&format!("/{database}"));
    url.into()
}

fn source_sql(script: &str) -> Result<String, String> {
    let mut result = String::new();
    for line in script.lines() {
        let command = line.trim_start();
        if command.starts_with("\\pset") || command.starts_with("\\echo") {
            continue;
        }
        if command.starts_with('\\') {
            return Err("requires a psql meta-command".into());
        }
        result.push_str(line);
        result.push('\n');
    }
    Ok(result)
}

fn statements(script: &str) -> Vec<String> {
    enum State {
        Plain,
        Single,
        Double,
        LineComment,
        BlockComment(usize),
        Dollar(String),
    }

    let bytes = script.as_bytes();
    let mut state = State::Plain;
    let mut start = 0;
    let mut index = 0;
    let mut result = Vec::new();

    while index < bytes.len() {
        match &mut state {
            State::Plain => match bytes[index] {
                b'\'' => {
                    state = State::Single;
                    index += 1;
                }
                b'"' => {
                    state = State::Double;
                    index += 1;
                }
                b'-' if bytes.get(index + 1) == Some(&b'-') => {
                    state = State::LineComment;
                    index += 2;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    state = State::BlockComment(1);
                    index += 2;
                }
                b'$' => {
                    let mut end = index + 1;
                    while matches!(
                        bytes.get(end),
                        Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
                    ) {
                        end += 1;
                    }
                    if bytes.get(end) == Some(&b'$') {
                        state = State::Dollar(script[index..=end].into());
                        index = end + 1;
                    } else {
                        index += 1;
                    }
                }
                b';' => {
                    let statement = script[start..index].trim();
                    if !statement.is_empty() {
                        result.push(statement.into());
                    }
                    start = index + 1;
                    index += 1;
                }
                _ => index += 1,
            },
            State::Single => {
                if bytes[index] == b'\\' {
                    index += 2;
                } else if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                    } else {
                        state = State::Plain;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            State::Double => {
                if bytes[index] == b'"' {
                    if bytes.get(index + 1) == Some(&b'"') {
                        index += 2;
                    } else {
                        state = State::Plain;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            State::LineComment => {
                if bytes[index] == b'\n' {
                    state = State::Plain;
                }
                index += 1;
            }
            State::BlockComment(depth) => {
                if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    *depth += 1;
                    index += 2;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    *depth -= 1;
                    if *depth == 0 {
                        state = State::Plain;
                    }
                    index += 2;
                } else {
                    index += 1;
                }
            }
            State::Dollar(delimiter) => {
                if script[index..].starts_with(delimiter.as_str()) {
                    index += delimiter.len();
                    state = State::Plain;
                } else {
                    index += 1;
                }
            }
        }
    }

    let statement = script[start..].trim();
    if !statement.is_empty() {
        result.push(statement.into());
    }
    result
}

fn statement_is_stateful(sql: &str) -> bool {
    let sql = sql.trim_start().to_ascii_uppercase();
    !sql.starts_with("SELECT") && !sql.starts_with("VALUES") && !sql.starts_with("SHOW")
}

fn compare_source_statement(
    runtime: &Runtime,
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
    sql: &str,
) -> Result<(), String> {
    let normalized = sql.to_ascii_uppercase();
    if normalized.contains("COPY") && normalized.contains("FROM STDIN") {
        return Err("requires inline COPY fixture data".into());
    }
    let mut parsed = match parser::parse(sql) {
        Ok(parsed) if parsed.len() == 1 => parsed,
        Ok(_) => return Err("does not contain exactly one SQL statement".into()),
        Err(fake_error) => {
            match runtime.block_on(sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut *postgres)) {
                Err(postgres_error)
                    if postgres_error
                        .as_database_error()
                        .and_then(|error| error.code())
                        .is_some_and(|code| code == fake_error.sqlstate.get_code()) =>
                {
                    return Ok(());
                }
                Err(postgres_error) => {
                    return Err(format!(
                        "pg_fake cannot parse it ({}) while PostgreSQL returns {}",
                        fake_error.sqlstate.get_code(),
                        postgres_error
                            .as_database_error()
                            .and_then(|error| error.code())
                            .as_deref()
                            .unwrap_or("no SQLSTATE")
                    ));
                }
                Ok(_) => return Err("pg_fake cannot parse a PostgreSQL-valid statement".into()),
            }
        }
    };
    let statement = parsed.pop().unwrap();
    let [expected, actual] = [
        TestConnection::Postgres(postgres),
        TestConnection::Fake(fake),
    ]
    .map(|mut connection| connection.execute(runtime, &statement, sql));
    if actual == expected {
        Ok(())
    } else {
        Err(format!("PostgreSQL: {expected:?}; pg_fake: {actual:?}"))
    }
}

fn collect_phase2_report(
    runtime: &Runtime,
    admin: &mut PgConnection,
    server_url: &str,
) -> (usize, Vec<String>, Vec<String>) {
    let database = format!("pg_fake_regress_phase2_{}", std::process::id());
    let sql = format!("CREATE DATABASE {database}");
    runtime
        .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut *admin))
        .expect("must create PostgreSQL Phase 2 regression database");
    let database_url = database_url(server_url, &database);
    let mut postgres = runtime
        .block_on(PgConnection::connect(&database_url))
        .expect("must connect to PostgreSQL Phase 2 regression database");
    let mut passed = 0;
    let mut blockers = Vec::new();
    let mut regressions = Vec::new();

    for feature in phase2_manifest::FEATURES {
        let mut fake = PgFakeConnection::new(Db::create());
        let mut first_blocker = None;
        for case in feature.cases {
            let mut result = Ok(());
            for setup in case.setup {
                result = compare_source_statement(runtime, &mut postgres, &mut fake, setup);
                if result.is_err() {
                    break;
                }
            }
            if result.is_ok() {
                result = compare_source_statement(runtime, &mut postgres, &mut fake, case.sql);
            }
            match result {
                Ok(()) => passed += 1,
                Err(error) => {
                    if matches!(case.baseline, phase2_manifest::Baseline::MustPass) {
                        regressions.push(format!("{}:{}: {error}", feature.name, case.id));
                    }
                    if first_blocker.is_none() {
                        first_blocker = Some(format!(
                            "{} ({}) at {}: {error}",
                            case.id, feature.name, case.source
                        ));
                    }
                }
            }
        }
        blockers.push(first_blocker.unwrap_or_else(|| format!("{}: none", feature.name)));
    }

    drop(postgres);
    let sql = format!("DROP DATABASE {database} WITH (FORCE)");
    runtime
        .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut *admin))
        .expect("must drop PostgreSQL Phase 2 regression database");
    (passed, blockers, regressions)
}

#[test]
fn reports_phase2_regression_progress() {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/postgres_regress/upstream");
    let mut paths = fs::read_dir(directory)
        .expect("must read PostgreSQL regression SQL directory")
        .map(|entry| {
            entry
                .expect("must read PostgreSQL regression SQL entry")
                .path()
        })
        .collect::<Vec<_>>();
    paths.sort();
    assert!(paths.len() >= 100);
    assert!(
        paths
            .iter()
            .all(|path| path.extension().is_some_and(|extension| extension == "sql"))
    );
    let server = start_postgres_server();
    let runtime = Runtime::new().expect("must create tokio runtime");
    let mut admin = runtime
        .block_on(PgConnection::connect(&server.url))
        .expect("must connect SQLx to PostgreSQL");
    let mut passed = 0;
    let mut skipped = Vec::new();

    for (index, path) in paths.iter().enumerate() {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let script = match fs::read_to_string(path) {
            Ok(script) => script,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                skipped.push((
                    format!("{name}:encoding"),
                    "requires a non-UTF-8 client encoding".to_owned(),
                ));
                continue;
            }
            Err(error) => panic!("must read PostgreSQL regression SQL: {error}"),
        };
        let script = match source_sql(&script) {
            Ok(script) => script,
            Err(error) => {
                skipped.push((format!("{name}:psql"), error));
                continue;
            }
        };

        let database = format!("pg_fake_regress_source_{}_{}", std::process::id(), index);
        let sql = format!("CREATE DATABASE {database}");
        runtime
            .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut admin))
            .expect("must create PostgreSQL regression database");
        let database_url = database_url(&server.url, &database);
        let mut postgres = runtime
            .block_on(PgConnection::connect(&database_url))
            .expect("must connect to PostgreSQL regression database");
        let mut fake = PgFakeConnection::new(Db::create());
        let mut tainted = false;
        let mut first_blocker = None;
        for (statement_number, statement) in statements(&script).into_iter().enumerate() {
            if tainted {
                continue;
            }
            match compare_source_statement(&runtime, &mut postgres, &mut fake, &statement) {
                Ok(()) => passed += 1,
                Err(error) => {
                    if first_blocker.is_none() {
                        first_blocker = Some((
                            format!("{name}:{}", statement_number + 1),
                            format!("statement {}: {error}", statement_number + 1),
                        ));
                    }
                    tainted = statement_is_stateful(&statement);
                }
            }
        }
        drop(postgres);
        let sql = format!("DROP DATABASE {database} WITH (FORCE)");
        runtime
            .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut admin))
            .expect("must drop PostgreSQL regression database");

        if let Some(blocker) = first_blocker {
            skipped.push(blocker);
        }
    }

    assert!(
        passed >= MINIMUM_PASSED_STATEMENTS,
        "full corpus regressed below the reviewed baseline of {MINIMUM_PASSED_STATEMENTS}: {passed} statements passed"
    );
    assert_eq!(
        skipped.len(),
        REVIEWED_SKIPPED_SCRIPTS,
        "full corpus skipped-script count changed from the reviewed baseline"
    );
    let (phase2_passed, phase2_blockers, phase2_regressions) =
        collect_phase2_report(&runtime, &mut admin, &server.url);
    eprintln!("PostgreSQL behavioral statements passed: {passed}");
    eprintln!("PostgreSQL behavioral scripts skipped: {}", skipped.len());
    eprintln!("Phase 2 conformance cases passed: {phase2_passed}");
    for (name, reason) in &skipped {
        eprintln!("SKIP {name}: {reason}");
    }
    for blocker in phase2_blockers {
        eprintln!("PHASE2 BLOCKER {blocker}");
    }
    assert!(
        phase2_regressions.is_empty(),
        "reviewed Phase 2 cases regressed:\n{}",
        phase2_regressions.join("\n")
    );
    let mut expected_skipped = include_str!("postgres_regress/SKIPPED.txt")
        .lines()
        .collect::<Vec<_>>();
    assert_eq!(
        expected_skipped.len(),
        REVIEWED_SKIPPED_SCRIPTS,
        "reviewed skip manifest must retain the baseline script count"
    );
    let mut actual_skipped = skipped
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    expected_skipped.sort();
    actual_skipped.sort();
    assert_eq!(actual_skipped, expected_skipped);
}
