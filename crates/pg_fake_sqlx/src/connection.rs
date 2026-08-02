use std::{
    collections::HashMap,
    fmt,
    future::Future,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use either::Either;
use futures_core::{future::BoxFuture, stream::BoxStream};
use futures_util::{TryStreamExt, stream};
use log::LevelFilter;
use pg_fake::api::{Db, Session, Statement as CoreStatement, StatementResult};
use pg_fake::value::BaseType;
use sqlx::{
    ColumnIndex, ConnectOptions, Connection, Database, Execute, Executor, SqlStr, Statement,
    database::HasStatementCache,
};
use sqlx_core::{connection::LogSettings, transaction::TransactionManager};
use url::Url;

use crate::{
    PgFakeArguments, PgFakeColumn, PgFakeRow, PgFakeTypeInfo, PgFakeValue, error::database_error,
};

#[derive(Debug)]
pub struct PgFake;

impl Database for PgFake {
    type Connection = PgFakeConnection;
    type TransactionManager = PgFakeTransactionManager;
    type Row = PgFakeRow;
    type QueryResult = PgFakeQueryResult;
    type Column = PgFakeColumn;
    type TypeInfo = PgFakeTypeInfo;
    type Value = PgFakeValue;
    type ValueRef<'r> = crate::PgFakeValueRef<'r>;
    type Arguments = PgFakeArguments;
    type ArgumentBuffer = Vec<pg_fake::value::Value>;
    type Statement = PgFakeStatement;

    const NAME: &'static str = "pg_fake";
    const URL_SCHEMES: &'static [&'static str] = &["pg-fake"];
}

impl HasStatementCache for PgFake {}

pub type PgFakePool = sqlx::Pool<PgFake>;
pub type PgFakePoolOptions = sqlx::pool::PoolOptions<PgFake>;

#[derive(Debug, Default)]
pub struct PgFakeQueryResult {
    rows_affected: u64,
}

impl PgFakeQueryResult {
    pub fn rows_affected(&self) -> u64 {
        self.rows_affected
    }
}

impl Extend<PgFakeQueryResult> for PgFakeQueryResult {
    fn extend<T: IntoIterator<Item = PgFakeQueryResult>>(&mut self, results: T) {
        self.rows_affected += results
            .into_iter()
            .map(|result| result.rows_affected)
            .sum::<u64>();
    }
}

#[derive(Clone)]
pub struct PgFakeConnectOptions {
    db: Db,
    log_settings: LogSettings,
}

impl PgFakeConnectOptions {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            log_settings: LogSettings::default(),
        }
    }
}

impl fmt::Debug for PgFakeConnectOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PgFakeConnectOptions")
            .field("log_settings", &self.log_settings)
            .finish_non_exhaustive()
    }
}

impl FromStr for PgFakeConnectOptions {
    type Err = sqlx::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = Url::parse(value).map_err(sqlx::Error::config)?;
        Self::from_url(&url)
    }
}

impl ConnectOptions for PgFakeConnectOptions {
    type Connection = PgFakeConnection;

    fn from_url(url: &Url) -> Result<Self, sqlx::Error> {
        if url.scheme() != "pg-fake" {
            return Err(sqlx::Error::Configuration(
                format!("expected pg-fake URL, got {}", url.scheme()).into(),
            ));
        }
        Ok(Self::new(Db::new()))
    }

    fn to_url_lossy(&self) -> Url {
        Url::parse("pg-fake://localhost").expect("constant pg-fake URL must parse")
    }

    fn connect(&self) -> impl Future<Output = Result<PgFakeConnection, sqlx::Error>> + Send + '_ {
        let connection = PgFakeConnection::new(self.db.clone());
        async move { Ok(connection) }
    }

    fn log_statements(mut self, level: LevelFilter) -> Self {
        self.log_settings.log_statements(level);
        self
    }

    fn log_slow_statements(mut self, level: LevelFilter, duration: Duration) -> Self {
        self.log_settings.log_slow_statements(level, duration);
        self
    }
}

struct ConnectionState {
    session: Session,
    statements: HashMap<String, CoreStatement>,
}

pub struct PgFakeConnection {
    state: Arc<Mutex<ConnectionState>>,
    transaction_depth: usize,
    pending_rollback: bool,
}

impl PgFakeConnection {
    pub fn new(db: Db) -> Self {
        Self {
            state: Arc::new(Mutex::new(ConnectionState {
                session: db.session(),
                statements: HashMap::new(),
            })),
            transaction_depth: 0,
            pending_rollback: false,
        }
    }

    fn take_pending_rollback(&mut self) -> bool {
        std::mem::take(&mut self.pending_rollback)
    }

    async fn run_control(&mut self, sql: String) -> Result<(), sqlx::Error> {
        let state = self.state.clone();
        let rollback_first = self.take_pending_rollback();
        tokio::task::spawn_blocking(move || {
            let mut state = state.lock().expect("connection mutex is poisoned");
            if rollback_first {
                state.session.execute("ROLLBACK").map_err(database_error)?;
            }
            state.session.execute(&sql).map_err(database_error)?;
            Ok(())
        })
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?
    }

    fn run(
        &mut self,
        sql: String,
        arguments: Option<PgFakeArguments>,
        statement: Option<PgFakeStatement>,
        persistent: bool,
    ) -> impl Future<Output = Result<Vec<Either<PgFakeQueryResult, PgFakeRow>>, sqlx::Error>>
    + Send
    + 'static {
        let state = self.state.clone();
        let rollback_first = self.take_pending_rollback();
        async move {
            tokio::task::spawn_blocking(move || {
                let mut state = state.lock().expect("connection mutex is poisoned");
                if rollback_first {
                    state.session.execute("ROLLBACK").map_err(database_error)?;
                }
                let results = if let Some(arguments) = arguments {
                    let prepared = if let Some(statement) = statement {
                        statement.statement
                    } else if persistent {
                        if let Some(statement) = state.statements.get(&sql) {
                            statement.clone()
                        } else {
                            let statement = state.session.prepare(&sql).map_err(database_error)?;
                            state.statements.insert(sql.clone(), statement.clone());
                            statement
                        }
                    } else {
                        state.session.prepare(&sql).map_err(database_error)?
                    };
                    vec![
                        state
                            .session
                            .run_prepared(&prepared, &arguments.values)
                            .map_err(database_error)?,
                    ]
                } else {
                    state.session.execute(&sql).map_err(database_error)?
                };
                Ok(map_results(results))
            })
            .await
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?
        }
    }
}

impl fmt::Debug for PgFakeConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PgFakeConnection")
            .field("transaction_depth", &self.transaction_depth)
            .field("pending_rollback", &self.pending_rollback)
            .finish_non_exhaustive()
    }
}

fn map_results(results: Vec<StatementResult>) -> Vec<Either<PgFakeQueryResult, PgFakeRow>> {
    results
        .into_iter()
        .flat_map(|result| match result {
            StatementResult::Affected(rows_affected) => {
                vec![Either::Left(PgFakeQueryResult { rows_affected })]
            }
            StatementResult::Query(result) => {
                let columns = Arc::new(
                    result
                        .columns
                        .into_iter()
                        .enumerate()
                        .map(|(ordinal, column)| PgFakeColumn {
                            ordinal,
                            name: column.name,
                            type_info: PgFakeTypeInfo::with_typmod(
                                BaseType::from_oid(column.type_oid)
                                    .expect("core returned an unknown Phase-1 type OID"),
                                column.typmod,
                            ),
                        })
                        .collect::<Vec<_>>(),
                );
                let mut output = result
                    .rows
                    .into_iter()
                    .map(|values| {
                        let values = values
                            .into_iter()
                            .zip(columns.iter())
                            .map(|(value, column)| PgFakeValue {
                                value,
                                type_info: column.type_info,
                            })
                            .collect();
                        Either::Right(PgFakeRow {
                            columns: columns.clone(),
                            values,
                        })
                    })
                    .collect::<Vec<_>>();
                output.push(Either::Left(PgFakeQueryResult::default()));
                output
            }
        })
        .collect()
}

impl Connection for PgFakeConnection {
    type Database = PgFake;
    type Options = PgFakeConnectOptions;

    async fn close(self) -> Result<(), sqlx::Error> {
        Ok(())
    }

    async fn close_hard(self) -> Result<(), sqlx::Error> {
        Ok(())
    }

    fn ping(&mut self) -> impl Future<Output = Result<(), sqlx::Error>> + Send + '_ {
        let rollback = self.take_pending_rollback();
        let state = self.state.clone();
        async move {
            if rollback {
                tokio::task::spawn_blocking(move || {
                    state
                        .lock()
                        .expect("connection mutex is poisoned")
                        .session
                        .execute("ROLLBACK")
                        .map_err(database_error)
                        .map(|_| ())
                })
                .await
                .map_err(|error| sqlx::Error::Protocol(error.to_string()))??;
            }
            Ok(())
        }
    }

    fn begin(
        &mut self,
    ) -> impl Future<Output = Result<sqlx::Transaction<'_, PgFake>, sqlx::Error>> + Send + '_ {
        sqlx::Transaction::begin(self, None)
    }

    fn cached_statements_size(&self) -> usize {
        self.state
            .lock()
            .expect("connection mutex is poisoned")
            .statements
            .len()
    }

    fn clear_cached_statements(
        &mut self,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send + '_ {
        self.state
            .lock()
            .expect("connection mutex is poisoned")
            .statements
            .clear();
        async { Ok(()) }
    }

    fn shrink_buffers(&mut self) {}

    async fn flush(&mut self) -> Result<(), sqlx::Error> {
        Ok(())
    }

    fn should_flush(&self) -> bool {
        false
    }
}

impl<'c> Executor<'c> for &'c mut PgFakeConnection {
    type Database = PgFake;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        mut query: E,
    ) -> BoxStream<'e, Result<Either<PgFakeQueryResult, PgFakeRow>, sqlx::Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, PgFake>,
    {
        let statement = query.statement().cloned();
        let arguments = match query.take_arguments() {
            Ok(arguments) => arguments,
            Err(error) => return Box::pin(stream::once(async { Err(sqlx::Error::Encode(error)) })),
        };
        let persistent = query.persistent();
        let sql = query.sql().as_str().to_owned();
        Box::pin(
            stream::once(self.run(sql, arguments, statement, persistent))
                .map_ok(|items| stream::iter(items.into_iter().map(Ok)))
                .try_flatten(),
        )
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        mut query: E,
    ) -> BoxFuture<'e, Result<Option<PgFakeRow>, sqlx::Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, PgFake>,
    {
        let statement = query.statement().cloned();
        let arguments = match query.take_arguments() {
            Ok(arguments) => arguments,
            Err(error) => return Box::pin(async { Err(sqlx::Error::Encode(error)) }),
        };
        let persistent = query.persistent();
        let sql = query.sql().as_str().to_owned();
        Box::pin(async move {
            Ok(self
                .run(sql, arguments, statement, persistent)
                .await?
                .into_iter()
                .find_map(Either::right))
        })
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [PgFakeTypeInfo],
    ) -> BoxFuture<'e, Result<PgFakeStatement, sqlx::Error>>
    where
        'c: 'e,
    {
        let state = self.state.clone();
        let rollback_first = self.take_pending_rollback();
        let query = sql.as_str().to_owned();
        let supplied_parameters = parameters.to_vec();
        Box::pin(async move {
            let (statement, inferred_parameters, columns) =
                tokio::task::spawn_blocking(move || {
                    let mut state = state.lock().expect("connection mutex is poisoned");
                    if rollback_first {
                        state.session.execute("ROLLBACK").map_err(database_error)?;
                    }
                    let statement = state.session.prepare(&query).map_err(database_error)?;
                    let parameters = statement
                        .parameter_types()
                        .iter()
                        .copied()
                        .map(PgFakeTypeInfo::new)
                        .collect::<Vec<_>>();
                    let columns = statement
                        .columns()
                        .iter()
                        .enumerate()
                        .map(|(ordinal, column)| PgFakeColumn {
                            ordinal,
                            name: column.name.clone(),
                            type_info: PgFakeTypeInfo::with_typmod(
                                BaseType::from_oid(column.type_oid)
                                    .expect("core returned an unknown Phase-1 type OID"),
                                column.typmod,
                            ),
                        })
                        .collect();
                    Ok::<_, sqlx::Error>((statement, parameters, columns))
                })
                .await
                .map_err(|error| sqlx::Error::Protocol(error.to_string()))??;
            Ok(PgFakeStatement {
                sql,
                statement,
                parameters: if supplied_parameters.is_empty() {
                    inferred_parameters
                } else {
                    supplied_parameters
                },
                columns,
            })
        })
    }
}

pub struct PgFakeTransactionManager;

impl TransactionManager for PgFakeTransactionManager {
    type Database = PgFake;

    async fn begin(
        connection: &mut PgFakeConnection,
        statement: Option<SqlStr>,
    ) -> Result<(), sqlx::Error> {
        if connection.transaction_depth != 0 {
            return Err(sqlx::Error::InvalidSavePointStatement);
        }
        connection
            .run_control(
                statement
                    .as_ref()
                    .map(SqlStr::as_str)
                    .unwrap_or("BEGIN")
                    .to_owned(),
            )
            .await?;
        connection.transaction_depth = 1;
        Ok(())
    }

    async fn commit(connection: &mut PgFakeConnection) -> Result<(), sqlx::Error> {
        if connection.transaction_depth == 0 {
            return Err(sqlx::Error::Protocol("no transaction to commit".into()));
        }
        connection.run_control("COMMIT".into()).await?;
        connection.transaction_depth = 0;
        Ok(())
    }

    async fn rollback(connection: &mut PgFakeConnection) -> Result<(), sqlx::Error> {
        if connection.transaction_depth == 0 {
            return Err(sqlx::Error::Protocol("no transaction to roll back".into()));
        }
        connection.run_control("ROLLBACK".into()).await?;
        connection.transaction_depth = 0;
        Ok(())
    }

    fn start_rollback(connection: &mut PgFakeConnection) {
        if connection.transaction_depth != 0 {
            connection.pending_rollback = true;
            connection.transaction_depth = 0;
        }
    }

    fn get_transaction_depth(connection: &PgFakeConnection) -> usize {
        connection.transaction_depth
    }
}

#[derive(Debug, Clone)]
pub struct PgFakeStatement {
    sql: SqlStr,
    statement: CoreStatement,
    parameters: Vec<PgFakeTypeInfo>,
    columns: Vec<PgFakeColumn>,
}

impl Statement for PgFakeStatement {
    type Database = PgFake;

    fn into_sql(self) -> SqlStr {
        self.sql
    }

    fn sql(&self) -> &SqlStr {
        &self.sql
    }

    fn parameters(&self) -> Option<Either<&[PgFakeTypeInfo], usize>> {
        Some(Either::Left(&self.parameters))
    }

    fn columns(&self) -> &[PgFakeColumn] {
        &self.columns
    }

    sqlx_core::impl_statement_query!(PgFakeArguments);
}

sqlx_core::impl_column_index_for_statement!(PgFakeStatement);

impl ColumnIndex<PgFakeStatement> for str {
    fn index(&self, statement: &PgFakeStatement) -> Result<usize, sqlx::Error> {
        statement
            .columns
            .iter()
            .position(|column| column.name == self)
            .ok_or_else(|| sqlx::Error::ColumnNotFound(self.to_owned()))
    }
}
