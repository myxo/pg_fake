use std::str::FromStr;

use bigdecimal::BigDecimal;
use pg_fake::parser::{self, Statement};
use pg_fake_sqlx::{PgFake, PgFakeConnection};
#[cfg(test)]
use sqlx::Connection;
use sqlx::{
    AssertSqlSafe, Column, ColumnIndex, Database, Decode, Executor, Row, Type, TypeInfo, ValueRef,
};
use sqlx_postgres::{PgConnection, Postgres};
use tokio::runtime::Runtime;

#[cfg(test)]
use super::common;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Affected(u64),
    Rows(Vec<Vec<Option<String>>>),
    Error(String),
}

#[derive(Clone, Copy)]
pub(super) enum RowOrder {
    Unordered,
    Ordered,
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

#[cfg(test)]
pub(super) struct IsolatedPostgresServer {
    pub(super) url: String,
    database: String,
    connection: PgConnection,
    runtime: Runtime,
    _server: common::PostgresServer,
}

#[cfg(test)]
impl Drop for IsolatedPostgresServer {
    fn drop(&mut self) {
        let sql = format!("DROP DATABASE {} WITH (FORCE)", self.database);
        let _ = self
            .runtime
            .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut self.connection));
    }
}

#[cfg(test)]
pub(super) fn start_isolated_postgres_server() -> IsolatedPostgresServer {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("must create PostgreSQL setup runtime");
    let mut connection = runtime
        .block_on(PgConnection::connect(&server.url))
        .expect("must connect to PostgreSQL for differential-test setup");
    let backend = runtime
        .block_on(
            sqlx::query_scalar::<_, i32>(AssertSqlSafe("SELECT pg_backend_pid()"))
                .fetch_one(&mut connection),
        )
        .expect("must identify differential-test setup connection");
    let database = format!("pg_fake_differential_{}_{backend}", std::process::id());
    let mut url = url::Url::parse(&server.url).expect("must parse PostgreSQL test URL");
    url.set_path(&database);
    let sql = format!("CREATE DATABASE {database} TEMPLATE template0");
    runtime
        .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut connection))
        .expect("must create isolated differential-test database");
    IsolatedPostgresServer {
        url: url.into(),
        database,
        connection,
        runtime,
        _server: server,
    }
}

enum TestConnection<'connection> {
    Fake(&'connection mut PgFakeConnection),
    Postgres(&'connection mut PgConnection),
}

impl TestConnection<'_> {
    fn execute(&mut self, runtime: &Runtime, statement: &Statement, sql: &str) -> Outcome {
        match self {
            Self::Fake(connection) => runtime.block_on(execute_sqlx::<PgFake>(
                connection,
                statement,
                sql,
                |result| result.rows_affected(),
            )),
            Self::Postgres(connection) => runtime.block_on(execute_sqlx::<Postgres>(
                connection,
                statement,
                sql,
                |result| result.rows_affected(),
            )),
        }
    }
}

async fn execute_sqlx<DB>(
    connection: &mut DB::Connection,
    statement: &Statement,
    sql: &str,
    rows_affected: impl FnOnce(DB::QueryResult) -> u64,
) -> Outcome
where
    DB: Database,
    for<'connection> &'connection mut DB::Connection: Executor<'connection, Database = DB>,
    for<'row> String: Decode<'row, DB> + Type<DB>,
    usize: ColumnIndex<DB::Row>,
{
    if returns_rows(statement) {
        match sqlx::raw_sql(AssertSqlSafe(sql))
            .fetch_all(&mut *connection)
            .await
        {
            Ok(rows) => {
                let column_types = rows
                    .first()
                    .map(|row| {
                        row.columns()
                            .iter()
                            .map(|column| column.type_info().name().to_owned())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut values = rows
                    .iter()
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
                    .collect::<Vec<_>>();
                normalize_rows(&mut values, &column_types);
                Outcome::Rows(values)
            }
            Err(error) => make_error_outcome(error),
        }
    } else {
        match sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&mut *connection)
            .await
        {
            Ok(result) => Outcome::Affected(rows_affected(result)),
            Err(error) => make_error_outcome(error),
        }
    }
}

fn make_error_outcome(error: sqlx::Error) -> Outcome {
    Outcome::Error(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .expect("database execution errors must have a SQLSTATE")
            .into_owned(),
    )
}

pub(super) fn assert_statement(
    runtime: &Runtime,
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
    sql: &str,
    row_order: RowOrder,
) {
    assert_statement_outcome(runtime, postgres, fake, sql, row_order, false);
}

pub(super) fn assert_statement_allow_error(
    runtime: &Runtime,
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
    sql: &str,
    row_order: RowOrder,
) {
    assert_statement_outcome(runtime, postgres, fake, sql, row_order, true);
}

fn assert_statement_outcome(
    runtime: &Runtime,
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
    sql: &str,
    row_order: RowOrder,
    allow_error: bool,
) {
    let mut statements =
        parser::parse(sql).unwrap_or_else(|error| panic!("SQL must parse: {sql}\n{error}"));
    assert_eq!(statements.len(), 1, "operation must be one statement");
    let statement = statements.pop().expect("statement count was checked");
    let [expected, actual] = [
        TestConnection::Postgres(postgres),
        TestConnection::Fake(fake),
    ]
    .map(|mut connection| connection.execute(runtime, &statement, sql));
    if !allow_error && let Outcome::Error(sqlstate) = &expected {
        panic!("unexpected PostgreSQL error ({sqlstate}): {sql}");
    }
    match (expected, actual) {
        (Outcome::Rows(mut expected), Outcome::Rows(mut actual)) => {
            if matches!(row_order, RowOrder::Unordered) {
                expected.sort();
                actual.sort();
            }
            assert_eq!(actual, expected, "SQL: {sql}");
        }
        (expected, actual) => assert_eq!(actual, expected, "SQL: {sql}"),
    }
}

fn normalize_rows(rows: &mut [Vec<Option<String>>], column_types: &[String]) {
    for row in rows {
        assert_eq!(row.len(), column_types.len());
        for (value, column_type) in row.iter_mut().zip(column_types) {
            let Some(value) = value else {
                continue;
            };
            *value = match column_type.as_str() {
                "FLOAT4" => format!("{:08x}", value.parse::<f32>().unwrap().to_bits()),
                "FLOAT8" => format!("{:016x}", value.parse::<f64>().unwrap().to_bits()),
                "NUMERIC" => BigDecimal::from_str(value)
                    .unwrap()
                    .normalized()
                    .to_plain_string(),
                _ => continue,
            };
        }
    }
}
