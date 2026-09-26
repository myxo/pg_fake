use std::{env, io::Read};

use bigdecimal::BigDecimal;
use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Executor, Row, Statement as _, TypeInfo, ValueRef};
use sqlx_postgres::PgConnection;
use tokio::runtime::Runtime;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Affected(u64),
    Rows {
        columns: Vec<(String, String)>,
        rows: Vec<Vec<Option<String>>>,
    },
    Error(String),
}

#[derive(Clone, Copy)]
enum Quote {
    Plain,
    Single { escape: bool },
    Double,
    LineComment,
    BlockComment(usize),
    Dollar(usize, usize),
}

fn split_statements(script: &str) -> Option<Vec<&str>> {
    let bytes = script.as_bytes();
    let mut quote = Quote::Plain;
    let mut start = 0;
    let mut index = 0;
    let mut statements = Vec::new();
    while index < bytes.len() {
        match quote {
            Quote::Plain => match bytes[index] {
                b'\'' => {
                    let escape = index > 0
                        && matches!(bytes[index - 1], b'E' | b'e')
                        && (index == 1 || !bytes[index - 2].is_ascii_alphanumeric());
                    quote = Quote::Single { escape };
                    index += 1;
                }
                b'"' => {
                    quote = Quote::Double;
                    index += 1;
                }
                b'-' if bytes.get(index + 1) == Some(&b'-') => {
                    quote = Quote::LineComment;
                    index += 2;
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    quote = Quote::BlockComment(1);
                    index += 2;
                }
                b'$' => {
                    let mut end = index + 1;
                    while bytes
                        .get(end)
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    {
                        end += 1;
                    }
                    if bytes.get(end) == Some(&b'$') {
                        quote = Quote::Dollar(index, end + 1);
                        index = end + 1;
                    } else {
                        index += 1;
                    }
                }
                b';' => {
                    let statement = script[start..index].trim();
                    if !statement.is_empty() {
                        statements.push(statement);
                    }
                    start = index + 1;
                    index += 1;
                }
                _ => index += 1,
            },
            Quote::Single { escape } => {
                if escape && bytes[index] == b'\\' {
                    index += 2;
                } else if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                    } else {
                        quote = Quote::Plain;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            Quote::Double => {
                if bytes[index] == b'"' {
                    if bytes.get(index + 1) == Some(&b'"') {
                        index += 2;
                    } else {
                        quote = Quote::Plain;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            Quote::LineComment => {
                if bytes[index] == b'\n' {
                    quote = Quote::Plain;
                }
                index += 1;
            }
            Quote::BlockComment(depth) => {
                if bytes[index..].starts_with(b"/*") {
                    quote = Quote::BlockComment(depth + 1);
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    quote = if depth == 1 {
                        Quote::Plain
                    } else {
                        Quote::BlockComment(depth - 1)
                    };
                    index += 2;
                } else {
                    index += 1;
                }
            }
            Quote::Dollar(begin, end) => {
                if bytes[index..].starts_with(&bytes[begin..end]) {
                    quote = Quote::Plain;
                    index += end - begin;
                } else {
                    index += 1;
                }
            }
        }
    }
    if !matches!(quote, Quote::Plain | Quote::LineComment) {
        return None;
    }
    let statement = script[start..].trim();
    if !statement.is_empty() {
        statements.push(statement);
    }
    Some(statements)
}

fn make_error(error: sqlx::Error) -> Outcome {
    Outcome::Error(
        error
            .as_database_error()
            .and_then(|database_error| database_error.code())
            .map(|code| code.into_owned())
            .unwrap_or_else(|| format!("infrastructure: {error}")),
    )
}

fn normalize_rows(rows: &mut [Vec<Option<String>>], columns: &[(String, String)]) {
    for row in rows {
        for (cell, (_, data_type)) in row.iter_mut().zip(columns) {
            let Some(value) = cell else { continue };
            *value = match data_type.as_str() {
                "FLOAT4" => value
                    .parse::<f32>()
                    .map(|number| format!("{:08x}", number.to_bits()))
                    .unwrap_or_else(|_| value.clone()),
                "FLOAT8" => value
                    .parse::<f64>()
                    .map(|number| format!("{:016x}", number.to_bits()))
                    .unwrap_or_else(|_| value.clone()),
                "NUMERIC" => value
                    .parse::<BigDecimal>()
                    .map(|number| number.normalized().to_plain_string())
                    .unwrap_or_else(|_| value.clone()),
                _ => continue,
            };
        }
    }
}

async fn observe_postgres(connection: &mut PgConnection, sql: &str) -> Outcome {
    let prepared = match connection.prepare(sql).await {
        Ok(prepared) => prepared,
        Err(error) => return make_error(error),
    };
    if prepared.columns().is_empty() {
        return match sqlx::raw_sql(sql).execute(connection).await {
            Ok(result) => Outcome::Affected(result.rows_affected()),
            Err(error) => make_error(error),
        };
    }
    let columns = prepared
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    match sqlx::raw_sql(sql).fetch_all(connection).await {
        Ok(rows) => {
            let mut values = rows
                .iter()
                .map(|row| {
                    (0..row.len())
                        .map(|index| {
                            let value = row.try_get_raw(index).unwrap();
                            (!value.is_null())
                                .then(|| row.try_get_unchecked::<String, _>(index).unwrap())
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            normalize_rows(&mut values, &columns);
            values.sort();
            Outcome::Rows {
                columns,
                rows: values,
            }
        }
        Err(error) => make_error(error),
    }
}

async fn observe_fake(connection: &mut PgFakeConnection, sql: &str) -> Outcome {
    let prepared = match connection.prepare(sql).await {
        Ok(prepared) => prepared,
        Err(error) => return make_error(error),
    };
    if prepared.columns().is_empty() {
        return match sqlx::raw_sql(sql).execute(connection).await {
            Ok(result) => Outcome::Affected(result.rows_affected()),
            Err(error) => make_error(error),
        };
    }
    let columns = prepared
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    match sqlx::raw_sql(sql).fetch_all(connection).await {
        Ok(rows) => {
            let mut values = rows
                .iter()
                .map(|row| {
                    (0..row.len())
                        .map(|index| {
                            let value = row.try_get_raw(index).unwrap();
                            (!value.is_null())
                                .then(|| row.try_get_unchecked::<String, _>(index).unwrap())
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            normalize_rows(&mut values, &columns);
            values.sort();
            Outcome::Rows {
                columns,
                rows: values,
            }
        }
        Err(error) => make_error(error),
    }
}

struct PostgresReference {
    runtime: Runtime,
    url: String,
    connection: PgConnection,
}

impl PostgresReference {
    fn connect(url: String) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("must create runtime");
        let connection = runtime.block_on(async {
            PgConnection::connect(&url)
                .await
                .expect("must connect to PostgreSQL reference")
        });
        Self {
            runtime,
            url,
            connection,
        }
    }

    fn check_candidate(&mut self, bytes: &[u8]) -> Option<Vec<(String, Outcome, Outcome)>> {
        let script = std::str::from_utf8(bytes).ok()?;
        if script.len() > 16_384 {
            return None;
        }
        let statements = split_statements(script)?;
        if statements.is_empty() || statements.len() > 32 {
            return None;
        }
        if statements.iter().any(|statement| {
            let upper = statement.trim_start().to_ascii_uppercase();
            [
                "BEGIN",
                "START TRANSACTION",
                "COMMIT",
                "ROLLBACK",
                "SAVEPOINT",
                "RELEASE",
                "END",
            ]
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        }) {
            return None;
        }
        let Self {
            runtime,
            url,
            connection,
        } = self;
        runtime.block_on(async {
            // A previous forkserver child may have died inside its
            // transaction, so always roll back before starting a new one.
            if sqlx::raw_sql("ROLLBACK; BEGIN; SET LOCAL statement_timeout = '2s';")
                .execute(&mut *connection)
                .await
                .is_err()
            {
                *connection = PgConnection::connect(url)
                    .await
                    .expect("must reconnect to PostgreSQL reference");
                sqlx::raw_sql("BEGIN; SET LOCAL statement_timeout = '2s';")
                    .execute(&mut *connection)
                    .await
                    .expect("must begin reference transaction");
            }
            let mut fake = PgFakeConnection::new(Db::create_builder().set_random_seed(0).build());
            sqlx::raw_sql("BEGIN")
                .execute(&mut fake)
                .await
                .expect("must begin fake transaction");
            let mut observations = Vec::new();
            for statement in statements {
                let expected = observe_postgres(connection, statement).await;
                let actual = observe_fake(&mut fake, statement).await;
                observations.push((statement.to_owned(), expected, actual));
            }
            let _ = sqlx::raw_sql("ROLLBACK").execute(&mut *connection).await;
            let _ = sqlx::raw_sql("DISCARD ALL").execute(&mut *connection).await;
            Some(observations)
        })
    }
}

#[cfg(fuzzing)]
unsafe extern "C" {
    fn __afl_manual_init();
}

fn main() {
    let url = env::var("PG_FAKE_SQUIRREL_POSTGRES_URL")
        .expect("PG_FAKE_SQUIRREL_POSTGRES_URL must point to PostgreSQL 18");
    // Seed RandomState before the fork point, otherwise every forkserver
    // child reseeds and hash iteration order differs between runs.
    let mut seed_warmup = std::collections::HashMap::new();
    seed_warmup.insert(0u8, 0u8);
    std::hint::black_box(seed_warmup.get(&0));
    let mut reference = PostgresReference::connect(url);
    #[cfg(fuzzing)]
    unsafe {
        __afl_manual_init();
    }
    if let Some(script) = env::args().nth(1) {
        let observations = reference.check_candidate(script.as_bytes());
        println!("{observations:?}");
        let mismatch = observations.and_then(|observations| {
            observations
                .into_iter()
                .find(|(_, expected, actual)| actual != expected)
        });
        if let Some((sql, expected, actual)) = mismatch {
            eprintln!("MISMATCH\t{sql}\t{expected:?}\t{actual:?}");
            std::process::exit(1);
        }
        return;
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .expect("must read AFL input");
    if let Some(observations) = reference.check_candidate(&bytes) {
        for (sql, expected, actual) in observations {
            if actual != expected {
                eprintln!("SQL: {sql}\nPostgreSQL: {expected:?}\npg_fake: {actual:?}");
                std::process::abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_statements;

    #[test]
    fn splits_scripts_without_splitting_quoted_semicolons() {
        for expression in ["'a;b'", "\"a;b\"", "$tag$a;b$tag$", "E'a\\';b'"] {
            let script = format!("SELECT {expression}; SELECT 2");
            assert_eq!(split_statements(&script).unwrap().len(), 2);
        }
        assert_eq!(
            split_statements("SELECT 1 /* outer; /* inner; */ end; */; SELECT 2")
                .unwrap()
                .len(),
            2
        );
    }
}
