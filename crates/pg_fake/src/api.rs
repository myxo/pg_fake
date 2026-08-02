use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use sqlparser::ast::{
    Expr, OneOrManyWithParens, TransactionIsolationLevel as AstIsolationLevel, TransactionMode,
    Value as AstValue,
};

use crate::{
    error::{PgError, Result, SqlState},
    executor::{self, DatabaseState, ExecutionResult},
    parser,
    txn::{LockAttempt, Snapshot, TransactionStatus, Xid},
    value::{Oid, Value},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_oid: Oid,
    pub typmod: i32,
}
#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadCommitted,
    RepeatableRead,
}
#[derive(Clone)]
pub struct Db {
    state: Arc<Mutex<DatabaseState>>,
    condvar: Arc<Condvar>,
    default_lock_timeout: Duration,
}
pub struct DbBuilder {
    lock_timeout: Duration,
}
pub struct Session {
    db: Db,
    transaction: Option<SessionTransaction>,
    default_isolation: IsolationLevel,
    lock_timeout: Duration,
}
#[derive(Clone, Copy)]
enum SessionTransaction {
    Active(ActiveTransaction),
    Aborted(Xid),
}
#[derive(Clone, Copy)]
struct ActiveTransaction {
    xid: Xid,
    isolation: IsolationLevel,
    snapshot: Option<Snapshot>,
    statement_started: bool,
}
pub struct Transaction<'session> {
    session: &'session mut Session,
    finished: bool,
}

fn abort(state: &mut DatabaseState, xid: Xid) {
    state.transactions.abort(xid);
    for table in state.tables.values_mut() {
        table.abort(xid);
    }
    state.row_locks.release(xid);
}

fn invalid_lock_timeout() -> PgError {
    PgError::new(
        SqlState::InvalidParameterValue,
        "invalid value for parameter lock_timeout",
    )
}

fn parse_lock_timeout(expression: &Expr) -> Result<Duration> {
    let text = match expression {
        Expr::Value(AstValue::Number(value, _)) => value.as_str(),
        Expr::Value(AstValue::SingleQuotedString(value)) => value.trim(),
        _ => return Err(invalid_lock_timeout()),
    };
    let lower = text.to_ascii_lowercase();
    if let Some(milliseconds) = lower.strip_suffix("ms") {
        return milliseconds
            .trim()
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| invalid_lock_timeout());
    }
    if let Some(seconds) = lower.strip_suffix('s') {
        return seconds
            .trim()
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| invalid_lock_timeout());
    }
    lower
        .trim()
        .parse::<u64>()
        .map(Duration::from_millis)
        .map_err(|_| invalid_lock_timeout())
}

fn lock_timeout_error() -> PgError {
    PgError::new(
        SqlState::LockNotAvailable,
        "canceling statement due to lock timeout",
    )
}

fn isolation_from_modes(modes: &[TransactionMode]) -> Result<Option<IsolationLevel>> {
    let mut isolation = None;
    for mode in modes {
        let level = match mode {
            TransactionMode::IsolationLevel(
                AstIsolationLevel::ReadUncommitted | AstIsolationLevel::ReadCommitted,
            ) => IsolationLevel::ReadCommitted,
            TransactionMode::IsolationLevel(AstIsolationLevel::RepeatableRead) => {
                IsolationLevel::RepeatableRead
            }
            TransactionMode::IsolationLevel(AstIsolationLevel::Serializable) => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "SERIALIZABLE isolation is not implemented",
                ));
            }
            TransactionMode::AccessMode(_) => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "transaction access modes are not implemented",
                ));
            }
        };
        if isolation.replace(level).is_some() {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "isolation level specified more than once",
            ));
        }
    }
    Ok(isolation)
}

fn acquire_row_locks<'a>(
    condvar: &Condvar,
    timeout: Duration,
    mut state: MutexGuard<'a, DatabaseState>,
    statement: &parser::Statement,
    xid: Xid,
    isolation: IsolationLevel,
    mut snapshot: Snapshot,
) -> Result<(MutexGuard<'a, DatabaseState>, Snapshot)> {
    let deadline = (timeout != Duration::ZERO).then(|| Instant::now() + timeout);
    loop {
        let required = executor::required_row_locks(&state, statement, xid, &snapshot)?;
        let mut blocked = None;
        for required_lock in required {
            match state
                .row_locks
                .acquire(required_lock.key, xid, required_lock.mode)
            {
                LockAttempt::Acquired => condvar.notify_all(),
                LockAttempt::Blocked(conflicts) => {
                    blocked = Some((required_lock.key, conflicts));
                    break;
                }
            }
        }
        let Some((key, conflicts)) = blocked else {
            return Ok((state, snapshot));
        };
        state = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.row_locks.cancel_wait(key, xid);
                return Err(lock_timeout_error());
            }
            let (state, timeout) = condvar
                .wait_timeout(state, remaining)
                .expect("database mutex is poisoned");
            if timeout.timed_out() {
                let mut state = state;
                state.row_locks.cancel_wait(key, xid);
                return Err(lock_timeout_error());
            }
            state
        } else {
            condvar.wait(state).expect("database mutex is poisoned")
        };
        state.row_locks.cancel_wait(key, xid);
        if isolation == IsolationLevel::RepeatableRead
            && conflicts.iter().any(|holder| {
                matches!(
                    state.transactions.status(*holder),
                    Some(TransactionStatus::Committed(_))
                )
            })
        {
            return Err(PgError::new(
                SqlState::SerializationFailure,
                "could not serialize access due to concurrent update",
            ));
        }
        if isolation == IsolationLevel::ReadCommitted
            && conflicts.iter().any(|holder| {
                !matches!(
                    state.transactions.status(*holder),
                    Some(TransactionStatus::InFlight)
                )
            })
        {
            snapshot = Snapshot::new(&state.transactions);
        }
    }
}

impl Db {
    pub fn new() -> Self {
        Db::builder().build()
    }
    pub fn builder() -> DbBuilder {
        DbBuilder {
            lock_timeout: Duration::from_secs(1),
        }
    }
    pub fn session(&self) -> Session {
        Session {
            db: self.clone(),
            transaction: None,
            default_isolation: IsolationLevel::ReadCommitted,
            lock_timeout: self.default_lock_timeout,
        }
    }
}
impl DbBuilder {
    pub fn lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
    pub fn build(self) -> Db {
        Db {
            state: Arc::new(Mutex::new(DatabaseState::new())),
            condvar: Arc::new(Condvar::new()),
            default_lock_timeout: self.lock_timeout,
        }
    }
}
impl Default for Db {
    fn default() -> Self {
        Self::new()
    }
}
impl Session {
    pub fn execute(&mut self, sql: &str) -> Result<u64> {
        match self.run(sql)? {
            ExecutionResult::Affected(rows) => Ok(rows),
            ExecutionResult::Query(_) => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "use query for SELECT statements",
            )),
        }
    }
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        if !params.is_empty() {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "parameters are not implemented",
            ));
        }
        match self.run(sql)? {
            ExecutionResult::Query(result) => Ok(result),
            ExecutionResult::Affected(_) => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "query requires a SELECT statement",
            )),
        }
    }
    pub fn begin(&mut self) -> Result<Transaction<'_>> {
        self.begin_with(self.default_isolation)
    }
    pub fn begin_with(&mut self, isolation: IsolationLevel) -> Result<Transaction<'_>> {
        if self.transaction.is_some() {
            return Err(PgError::new(
                SqlState::ActiveSqlTransaction,
                "transaction already in progress",
            ));
        }
        self.start_transaction(isolation);
        Ok(Transaction {
            session: self,
            finished: false,
        })
    }
    fn start_transaction(&mut self, isolation: IsolationLevel) {
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        self.transaction = Some(SessionTransaction::Active(ActiveTransaction {
            xid: state.transactions.begin(),
            isolation,
            snapshot: None,
            statement_started: false,
        }));
    }
    fn finish_transaction(&mut self, commit: bool) {
        let Some(transaction) = self.transaction.take() else {
            return;
        };
        let xid = match transaction {
            SessionTransaction::Active(transaction) if commit => {
                let mut state = self.db.state.lock().expect("database mutex is poisoned");
                state.transactions.commit(transaction.xid);
                state.row_locks.release(transaction.xid);
                self.db.condvar.notify_all();
                return;
            }
            SessionTransaction::Active(transaction) => transaction.xid,
            SessionTransaction::Aborted(xid) => xid,
        };
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        abort(&mut state, xid);
        self.db.condvar.notify_all();
    }
    fn abort_transaction(&mut self) {
        if let Some(SessionTransaction::Active(transaction)) = self.transaction {
            self.transaction = Some(SessionTransaction::Aborted(transaction.xid));
        }
    }
    fn failed<T>(&mut self, error: PgError) -> Result<T> {
        self.abort_transaction();
        Err(error)
    }
    fn run(&mut self, sql: &str) -> Result<ExecutionResult> {
        let mut statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => return self.failed(error),
        };
        if statements.len() != 1 {
            return self.failed(PgError::new(
                SqlState::SyntaxError,
                "exactly one statement is required",
            ));
        }
        let statement = statements.pop().expect("statement count was checked");
        match &statement {
            parser::Statement::StartTransaction { modes, .. } => {
                return match self.transaction {
                    None => {
                        let isolation =
                            isolation_from_modes(modes)?.unwrap_or(self.default_isolation);
                        self.start_transaction(isolation);
                        Ok(ExecutionResult::Affected(0))
                    }
                    Some(SessionTransaction::Active(_)) => Ok(ExecutionResult::Affected(0)),
                    Some(SessionTransaction::Aborted(_)) => Err(PgError::new(
                        SqlState::InFailedSqlTransaction,
                        "current transaction is aborted",
                    )),
                };
            }
            parser::Statement::SetTransaction {
                modes,
                snapshot,
                session,
            } => {
                if matches!(self.transaction, Some(SessionTransaction::Aborted(_))) {
                    return Err(PgError::new(
                        SqlState::InFailedSqlTransaction,
                        "current transaction is aborted",
                    ));
                }
                if snapshot.is_some() {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "transaction snapshots are not implemented",
                    ));
                }
                let isolation = match isolation_from_modes(modes) {
                    Ok(isolation) => isolation,
                    Err(error) => return self.failed(error),
                };
                let Some(isolation) = isolation else {
                    return self.failed(PgError::new(
                        SqlState::SyntaxError,
                        "transaction isolation level is required",
                    ));
                };
                if *session {
                    self.default_isolation = isolation;
                    return Ok(ExecutionResult::Affected(0));
                }
                let Some(SessionTransaction::Active(mut transaction)) = self.transaction else {
                    return Ok(ExecutionResult::Affected(0));
                };
                if transaction.statement_started && isolation != transaction.isolation {
                    return self.failed(PgError::new(
                        SqlState::ActiveSqlTransaction,
                        "transaction isolation level must be set before any query",
                    ));
                }
                transaction.isolation = isolation;
                self.transaction = Some(SessionTransaction::Active(transaction));
                return Ok(ExecutionResult::Affected(0));
            }
            parser::Statement::SetVariable {
                local,
                hivevar,
                variables,
                value,
            } => {
                let OneOrManyWithParens::One(variable) = variables else {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "setting multiple variables is not implemented",
                    ));
                };
                if variable.to_string().eq_ignore_ascii_case("lock_timeout") {
                    if matches!(self.transaction, Some(SessionTransaction::Aborted(_))) {
                        return Err(PgError::new(
                            SqlState::InFailedSqlTransaction,
                            "current transaction is aborted",
                        ));
                    }
                    if *local || *hivevar || value.len() != 1 {
                        return self.failed(PgError::new(
                            SqlState::FeatureNotSupported,
                            "lock_timeout setting variant is not implemented",
                        ));
                    }
                    self.lock_timeout = match parse_lock_timeout(&value[0]) {
                        Ok(timeout) => timeout,
                        Err(error) => return self.failed(error),
                    };
                    return Ok(ExecutionResult::Affected(0));
                }
            }
            parser::Statement::Commit { chain } => {
                if *chain {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "COMMIT AND CHAIN is not implemented",
                    ));
                }
                self.finish_transaction(true);
                return Ok(ExecutionResult::Affected(0));
            }
            parser::Statement::Rollback { chain, savepoint } => {
                if *chain || savepoint.is_some() {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "ROLLBACK variant is not implemented",
                    ));
                }
                self.finish_transaction(false);
                return Ok(ExecutionResult::Affected(0));
            }
            _ => {}
        }
        if matches!(self.transaction, Some(SessionTransaction::Aborted(_))) {
            return Err(PgError::new(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        if self.transaction.is_some()
            && matches!(parser::classify(&statement), parser::StatementKind::Ddl)
        {
            return self.failed(PgError::new(
                SqlState::FeatureNotSupported,
                "DDL in an explicit transaction is not implemented",
            ));
        }
        if let Some(SessionTransaction::Active(mut transaction)) = self.transaction {
            let state_lock = self.db.state.clone();
            let condvar = self.db.condvar.clone();
            let state = state_lock.lock().expect("database mutex is poisoned");
            let snapshot = match transaction.isolation {
                IsolationLevel::ReadCommitted => Snapshot::new(&state.transactions),
                IsolationLevel::RepeatableRead => *transaction
                    .snapshot
                    .get_or_insert_with(|| Snapshot::new(&state.transactions)),
            };
            transaction.statement_started = true;
            self.transaction = Some(SessionTransaction::Active(transaction));
            let (mut state, snapshot) = match acquire_row_locks(
                &condvar,
                self.lock_timeout,
                state,
                &statement,
                transaction.xid,
                transaction.isolation,
                snapshot,
            ) {
                Ok(acquired) => acquired,
                Err(error) => return self.failed(error),
            };
            return match executor::dispatch(&mut state, &statement, transaction.xid, &snapshot) {
                Ok(result) => Ok(result),
                Err(error) => {
                    drop(state);
                    self.failed(error)
                }
            };
        }
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        let xid = state.transactions.begin();
        let snapshot = Snapshot::new(&state.transactions);
        let (mut state, snapshot) = match acquire_row_locks(
            &self.db.condvar,
            self.lock_timeout,
            state,
            &statement,
            xid,
            self.default_isolation,
            snapshot,
        ) {
            Ok(acquired) => acquired,
            Err(error) => {
                let mut state = self.db.state.lock().expect("database mutex is poisoned");
                abort(&mut state, xid);
                self.db.condvar.notify_all();
                return Err(error);
            }
        };
        match executor::dispatch(&mut state, &statement, xid, &snapshot) {
            Ok(result) => {
                state.transactions.commit(xid);
                state.row_locks.release(xid);
                self.db.condvar.notify_all();
                Ok(result)
            }
            Err(error) => {
                abort(&mut state, xid);
                self.db.condvar.notify_all();
                Err(error)
            }
        }
    }
}

impl Transaction<'_> {
    pub fn execute(&mut self, sql: &str) -> Result<u64> {
        self.session.execute(sql)
    }
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.session.query(sql, params)
    }
    pub fn commit(mut self) -> Result<()> {
        self.session.finish_transaction(true);
        self.finished = true;
        Ok(())
    }
    pub fn rollback(mut self) -> Result<()> {
        self.session.finish_transaction(false);
        self.finished = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.session.finish_transaction(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread};

    use crate::{
        txn::{Snapshot, visible_version},
        value::BaseType,
    };

    use super::*;

    fn wait_until_blocked(db: &Db) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if db.state.lock().unwrap().row_locks.has_waiters() {
                return;
            }
            assert!(Instant::now() < deadline, "transaction did not block");
            thread::yield_now();
        }
    }

    #[test]
    fn autocommit_creates_and_drops_tables() {
        let db = Db::new();
        let mut session = db.session();
        assert_eq!(session.execute("CREATE TABLE items (id INTEGER NOT NULL, name VARCHAR(12), amount NUMERIC(8, 2))").unwrap(), 0);
        let state = db.state.lock().unwrap();
        let table = state.catalog.table("items").unwrap();
        assert_eq!(table.columns[0].data_type.base, BaseType::Int4);
        assert_eq!(table.columns[1].data_type.typmod, 16);
        assert_eq!(table.columns[2].data_type.typmod, (8 << 16) + 2 + 4);
        drop(state);
        assert_eq!(session.execute("DROP TABLE items").unwrap(), 1);
    }

    #[test]
    fn selects_projections_with_metadata_in_row_id_order() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, name TEXT)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (2, 'second'), (1, 'first')")
            .unwrap();

        let result = session.query("SELECT name, id FROM items", &[]).unwrap();

        assert_eq!(
            result.columns,
            vec![
                ColumnMeta {
                    name: "name".into(),
                    type_oid: BaseType::Text.oid(),
                    typmod: -1,
                },
                ColumnMeta {
                    name: "id".into(),
                    type_oid: BaseType::Int4.oid(),
                    typmod: -1,
                },
            ]
        );
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Text("second".into()), Value::Int4(2)],
                vec![Value::Text("first".into()), Value::Int4(1)],
            ]
        );
        let all_columns = session.query("SELECT * FROM items", &[]).unwrap();
        assert_eq!(
            all_columns.rows,
            vec![
                vec![Value::Int4(2), Value::Text("second".into())],
                vec![Value::Int4(1), Value::Text("first".into())],
            ]
        );
    }

    #[test]
    fn select_excludes_uncommitted_rows_from_another_transaction() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        let mut state = db.state.lock().unwrap();
        let writer = state.transactions.begin();
        let table_id = state.catalog.table("items").unwrap().id;
        state
            .tables
            .get_mut(&table_id)
            .unwrap()
            .insert(writer, vec![Value::Int4(1)]);
        drop(state);

        assert!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap()
                .rows
                .is_empty()
        );

        let mut state = db.state.lock().unwrap();
        state.transactions.abort(writer);
    }

    #[test]
    fn select_reports_unknown_tables_and_columns() {
        let db = Db::new();
        let mut session = db.session();

        assert_eq!(
            session
                .query("SELECT * FROM missing", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        assert_eq!(
            session
                .query("SELECT missing FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedColumn
        );
    }

    #[test]
    fn evaluates_arithmetic_and_comparison_projections() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER, name TEXT, price NUMERIC)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (7, 3, 'seven', 2.5)")
            .unwrap();

        let result = session
            .query(
                "SELECT id + amount, id - amount, id * amount, id / amount, id % amount, id > amount, name = 'seven', price * 2.0 FROM items",
                &[],
            )
            .unwrap();

        assert_eq!(
            result.rows,
            vec![vec![
                Value::Int4(10),
                Value::Int4(4),
                Value::Int4(21),
                Value::Int4(2),
                Value::Int4(1),
                Value::Bool(true),
                Value::Bool(true),
                Value::Numeric("5.00".parse().unwrap()),
            ]]
        );
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["?column?"; 8]
        );
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| (column.type_oid, column.typmod))
                .collect::<Vec<_>>(),
            vec![
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Bool.oid(), -1),
                (BaseType::Bool.oid(), -1),
                (BaseType::Numeric.oid(), -1),
            ]
        );
        assert_eq!(
            session
                .query("SELECT id / 0 FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::DivisionByZero
        );
        session
            .execute("INSERT INTO items VALUES (2147483647, 1, 'max', 1.0)")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT id + 1 FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::NumericValueOutOfRange
        );
    }

    #[test]
    fn evaluates_case_and_common_scalar_functions() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE items (
                    id INTEGER,
                    score INTEGER,
                    label TEXT,
                    delta INTEGER
                )",
            )
            .unwrap();
        session
            .execute(
                "INSERT INTO items VALUES
                    (1, 7, 'MiXeD', 3),
                    (2, 0, NULL, NULL),
                    (3, NULL, 'third', 4)",
            )
            .unwrap();

        let result = session
            .query(
                "SELECT
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
                    abs(-delta)
                 FROM items",
                &[],
            )
            .unwrap();

        assert_eq!(
            result.rows,
            vec![
                vec![
                    Value::Text("high".into()),
                    Value::Text("one".into()),
                    Value::Null,
                    Value::Text("MiXeD".into()),
                    Value::Int4(7),
                    Value::Int4(7),
                    Value::Int4(5),
                    Value::Int4(5),
                    Value::Text("mixed".into()),
                    Value::Text("MIXED".into()),
                    Value::Int4(3),
                ],
                vec![
                    Value::Text("low".into()),
                    Value::Null,
                    Value::Null,
                    Value::Text("fallback".into()),
                    Value::Null,
                    Value::Int4(5),
                    Value::Int4(0),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
                vec![
                    Value::Text("missing".into()),
                    Value::Text("other".into()),
                    Value::Null,
                    Value::Text("third".into()),
                    Value::Null,
                    Value::Int4(5),
                    Value::Int4(5),
                    Value::Int4(5),
                    Value::Text("third".into()),
                    Value::Text("THIRD".into()),
                    Value::Int4(4),
                ],
            ]
        );
        assert_eq!(
            session
                .query(
                    "SELECT
                        CASE WHEN id = 1 THEN 10 ELSE 1 / (id - 1) END,
                        COALESCE(score, 1 / (score - 7))
                     FROM items
                     WHERE id = 1",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(10), Value::Int4(7)]]
        );
    }

    #[test]
    fn simple_case_accepts_minimum_int4_literal() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (0)").unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT CASE id WHEN -2147483648 THEN 'minimum' ELSE 'other' END FROM items",
                    &[]
                )
                .unwrap()
                .rows,
            vec![vec![Value::Text("other".into())]]
        );
    }

    #[test]
    fn abs_supports_all_phase_one_numeric_types() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE numbers (
                    int2_value SMALLINT,
                    int4_value INTEGER,
                    int8_value BIGINT,
                    float4_value REAL,
                    float8_value DOUBLE PRECISION,
                    numeric_value NUMERIC
                )",
            )
            .unwrap();
        let mut state = db.state.lock().unwrap();
        let xid = state.transactions.begin();
        let table_id = state.catalog.table("numbers").unwrap().id;
        state.tables.get_mut(&table_id).unwrap().insert(
            xid,
            vec![
                Value::Int2(-2),
                Value::Int4(-4),
                Value::Int8(-8),
                Value::Float4(-4.5),
                Value::Float8(-8.5),
                Value::Numeric("-12.25".parse().unwrap()),
            ],
        );
        state.transactions.commit(xid);
        drop(state);

        assert_eq!(
            session
                .query(
                    "SELECT
                        abs(int2_value),
                        abs(int4_value),
                        abs(int8_value),
                        abs(float4_value),
                        abs(float8_value),
                        abs(numeric_value)
                     FROM numbers",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![
                Value::Int2(2),
                Value::Int4(4),
                Value::Int8(8),
                Value::Float4(4.5),
                Value::Float8(8.5),
                Value::Numeric("12.25".parse().unwrap()),
            ]]
        );
    }

    #[test]
    fn case_and_functions_report_type_and_name_errors() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT CASE WHEN id = 1 THEN id ELSE TRUE END FROM items",
                    &[]
                )
                .unwrap_err()
                .sqlstate,
            SqlState::DatatypeMismatch
        );
        assert_eq!(
            session
                .query("SELECT unknown_function(id) FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedFunction
        );
    }

    #[test]
    fn coerces_phase_one_types_in_all_cast_contexts() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE types (
                    small_value SMALLINT,
                    int_value INTEGER,
                    big_value BIGINT,
                    numeric_value NUMERIC,
                    real_value REAL,
                    double_value DOUBLE PRECISION,
                    short_label VARCHAR(4)
                )",
            )
            .unwrap();
        session
            .execute("INSERT INTO types VALUES (1, 2, 3, 4, 5, 6, 'abcd')")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT
                        small_value + int_value,
                        int_value + big_value,
                        big_value + numeric_value,
                        numeric_value + real_value,
                        real_value + double_value,
                        int_value = '2',
                        CASE WHEN TRUE THEN int_value ELSE numeric_value END,
                        COALESCE(NULL, int_value, numeric_value)
                     FROM types",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![
                Value::Int4(3),
                Value::Int8(5),
                Value::Numeric("7".parse().unwrap()),
                Value::Float4(9.0),
                Value::Float8(11.0),
                Value::Bool(true),
                Value::Numeric("2".parse().unwrap()),
                Value::Numeric("2".parse().unwrap()),
            ]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT
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
                     FROM types",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![
                Value::Int4(42),
                Value::Numeric("3.5".parse().unwrap()),
                Value::Int4(3),
                Value::Text("1".into()),
                Value::Text("true".into()),
                Value::Bool(true),
                Value::Int4(1),
                Value::Bytea(vec![0, 0, 1, 2]),
                Value::Int4(258),
                Value::Text("abc".into()),
                Value::Numeric("12.4".parse().unwrap()),
            ]]
        );

        session
            .execute("UPDATE types SET small_value = int_value, int_value = 2.6")
            .unwrap();
        session.execute("UPDATE types SET int_value = '7'").unwrap();
        assert_eq!(
            session
                .query("SELECT small_value, int_value FROM types", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int2(2), Value::Int4(7)]]
        );
    }

    #[test]
    fn coercion_reports_postgres_error_categories() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE assignments (
                    small_value SMALLINT,
                    short_label VARCHAR(3),
                    fixed_numeric NUMERIC(4, 1)
                )",
            )
            .unwrap();

        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES ('bad', 'abc', 1)")
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTextRepresentation
        );
        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES (40000, 'abc', 1)")
                .unwrap_err()
                .sqlstate,
            SqlState::NumericValueOutOfRange
        );
        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES (1, 'toolong', 1)")
                .unwrap_err()
                .sqlstate,
            SqlState::StringDataRightTruncation
        );
        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES (1, 'abc', 1234.5)")
                .unwrap_err()
                .sqlstate,
            SqlState::NumericValueOutOfRange
        );
        assert_eq!(
            session
                .query("SELECT TRUE::BYTEA FROM assignments", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::CannotCoerce
        );
    }

    #[test]
    fn orders_rows_by_columns_expressions_and_output_positions() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE items (
                    id INTEGER,
                    name TEXT,
                    score INTEGER,
                    optional INTEGER
                )",
            )
            .unwrap();
        session
            .execute(
                "INSERT INTO items VALUES
                    (1, 'b', 2, NULL),
                    (2, 'a', 2, 5),
                    (3, 'c', 1, 3),
                    (4, NULL, 1, NULL),
                    (5, 'a', 2, 1)",
            )
            .unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT id, name FROM items
                     ORDER BY name ASC NULLS LAST, id DESC",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(5), Value::Text("a".into())],
                vec![Value::Int4(2), Value::Text("a".into())],
                vec![Value::Int4(1), Value::Text("b".into())],
                vec![Value::Int4(3), Value::Text("c".into())],
                vec![Value::Int4(4), Value::Null],
            ]
        );
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY score ASC, id DESC", &[],)
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(4)],
                vec![Value::Int4(3)],
                vec![Value::Int4(5)],
                vec![Value::Int4(2)],
                vec![Value::Int4(1)],
            ]
        );
        assert_eq!(
            session
                .query(
                    "SELECT name, id FROM items
                     ORDER BY 1 DESC NULLS FIRST, 2 ASC",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Null, Value::Int4(4)],
                vec![Value::Text("c".into()), Value::Int4(3)],
                vec![Value::Text("b".into()), Value::Int4(1)],
                vec![Value::Text("a".into()), Value::Int4(2)],
                vec![Value::Text("a".into()), Value::Int4(5)],
            ]
        );
        assert_eq!(
            session
                .query(
                    "SELECT id FROM items
                     ORDER BY score + id DESC, id ASC",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(5)],
                vec![Value::Int4(4)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)],
                vec![Value::Int4(1)],
            ]
        );
    }

    #[test]
    fn order_by_uses_postgres_null_defaults_and_validates_positions() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, optional INTEGER)")
            .unwrap();
        session
            .execute(
                "INSERT INTO items VALUES
                    (1, NULL), (2, 5), (3, 3), (4, NULL), (5, 1)",
            )
            .unwrap();

        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY optional ASC", &[])
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(5)],
                vec![Value::Int4(3)],
                vec![Value::Int4(2)],
                vec![Value::Int4(1)],
                vec![Value::Int4(4)],
            ]
        );
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY optional DESC", &[])
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(1)],
                vec![Value::Int4(4)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)],
                vec![Value::Int4(5)],
            ]
        );
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY 0", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidColumnReference
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY 2", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidColumnReference
        );
    }

    #[test]
    fn limits_and_offsets_rows_after_ordering() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session
            .execute("INSERT INTO items VALUES (4), (1), (5), (2), (3)")
            .unwrap();

        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY id LIMIT 2 OFFSET 1", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
        );
        assert_eq!(
            session
                .query("SELECT id FROM items LIMIT 2", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(4)], vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query("SELECT id FROM items OFFSET 3", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
        );
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY id LIMIT 0 OFFSET 2", &[])
                .unwrap()
                .rows,
            Vec::<Vec<Value>>::new()
        );
        assert_eq!(
            session
                .query(
                    "SELECT id FROM items ORDER BY id LIMIT NULL OFFSET NULL",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(1)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)],
                vec![Value::Int4(4)],
                vec![Value::Int4(5)],
            ]
        );
    }

    #[test]
    fn rejects_negative_limit_and_offset() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();

        assert_eq!(
            session
                .query("SELECT id FROM items LIMIT -1", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidRowCountInLimitClause
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session
                .query("SELECT id FROM items OFFSET -1", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidRowCountInResultOffsetClause
        );
    }

    #[test]
    fn applies_defaults_to_inserted_and_updated_rows() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE items (
                    id INTEGER NOT NULL DEFAULT 10,
                    amount INTEGER NOT NULL DEFAULT 2 + 3,
                    label TEXT DEFAULT upper('mixed'),
                    optional INTEGER
                )",
            )
            .unwrap();

        session.execute("INSERT INTO items DEFAULT VALUES").unwrap();
        session
            .execute("INSERT INTO items (id, label) VALUES (1, DEFAULT), (2, NULL)")
            .unwrap();
        session
            .execute("INSERT INTO items (id, amount) VALUES (3, DEFAULT)")
            .unwrap();
        session
            .execute("UPDATE items SET amount = DEFAULT, label = DEFAULT WHERE id = 2")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT id, amount, label, optional FROM items ORDER BY id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![
                vec![
                    Value::Int4(1),
                    Value::Int4(5),
                    Value::Text("MIXED".into()),
                    Value::Null,
                ],
                vec![
                    Value::Int4(2),
                    Value::Int4(5),
                    Value::Text("MIXED".into()),
                    Value::Null,
                ],
                vec![
                    Value::Int4(3),
                    Value::Int4(5),
                    Value::Text("MIXED".into()),
                    Value::Null,
                ],
                vec![
                    Value::Int4(10),
                    Value::Int4(5),
                    Value::Text("MIXED".into()),
                    Value::Null,
                ],
            ]
        );
    }

    #[test]
    fn enforces_not_null_after_defaults_and_assignments() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER NOT NULL, optional INTEGER)")
            .unwrap();

        assert_eq!(
            session
                .execute("INSERT INTO items (optional) VALUES (1)")
                .unwrap_err()
                .sqlstate,
            SqlState::NotNullViolation
        );
        session
            .execute("INSERT INTO items VALUES (1, NULL)")
            .unwrap();
        assert_eq!(
            session
                .execute("UPDATE items SET id = DEFAULT")
                .unwrap_err()
                .sqlstate,
            SqlState::NotNullViolation
        );
        assert_eq!(
            session
                .execute("CREATE TABLE invalid_default (a INTEGER, b INTEGER DEFAULT a)")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
    }

    #[test]
    fn enforces_check_constraints_on_insert_and_update() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE ranges (
                    value INTEGER CHECK (value > 0),
                    lower_bound INTEGER,
                    upper_bound INTEGER,
                    CHECK (lower_bound < upper_bound)
                )",
            )
            .unwrap();

        session
            .execute("INSERT INTO ranges VALUES (1, 1, 2), (NULL, NULL, NULL)")
            .unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO ranges VALUES (-1, 1, 2)")
                .unwrap_err()
                .sqlstate,
            SqlState::CheckViolation
        );
        assert_eq!(
            session
                .execute("INSERT INTO ranges VALUES (2, 3, 2)")
                .unwrap_err()
                .sqlstate,
            SqlState::CheckViolation
        );
        assert_eq!(
            session
                .execute("UPDATE ranges SET value = -1 WHERE value = 1")
                .unwrap_err()
                .sqlstate,
            SqlState::CheckViolation
        );
        session
            .execute("UPDATE ranges SET lower_bound = NULL WHERE value = 1")
            .unwrap();
        assert_eq!(
            session
                .execute("CREATE TABLE invalid_check (value INTEGER CHECK (value + 1))")
                .unwrap_err()
                .sqlstate,
            SqlState::DatatypeMismatch
        );
    }

    #[test]
    fn enforces_primary_and_multi_column_unique_constraints() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE accounts (
                    id INTEGER PRIMARY KEY,
                    tenant INTEGER,
                    email TEXT,
                    UNIQUE (tenant, email)
                )",
            )
            .unwrap();
        session
            .execute("INSERT INTO accounts VALUES (1, 1, 'a'), (2, 1, 'b')")
            .unwrap();

        assert_eq!(
            session
                .execute("INSERT INTO accounts VALUES (1, 2, 'c')")
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
        assert_eq!(
            session
                .execute("INSERT INTO accounts VALUES (3, 1, 'a')")
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
        assert_eq!(
            session
                .execute("UPDATE accounts SET id = 1 WHERE id = 2")
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
        assert_eq!(
            session
                .execute("INSERT INTO accounts VALUES (NULL, 2, 'd')")
                .unwrap_err()
                .sqlstate,
            SqlState::NotNullViolation
        );

        session
            .execute("INSERT INTO accounts VALUES (3, NULL, 'a'), (4, NULL, 'a')")
            .unwrap();
        session
            .execute("UPDATE accounts SET id = 5, email = 'c' WHERE id = 2")
            .unwrap();
        session
            .execute("DELETE FROM accounts WHERE id = 1")
            .unwrap();
        session
            .execute("INSERT INTO accounts VALUES (1, 1, 'a')")
            .unwrap();
    }

    #[test]
    fn rebuilds_unique_indexes_after_rollback() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();
        session.execute("BEGIN").unwrap();
        session.execute("UPDATE items SET id = 2").unwrap();
        session.execute("ROLLBACK").unwrap();

        session.execute("INSERT INTO items VALUES (2)").unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO items VALUES (1)")
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
    }

    #[test]
    fn explicit_transactions_control_insert_and_update_visibility() {
        let db = Db::new();
        let mut first = db.session();
        let mut second = db.session();
        first
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (1, 1)").unwrap();

        first.execute("BEGIN").unwrap();
        first
            .execute("UPDATE items SET amount = amount + 1 WHERE id = 1")
            .unwrap();
        first.execute("INSERT INTO items VALUES (2, 2)").unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)]
            ]
        );
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1), Value::Int4(1)]]
        );
        first.execute("COMMIT").unwrap();
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)]
            ]
        );

        first.execute("BEGIN").unwrap();
        first.execute("INSERT INTO items VALUES (3, 3)").unwrap();
        first.execute("ROLLBACK").unwrap();
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)]
            ]
        );
        first.execute("INSERT INTO items VALUES (4, 4)").unwrap();
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)],
                vec![Value::Int4(4), Value::Int4(4)],
            ]
        );
    }

    #[test]
    fn isolation_levels_control_snapshot_lifetime() {
        let db = Db::new();
        let mut first = db.session();
        let mut second = db.session();
        first.execute("CREATE TABLE items (id INTEGER)").unwrap();
        first.execute("INSERT INTO items VALUES (1)").unwrap();

        first.execute("BEGIN").unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        second.execute("INSERT INTO items VALUES (2)").unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
        );
        first.execute("COMMIT").unwrap();

        first
            .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
        );
        second.execute("INSERT INTO items VALUES (3)").unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
        );
        first.execute("COMMIT").unwrap();
    }

    #[test]
    fn isolation_level_selection_follows_postgres_order() {
        let db = Db::new();
        let mut first = db.session();
        let mut second = db.session();
        first.execute("CREATE TABLE items (id INTEGER)").unwrap();
        first.execute("INSERT INTO items VALUES (1)").unwrap();
        first
            .execute("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .unwrap();

        first.execute("BEGIN").unwrap();
        first.query("SELECT * FROM items", &[]).unwrap();
        second.execute("INSERT INTO items VALUES (2)").unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        first.execute("COMMIT").unwrap();

        first.execute("BEGIN").unwrap();
        first
            .execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .unwrap();
        first.query("SELECT * FROM items", &[]).unwrap();
        second.execute("INSERT INTO items VALUES (3)").unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)]
            ]
        );
        first.execute("COMMIT").unwrap();

        {
            let mut transaction = first.begin_with(IsolationLevel::RepeatableRead).unwrap();
            transaction.query("SELECT * FROM items", &[]).unwrap();
            second.execute("INSERT INTO items VALUES (4)").unwrap();
            assert_eq!(
                transaction
                    .query("SELECT * FROM items", &[])
                    .unwrap()
                    .rows
                    .len(),
                3
            );
            transaction.commit().unwrap();
        }

        first.execute("BEGIN").unwrap();
        first.query("SELECT * FROM items", &[]).unwrap();
        assert_eq!(
            first
                .execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
                .unwrap_err()
                .sqlstate,
            SqlState::ActiveSqlTransaction
        );
        assert_eq!(
            first
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InFailedSqlTransaction
        );
        first.execute("ROLLBACK").unwrap();
    }

    #[test]
    fn read_committed_writer_blocks_then_rechecks_after_commit() {
        let db = Db::builder().lock_timeout(Duration::from_secs(2)).build();
        let mut first = db.session();
        let mut second = db.session();
        first
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
        first.execute("BEGIN").unwrap();
        first.execute("UPDATE items SET amount = 2").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.execute("UPDATE items SET amount = amount + 1 WHERE id = 1"))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(1)
        );
        handle.join().unwrap();
        assert_eq!(
            first.query("SELECT amount FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(3)]]
        );
    }

    #[test]
    fn blocked_writer_proceeds_after_holder_rollback() {
        let db = Db::builder().lock_timeout(Duration::from_secs(2)).build();
        let mut first = db.session();
        let mut second = db.session();
        first
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
        first.execute("BEGIN").unwrap();
        first.execute("UPDATE items SET amount = 5").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.execute("UPDATE items SET amount = amount + 1 WHERE id = 1"))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(1)
        );
        handle.join().unwrap();
        assert_eq!(
            first.query("SELECT amount FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
    }

    #[test]
    fn repeatable_read_writer_fails_after_concurrent_commit() {
        let db = Db::builder().lock_timeout(Duration::from_secs(2)).build();
        let mut first = db.session();
        let mut second = db.session();
        first
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
        second
            .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .unwrap();
        second.query("SELECT * FROM items", &[]).unwrap();
        first.execute("BEGIN").unwrap();
        first.execute("UPDATE items SET amount = 2").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let error = second
                .execute("UPDATE items SET amount = amount + 1 WHERE id = 1")
                .unwrap_err();
            second.execute("ROLLBACK").unwrap();
            result_sender.send(error.sqlstate).unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            SqlState::SerializationFailure
        );
        handle.join().unwrap();
    }

    #[test]
    fn row_lock_clauses_use_update_and_share_compatibility() {
        let db = Db::builder().lock_timeout(Duration::from_secs(2)).build();
        let mut first = db.session();
        let mut second = db.session();
        let mut third = db.session();
        first.execute("CREATE TABLE items (id INTEGER)").unwrap();
        first.execute("INSERT INTO items VALUES (1)").unwrap();
        first.execute("BEGIN").unwrap();
        second.execute("BEGIN").unwrap();
        first.query("SELECT * FROM items FOR SHARE", &[]).unwrap();
        second.query("SELECT * FROM items FOR SHARE", &[]).unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(third.execute("DELETE FROM items WHERE id = 1"))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();
        assert!(matches!(
            result_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        second.execute("COMMIT").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(1)
        );
        handle.join().unwrap();

        first.execute("INSERT INTO items VALUES (2)").unwrap();
        first.execute("BEGIN").unwrap();
        first
            .query("SELECT * FROM items WHERE id = 2 FOR UPDATE", &[])
            .unwrap();
        let mut writer = db.session();
        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(writer.execute("UPDATE items SET id = 3 WHERE id = 2"))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(1)
        );
        handle.join().unwrap();
    }

    #[test]
    fn lock_timeout_builder_and_session_setting_control_waits() {
        let db = Db::builder()
            .lock_timeout(Duration::from_millis(40))
            .build();
        let mut first = db.session();
        let mut second = db.session();
        assert_eq!(second.lock_timeout, Duration::from_millis(40));
        second.execute("SET lock_timeout = 250").unwrap();
        assert_eq!(second.lock_timeout, Duration::from_millis(250));
        second.execute("SET lock_timeout = '2s'").unwrap();
        assert_eq!(second.lock_timeout, Duration::from_secs(2));
        second.execute("SET lock_timeout = '20ms'").unwrap();
        assert_eq!(second.lock_timeout, Duration::from_millis(20));

        first.execute("CREATE TABLE items (id INTEGER)").unwrap();
        first.execute("INSERT INTO items VALUES (1)").unwrap();
        first.execute("BEGIN").unwrap();
        first.execute("UPDATE items SET id = 2").unwrap();
        let started = Instant::now();
        assert_eq!(
            second
                .execute("UPDATE items SET id = 3")
                .unwrap_err()
                .sqlstate,
            SqlState::LockNotAvailable
        );
        assert!(started.elapsed() >= Duration::from_millis(10));
        first.execute("ROLLBACK").unwrap();
        second.execute("SET lock_timeout = 0").unwrap();
        assert_eq!(second.lock_timeout, Duration::ZERO);
    }

    #[test]
    fn rolled_back_update_restores_row_for_later_delete() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        session.execute("INSERT INTO items VALUES (1, 1)").unwrap();

        session.execute("BEGIN").unwrap();
        session.execute("UPDATE items SET amount = 2").unwrap();
        session.execute("ROLLBACK").unwrap();

        assert_eq!(
            session.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1), Value::Int4(1)]]
        );
        assert_eq!(session.execute("DELETE FROM items").unwrap(), 1);
    }

    #[test]
    fn deletes_matching_rows_and_all_rows() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (1, 2), (2, NULL), (3, 4)")
            .unwrap();

        assert_eq!(
            session
                .execute("DELETE FROM items WHERE amount > 2")
                .unwrap(),
            1
        );
        assert_eq!(
            session.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Null],
            ]
        );
        assert_eq!(session.execute("DELETE FROM items").unwrap(), 2);
        assert!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[test]
    fn explicit_delete_visibility_matches_transaction_outcome() {
        let db = Db::new();
        let mut writer = db.session();
        let mut reader = db.session();
        writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
        writer
            .execute("INSERT INTO items VALUES (1), (2), (3)")
            .unwrap();

        writer.execute("BEGIN").unwrap();
        assert_eq!(writer.execute("DELETE FROM items WHERE id = 1").unwrap(), 1);
        assert_eq!(
            writer.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
        );
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)]
            ]
        );
        writer.execute("ROLLBACK").unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)]
            ]
        );

        writer.execute("BEGIN").unwrap();
        writer.execute("DELETE FROM items WHERE id = 2").unwrap();
        writer.execute("COMMIT").unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(3)]]
        );
    }

    #[test]
    fn delete_requires_a_boolean_where_expression() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();

        assert_eq!(
            session
                .execute("DELETE FROM items WHERE id")
                .unwrap_err()
                .sqlstate,
            SqlState::DatatypeMismatch
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    fn explicit_transactions_abort_after_errors_and_raii_rolls_back() {
        let db = Db::new();
        let mut session = db.session();
        let mut reader = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();

        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO missing VALUES (1)")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InFailedSqlTransaction
        );
        session.execute("ROLLBACK").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();

        {
            let mut transaction = session.begin().unwrap();
            transaction.execute("INSERT INTO items VALUES (2)").unwrap();
        }
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        let mut transaction = session.begin().unwrap();
        transaction.execute("INSERT INTO items VALUES (3)").unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(3)]]
        );
    }

    #[test]
    fn rejects_ddl_inside_explicit_transactions() {
        let db = Db::new();
        let mut session = db.session();

        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .execute("CREATE TABLE items (id INTEGER)")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    fn insert_uses_exact_literal_types_and_commits() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, name TEXT)")
            .unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO items (name, id) VALUES ('one', 1), ('two', 2)")
                .unwrap(),
            2
        );
        let mut state = db.state.lock().unwrap();
        let reader = state.transactions.begin();
        let snapshot = Snapshot::new(&state.transactions);
        let schema = state.catalog.table("items").unwrap();
        let table = state.tables.get(&schema.id).unwrap();
        let rows = table
            .rows()
            .map(|(_, chain)| {
                visible_version(chain, &snapshot, reader, &state.transactions)
                    .unwrap()
                    .row
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                vec![Value::Int4(1), Value::Text("one".into())],
                vec![Value::Int4(2), Value::Text("two".into())]
            ]
        );
        let _ = table;
        drop(state);
        let error = session
            .execute("INSERT INTO items VALUES ('wrong', 'type')")
            .unwrap_err();
        assert_eq!(error.sqlstate, SqlState::InvalidTextRepresentation);
    }
}
