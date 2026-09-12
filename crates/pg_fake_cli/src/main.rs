use std::{
    env, fs,
    io::{self, BufRead, Write},
    path::PathBuf,
    process,
};

use pg_fake::{Db, QueryResult, Session, StatementResult, error::PgError, value::Value};

enum Command {
    Repl,
    File(PathBuf),
}

enum SqlInputState {
    Normal,
    SingleQuoted,
    DoubleQuoted,
    LineComment,
    BlockComment(usize),
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    match parse_command()? {
        Command::Repl => run_repl(),
        Command::File(path) => run_file(&path),
    }
}

fn parse_command() -> Result<Command, String> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(Command::Repl),
        [path] => Ok(Command::File(PathBuf::from(path))),
        _ => Err("usage: pg_fake_cli [sql-file]".into()),
    }
}

fn run_file(path: &PathBuf) -> Result<(), String> {
    let sql = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut session = Db::create().create_session();
    run_sql(&mut io::stdout().lock(), &mut session, &sql)
}

fn run_repl() -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    let mut session = Db::create().create_session();
    let mut sql = String::new();

    writeln!(output, "pg_fake CLI")
        .map_err(|error| format!("could not write to stdout: {error}"))?;
    loop {
        write!(
            output,
            "{}",
            if sql.is_empty() {
                "pg_fake=> "
            } else {
                "        -> "
            }
        )
        .map_err(|error| format!("could not write to stdout: {error}"))?;
        output
            .flush()
            .map_err(|error| format!("could not write to stdout: {error}"))?;

        let mut line = String::new();
        let read = input
            .read_line(&mut line)
            .map_err(|error| format!("could not read from stdin: {error}"))?;
        if read == 0 {
            if !sql.trim().is_empty() {
                eprintln!("error: incomplete SQL statement discarded");
            }
            writeln!(output).map_err(|error| format!("could not write to stdout: {error}"))?;
            return Ok(());
        }

        if sql.is_empty() && matches!(line.trim(), "\\q" | "\\quit") {
            return Ok(());
        }

        sql.push_str(&line);
        if !has_complete_sql_statement(&sql) {
            continue;
        }

        if let Err(error) = run_sql(&mut output, &mut session, &sql) {
            writeln!(output, "{error}")
                .map_err(|error| format!("could not write to stdout: {error}"))?;
        }
        sql.clear();
    }
}

fn run_sql(output: &mut impl Write, session: &mut Session, sql: &str) -> Result<(), String> {
    let results = session
        .execute(sql)
        .map_err(|error| format_sql_error(&error))?;
    write_results(output, &results)
}

fn has_complete_sql_statement(sql: &str) -> bool {
    let mut state = SqlInputState::Normal;
    let bytes = sql.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match state {
            SqlInputState::Normal => match bytes[index] {
                b'\'' => state = SqlInputState::SingleQuoted,
                b'"' => state = SqlInputState::DoubleQuoted,
                b'-' if bytes.get(index + 1) == Some(&b'-') => {
                    state = SqlInputState::LineComment;
                    index += 1;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    state = SqlInputState::BlockComment(1);
                    index += 1;
                }
                b';' => return true,
                _ => {}
            },
            SqlInputState::SingleQuoted => {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 1;
                    } else {
                        state = SqlInputState::Normal;
                    }
                }
            }
            SqlInputState::DoubleQuoted => {
                if bytes[index] == b'"' {
                    if bytes.get(index + 1) == Some(&b'"') {
                        index += 1;
                    } else {
                        state = SqlInputState::Normal;
                    }
                }
            }
            SqlInputState::LineComment => {
                if bytes[index] == b'\n' {
                    state = SqlInputState::Normal;
                }
            }
            SqlInputState::BlockComment(depth) => {
                if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    state = SqlInputState::BlockComment(depth + 1);
                    index += 1;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    state = if depth == 1 {
                        SqlInputState::Normal
                    } else {
                        SqlInputState::BlockComment(depth - 1)
                    };
                    index += 1;
                }
            }
        }
        index += 1;
    }
    false
}

fn write_results(output: &mut impl Write, results: &[StatementResult]) -> Result<(), String> {
    for result in results {
        match result {
            StatementResult::Affected(rows) => {
                writeln!(
                    output,
                    "{} {} affected",
                    rows,
                    if *rows == 1 { "row" } else { "rows" }
                )
            }
            StatementResult::Query(result) => write_query_result(output, result),
        }
        .map_err(|error| format!("could not write to stdout: {error}"))?;
    }
    Ok(())
}

fn write_query_result(output: &mut impl Write, result: &QueryResult) -> io::Result<()> {
    let mut rows = result
        .rows
        .iter()
        .map(|row| row.iter().map(format_value).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let mut widths = result
        .columns
        .iter()
        .map(|column| column.name.chars().count())
        .collect::<Vec<_>>();

    for row in &rows {
        for (width, value) in widths.iter_mut().zip(row) {
            *width = (*width).max(value.chars().count());
        }
    }

    for (index, column) in result.columns.iter().enumerate() {
        if index > 0 {
            write!(output, " | ")?;
        }
        write!(output, "{:width$}", column.name, width = widths[index])?;
    }
    writeln!(output)?;

    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            write!(output, "-+-")?;
        }
        write!(output, "{}", "-".repeat(*width))?;
    }
    writeln!(output)?;

    for row in rows.drain(..) {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                write!(output, " | ")?;
            }
            write!(output, "{:width$}", value, width = widths[index])?;
        }
        writeln!(output)?;
    }

    writeln!(
        output,
        "({} {})",
        result.rows.len(),
        if result.rows.len() == 1 {
            "row"
        } else {
            "rows"
        }
    )
}

fn format_value(value: &Value) -> String {
    if value.is_null() {
        "NULL".into()
    } else {
        value
            .format_postgres_text()
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    }
}

fn format_sql_error(error: &PgError) -> String {
    format!("ERROR [{}]: {}", error.sqlstate, error.message)
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
