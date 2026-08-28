use std::{
    fs::{self, File},
    path::Path,
    process,
};

use pg_fake::api::Db;
use tracing_subscriber::fmt::format::FmtSpan;

struct QueryCase {
    name: &'static str,
    sql: &'static str,
}

const FIXTURE_SQL: &str = "
    CREATE TABLE users (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        active BOOLEAN NOT NULL DEFAULT true,
        score INTEGER NOT NULL DEFAULT 0,
        manager_id INTEGER
    );
    CREATE TABLE teams (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL
    );
    CREATE TABLE memberships (
        user_id INTEGER NOT NULL,
        team_id INTEGER NOT NULL
    );
    INSERT INTO users (id, name, active, score, manager_id) VALUES
        (1, 'Ada', true, 10, NULL),
        (2, 'Ben', true, 15, 1),
        (3, 'Cora', false, 5, NULL);
    INSERT INTO teams (id, name) VALUES
        (1, 'Platform'),
        (2, 'Research');
    INSERT INTO memberships (user_id, team_id) VALUES
        (1, 1),
        (2, 2);
";

const QUERY_CASES: &[QueryCase] = &[
    QueryCase {
        name: "insert_one",
        sql: "INSERT INTO users (id, name, active, score) VALUES (4, 'Dora', true, 17)",
    },
    QueryCase {
        name: "insert_default",
        sql: "INSERT INTO users (id, name) VALUES (5, 'Eve')",
    },
    QueryCase {
        name: "insert_many",
        sql: "INSERT INTO users (id, name, active, score) VALUES (6, 'Finn', true, 3), (7, 'Gina', false, 8)",
    },
    QueryCase {
        name: "update_one",
        sql: "UPDATE users SET score = 20 WHERE id = 1",
    },
    QueryCase {
        name: "update_expression",
        sql: "UPDATE users SET score = score + 1 WHERE active = true",
    },
    QueryCase {
        name: "update_no_rows",
        sql: "UPDATE users SET score = 0 WHERE id = 999",
    },
    QueryCase {
        name: "delete_one",
        sql: "DELETE FROM users WHERE id = 3",
    },
    QueryCase {
        name: "delete_many",
        sql: "DELETE FROM users WHERE active = false",
    },
    QueryCase {
        name: "select_filter",
        sql: "SELECT id, name FROM users WHERE active = true",
    },
    QueryCase {
        name: "select_expression",
        sql: "SELECT id, score + 1 AS next_score FROM users",
    },
    QueryCase {
        name: "select_order_limit",
        sql: "SELECT id, name FROM users ORDER BY score DESC LIMIT 2",
    },
    QueryCase {
        name: "select_null",
        sql: "SELECT id FROM users WHERE manager_id IS NULL",
    },
    QueryCase {
        name: "join_inner",
        sql: "SELECT u.name, t.name FROM users u JOIN memberships m ON m.user_id = u.id JOIN teams t ON t.id = m.team_id",
    },
    QueryCase {
        name: "join_inner_filter",
        sql: "SELECT u.name, t.name FROM users u JOIN memberships m ON m.user_id = u.id JOIN teams t ON t.id = m.team_id WHERE u.active = true",
    },
    QueryCase {
        name: "join_left",
        sql: "SELECT u.name, t.name FROM users u LEFT JOIN memberships m ON m.user_id = u.id LEFT JOIN teams t ON t.id = m.team_id",
    },
    QueryCase {
        name: "join_cross",
        sql: "SELECT u.name, t.name FROM users u CROSS JOIN teams t",
    },
];

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let trace_directory = Path::new("traces/simple_queries");
    fs::create_dir_all(trace_directory)
        .map_err(|error| format!("could not create {}: {error}", trace_directory.display()))?;

    for query_case in QUERY_CASES {
        write_query_trace(trace_directory, query_case)?;
    }

    Ok(())
}

fn write_query_trace(trace_directory: &Path, query_case: &QueryCase) -> Result<(), String> {
    let mut session = Db::create().create_session();
    session
        .execute(FIXTURE_SQL)
        .map_err(|error| format!("could not prepare {} fixture: {error}", query_case.name))?;

    let trace_path = trace_directory.join(format!("{}.log", query_case.name));
    let trace_file = File::create(&trace_path)
        .map_err(|error| format!("could not create {}: {error}", trace_path.display()))?;
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_span_events(FmtSpan::ENTER | FmtSpan::EXIT)
        .with_writer(trace_file)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            query_case = query_case.name,
            sql = query_case.sql,
            "TRACE QUERY START"
        );
        session.execute(query_case.sql)
    })
    .map(|_| ())
    .map_err(|error| format!("could not execute {}: {error}", query_case.name))
}
