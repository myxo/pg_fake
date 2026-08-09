use std::{
    collections::BTreeSet,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use rand_chacha::{ChaCha12Rng, rand_core::SeedableRng};
use sqlparser::ast::{
    ContextModifier, Expr, Set, TransactionIsolationLevel as AstIsolationLevel, TransactionMode,
    Value as AstValue,
};

use crate::{
    analyzer,
    catalog::TableSchema,
    error::{PgError, Result, SqlState},
    executor::{self, DatabaseState},
    parser,
    storage::Table,
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
#[derive(Debug, Clone, PartialEq)]
pub enum StatementResult {
    Affected(u64),
    Query(QueryResult),
}
#[derive(Debug, Clone)]
pub struct Statement {
    statement: parser::Statement,
    parameter_types: Vec<crate::value::BaseType>,
    columns: Vec<ColumnMeta>,
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
    clock: Arc<Mutex<Clock>>,
    rng: Arc<Mutex<ChaCha12Rng>>,
}
pub struct DbBuilder {
    lock_timeout: Duration,
    mock_time: bool,
    seed: Option<u64>,
}
#[derive(Clone, Copy)]
enum Clock {
    Real,
    Mock(chrono::DateTime<chrono::Utc>),
}
pub struct Session {
    db: Db,
    transaction: Option<SessionTransaction>,
    default_isolation: IsolationLevel,
    lock_timeout: Duration,
    timezone: String,
    ddl_undo: Vec<DdlUndo>,
    settings_undo: Option<(IsolationLevel, Duration, String)>,
    deferred_constraints: BTreeSet<String>,
    defer_all_constraints: bool,
    deferred_foreign_keys_dirty: bool,
}
#[derive(Clone, Copy)]
enum SessionTransaction {
    Active(ActiveTransaction),
    Aborted { xid: Xid, implicit_batch: bool },
}
#[derive(Clone, Copy)]
struct ActiveTransaction {
    xid: Xid,
    isolation: IsolationLevel,
    snapshot: Option<Snapshot>,
    statement_started: bool,
    implicit_batch: bool,
    transaction_timestamp: chrono::DateTime<chrono::Utc>,
}
enum DdlUndo {
    DropCreated(String),
    RestoreDropped(TableSchema, Table),
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
    state.wait_for.remove_transaction(xid);
}

fn ddl_undo_for_statement(
    state: &DatabaseState,
    statement: &parser::Statement,
) -> Result<Vec<DdlUndo>> {
    match statement {
        parser::Statement::CreateTable(create) => {
            let name = executor::name(&create.name)?;
            Ok(state
                .catalog
                .table(&name)
                .is_err()
                .then_some(DdlUndo::DropCreated(name))
                .into_iter()
                .collect())
        }
        parser::Statement::Drop {
            object_type: sqlparser::ast::ObjectType::Table,
            names,
            ..
        } => names
            .iter()
            .filter_map(|name| {
                let name = match executor::name(name) {
                    Ok(name) => name,
                    Err(error) => return Some(Err(error)),
                };
                let schema = match state.catalog.table(&name) {
                    Ok(schema) => schema.clone(),
                    Err(_) => return None,
                };
                let table = state
                    .tables
                    .get(&schema.id)
                    .expect("catalog table must have storage")
                    .clone();
                Some(Ok(DdlUndo::RestoreDropped(schema, table)))
            })
            .collect(),
        _ => Ok(Vec::new()),
    }
}

fn invalid_lock_timeout() -> PgError {
    PgError::new(
        SqlState::InvalidParameterValue,
        "invalid value for parameter lock_timeout",
    )
}

fn parse_lock_timeout(expression: &Expr) -> Result<Duration> {
    let text = match expression {
        Expr::Value(value) => match &value.value {
            AstValue::Number(value, _) => value.as_str(),
            AstValue::SingleQuotedString(value) => value.trim(),
            _ => return Err(invalid_lock_timeout()),
        },
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

fn parse_timezone(expression: &Expr) -> Result<String> {
    let value = match expression {
        Expr::Value(value) => {
            let AstValue::SingleQuotedString(value) = &value.value else {
                return Err(PgError::new(
                    SqlState::InvalidParameterValue,
                    "invalid value for parameter TimeZone",
                ));
            };
            value
        }
        Expr::Identifier(sqlparser::ast::Ident { value, .. }) => value,
        _ => {
            return Err(PgError::new(
                SqlState::InvalidParameterValue,
                "invalid value for parameter TimeZone",
            ));
        }
    };
    // UTC and numeric offsets are accepted here. Named-zone interpretation is
    // intentionally validated by the timestamp input layer when it is used.
    if value.eq_ignore_ascii_case("utc") || value.parse::<chrono::FixedOffset>().is_ok() {
        Ok(value.to_string())
    } else {
        Err(PgError::new(
            SqlState::InvalidParameterValue,
            "invalid value for parameter TimeZone",
        ))
    }
}

fn lock_timeout_error() -> PgError {
    PgError::new(
        SqlState::LockNotAvailable,
        "canceling statement due to lock timeout",
    )
}

fn deadlock_error() -> PgError {
    PgError::new(SqlState::DeadlockDetected, "deadlock detected")
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
            TransactionMode::IsolationLevel(AstIsolationLevel::Snapshot) => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "SNAPSHOT isolation is not implemented",
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
    context: &executor::ExecutionContext,
) -> Result<(MutexGuard<'a, DatabaseState>, Snapshot)> {
    let deadline = (timeout != Duration::ZERO).then(|| Instant::now() + timeout);
    loop {
        let required = executor::required_row_locks(&state, statement, xid, &snapshot, context)?;
        let mut blocked = None;
        for required_lock in required {
            match state
                .row_locks
                .acquire(required_lock.key, xid, required_lock.mode)
            {
                LockAttempt::Acquired => condvar.notify_all(),
                LockAttempt::Blocked(conflicts) => {
                    if state.wait_for.wait_for(xid, &conflicts).is_some() {
                        condvar.notify_all();
                    }
                    blocked = Some((required_lock.key, conflicts));
                    break;
                }
            }
        }
        let Some((key, conflicts)) = blocked else {
            state.wait_for.clear_wait(xid);
            return Ok((state, snapshot));
        };
        if state.wait_for.take_victim(xid) {
            state.row_locks.cancel_wait(key, xid);
            state.wait_for.clear_wait(xid);
            return Err(deadlock_error());
        }
        let mut timed_out = false;
        state = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.row_locks.cancel_wait(key, xid);
                state.wait_for.clear_wait(xid);
                return Err(lock_timeout_error());
            }
            let (state, wait_result) = condvar
                .wait_timeout(state, remaining)
                .expect("database mutex is poisoned");
            timed_out = wait_result.timed_out();
            state
        } else {
            condvar.wait(state).expect("database mutex is poisoned")
        };
        state.row_locks.cancel_wait(key, xid);
        state.wait_for.clear_wait(xid);
        if state.wait_for.take_victim(xid) {
            return Err(deadlock_error());
        }
        if timed_out {
            return Err(lock_timeout_error());
        }
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
            mock_time: false,
            seed: None,
        }
    }
    pub fn session(&self) -> Session {
        Session {
            db: self.clone(),
            transaction: None,
            default_isolation: IsolationLevel::ReadCommitted,
            lock_timeout: self.default_lock_timeout,
            timezone: "UTC".into(),
            ddl_undo: Vec::new(),
            settings_undo: None,
            deferred_constraints: BTreeSet::new(),
            defer_all_constraints: false,
            deferred_foreign_keys_dirty: false,
        }
    }
}
impl DbBuilder {
    pub fn lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
    /// Enable a frozen, deterministic database clock. It begins at the Unix
    /// epoch and can subsequently be controlled through `Db::set_time` and
    /// `Db::advance_time`.
    pub fn mock_time(mut self, enabled: bool) -> Self {
        self.mock_time = enabled;
        self
    }
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }
    pub fn build(self) -> Db {
        Db {
            state: Arc::new(Mutex::new(DatabaseState::new())),
            condvar: Arc::new(Condvar::new()),
            default_lock_timeout: self.lock_timeout,
            clock: Arc::new(Mutex::new(if self.mock_time {
                Clock::Mock(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
            } else {
                Clock::Real
            })),
            rng: Arc::new(Mutex::new(match self.seed {
                Some(seed) => ChaCha12Rng::seed_from_u64(seed),
                None => ChaCha12Rng::from_os_rng(),
            })),
        }
    }
}
impl Default for Db {
    fn default() -> Self {
        Self::new()
    }
}
impl Db {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        match *self.clock.lock().expect("clock mutex is poisoned") {
            Clock::Real => chrono::Utc::now(),
            Clock::Mock(value) => value,
        }
    }

    /// Set the frozen mock clock. Real-clock databases reject the operation.
    pub fn set_time(&self, time: chrono::DateTime<chrono::Utc>) -> Result<()> {
        let mut clock = self.clock.lock().expect("clock mutex is poisoned");
        match &mut *clock {
            Clock::Mock(value) => {
                *value = time;
                Ok(())
            }
            Clock::Real => Err(PgError::new(
                SqlState::InvalidParameterValue,
                "mock time is disabled",
            )),
        }
    }

    /// Advance the frozen mock clock by `duration`. Real-clock databases reject
    /// the operation.
    pub fn advance_time(&self, duration: chrono::Duration) -> Result<()> {
        let mut clock = self.clock.lock().expect("clock mutex is poisoned");
        match &mut *clock {
            Clock::Mock(value) => {
                *value = value.checked_add_signed(duration).ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "clock time out of range")
                })?;
                Ok(())
            }
            Clock::Real => Err(PgError::new(
                SqlState::InvalidParameterValue,
                "mock time is disabled",
            )),
        }
    }
}
impl Session {
    pub fn execute(&mut self, sql: &str) -> Result<Vec<StatementResult>> {
        if let Some(result) = self.set_constraints(sql) {
            return result.map(|result| vec![result]);
        }
        let statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => return self.failed(error),
        };
        let mut results = Vec::with_capacity(statements.len());
        for statement in statements {
            if self.transaction.is_none() {
                self.start_transaction(self.default_isolation, true);
            }
            match self.run_statement(statement) {
                Ok(result) => results.push(result),
                Err(error) => {
                    if self.transaction_is_implicit_batch() {
                        let _ = self.finish_transaction(false);
                    }
                    return Err(error);
                }
            }
        }
        if self.transaction_is_implicit_batch() {
            self.finish_transaction(true)?;
        }
        Ok(results)
    }
    fn set_constraints(&mut self, sql: &str) -> Option<Result<StatementResult>> {
        let sql = sql.trim().trim_end_matches(';').trim();
        let upper = sql.to_ascii_uppercase();
        let rest = upper.strip_prefix("SET CONSTRAINTS ")?;
        let deferred = if rest.strip_suffix(" DEFERRED").is_some() {
            true
        } else if rest.strip_suffix(" IMMEDIATE").is_some() {
            false
        } else {
            return Some(Err(PgError::new(
                SqlState::SyntaxError,
                "SET CONSTRAINTS requires DEFERRED or IMMEDIATE",
            )));
        };
        let names = rest
            .strip_suffix(if deferred { " DEFERRED" } else { " IMMEDIATE" })
            .expect("suffix was checked");
        if self.transaction.is_none() {
            self.start_transaction(self.default_isolation, true);
        }
        if matches!(self.transaction, Some(SessionTransaction::Aborted { .. })) {
            return Some(Err(PgError::new(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            )));
        }
        let requested = if names.trim() == "ALL" {
            None
        } else {
            Some(
                names
                    .split(',')
                    .map(|name| name.trim().trim_matches('"').to_ascii_lowercase())
                    .collect::<Vec<_>>(),
            )
        };
        let state = self.db.state.lock().expect("database mutex is poisoned");
        let constraints = state
            .catalog
            .tables()
            .flat_map(|schema| schema.constraints.iter())
            .filter_map(|constraint| match constraint {
                crate::catalog::Constraint::ForeignKey(foreign_key) => Some(foreign_key),
                _ => None,
            })
            .collect::<Vec<_>>();
        let all_requested = requested.is_none();
        let selected = match requested {
            None => constraints.into_iter().cloned().collect(),
            Some(names) => {
                let selected = names
                    .iter()
                    .map(|name| {
                        constraints
                            .iter()
                            .find(|foreign_key| foreign_key.name == *name)
                            .map(|foreign_key| (*foreign_key).clone())
                            .ok_or_else(|| {
                                PgError::new(
                                    SqlState::UndefinedObject,
                                    format!("constraint {name:?} does not exist"),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>();
                match selected {
                    Ok(selected) => selected,
                    Err(error) => {
                        drop(state);
                        return Some(self.failed(error));
                    }
                }
            }
        };
        if selected.iter().any(|foreign_key| !foreign_key.deferrable) {
            drop(state);
            return Some(self.failed(PgError::new(
                SqlState::FeatureNotSupported,
                "constraint is not deferrable",
            )));
        }
        drop(state);
        if all_requested {
            self.defer_all_constraints = deferred;
            self.deferred_constraints.clear();
        } else {
            for foreign_key in selected {
                if deferred {
                    self.deferred_constraints.insert(foreign_key.name.clone());
                } else {
                    self.deferred_constraints.remove(&foreign_key.name);
                }
            }
        }
        if !deferred && self.deferred_foreign_keys_dirty {
            let state = self.db.state.lock().expect("database mutex is poisoned");
            let xid = match self.transaction {
                Some(SessionTransaction::Active(transaction)) => transaction.xid,
                _ => unreachable!(),
            };
            if let Err(error) = executor::validate_deferred_foreign_keys(&state, xid) {
                drop(state);
                return Some(self.failed(error));
            }
        }
        if self.transaction_is_implicit_batch() {
            return Some(
                self.finish_transaction(true)
                    .map(|()| StatementResult::Affected(0)),
            );
        }
        Some(Ok(StatementResult::Affected(0)))
    }
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        let statement = self.prepare(sql)?;
        self.execute_prepared(&statement, params)
    }
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let statement = self.prepare(sql)?;
        self.query_prepared(&statement, params)
    }
    pub fn prepare(&mut self, sql: &str) -> Result<Statement> {
        let mut statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => return self.failed(error),
        };
        if statements.len() != 1 {
            return self.failed(PgError::new(
                SqlState::SyntaxError,
                "prepared statements require exactly one statement",
            ));
        }
        let statement = statements.pop().expect("statement count was checked");
        if matches!(self.transaction, Some(SessionTransaction::Aborted { .. }))
            && !matches!(
                &statement,
                parser::Statement::Commit { .. } | parser::Statement::Rollback { .. }
            )
        {
            return Err(PgError::new(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        let prepared = {
            let state = self.db.state.lock().expect("database mutex is poisoned");
            analyzer::parameter_types(&statement, &state.catalog).and_then(|parameter_types| {
                let described = analyzer::bind(
                    &statement,
                    &parameter_types,
                    &vec![Value::Null; parameter_types.len()],
                )?;
                let columns = executor::query_columns(&state, &described)?;
                Ok((parameter_types, columns))
            })
        };
        match prepared {
            Ok((parameter_types, columns)) => Ok(Statement {
                statement,
                parameter_types,
                columns,
            }),
            Err(error) => self.failed(error),
        }
    }
    pub fn execute_prepared(&mut self, statement: &Statement, params: &[Value]) -> Result<u64> {
        match self.run_prepared(statement, params)? {
            StatementResult::Affected(rows) => Ok(rows),
            StatementResult::Query(_) => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "use query_prepared for SELECT statements",
            )),
        }
    }
    pub fn query_prepared(
        &mut self,
        statement: &Statement,
        params: &[Value],
    ) -> Result<QueryResult> {
        match self.run_prepared(statement, params)? {
            StatementResult::Query(result) => Ok(result),
            StatementResult::Affected(_) => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "query_prepared requires a SELECT statement",
            )),
        }
    }
    pub fn run_prepared(
        &mut self,
        statement: &Statement,
        params: &[Value],
    ) -> Result<StatementResult> {
        let statement =
            match analyzer::bind(&statement.statement, &statement.parameter_types, params) {
                Ok(statement) => statement,
                Err(error) => return self.failed(error),
            };
        self.run_statement(statement)
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
        self.start_transaction(isolation, false);
        Ok(Transaction {
            session: self,
            finished: false,
        })
    }
    fn start_transaction(&mut self, isolation: IsolationLevel, implicit_batch: bool) {
        assert!(self.ddl_undo.is_empty());
        assert!(self.settings_undo.is_none());
        self.deferred_constraints.clear();
        self.defer_all_constraints = false;
        self.deferred_foreign_keys_dirty = false;
        self.settings_undo = Some((
            self.default_isolation,
            self.lock_timeout,
            self.timezone.clone(),
        ));
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        self.transaction = Some(SessionTransaction::Active(ActiveTransaction {
            xid: state.transactions.begin(),
            isolation,
            snapshot: None,
            statement_started: false,
            implicit_batch,
            transaction_timestamp: self.db.now(),
        }));
    }
    fn finish_transaction(&mut self, commit: bool) -> Result<()> {
        let Some(transaction) = self.transaction.take() else {
            return Ok(());
        };
        let xid = match transaction {
            SessionTransaction::Active(transaction) if commit => {
                let state_lock = self.db.state.clone();
                let mut state = state_lock.lock().expect("database mutex is poisoned");
                if self.deferred_foreign_keys_dirty
                    && let Err(error) =
                        executor::validate_deferred_foreign_keys(&state, transaction.xid)
                {
                    self.rollback_ddl(&mut state);
                    if let Some((default_isolation, lock_timeout, timezone)) =
                        self.settings_undo.take()
                    {
                        self.default_isolation = default_isolation;
                        self.lock_timeout = lock_timeout;
                        self.timezone = timezone;
                    }
                    self.deferred_constraints.clear();
                    self.defer_all_constraints = false;
                    self.deferred_foreign_keys_dirty = false;
                    abort(&mut state, transaction.xid);
                    self.db.condvar.notify_all();
                    return Err(error);
                }
                state.transactions.commit(transaction.xid);
                state.row_locks.release(transaction.xid);
                state.wait_for.remove_transaction(transaction.xid);
                self.ddl_undo.clear();
                self.settings_undo = None;
                self.deferred_constraints.clear();
                self.defer_all_constraints = false;
                self.deferred_foreign_keys_dirty = false;
                self.db.condvar.notify_all();
                return Ok(());
            }
            SessionTransaction::Active(transaction) => transaction.xid,
            SessionTransaction::Aborted { xid, .. } => xid,
        };
        let state_lock = self.db.state.clone();
        let mut state = state_lock.lock().expect("database mutex is poisoned");
        self.rollback_ddl(&mut state);
        if let Some((default_isolation, lock_timeout, timezone)) = self.settings_undo.take() {
            self.default_isolation = default_isolation;
            self.lock_timeout = lock_timeout;
            self.timezone = timezone;
        }
        self.deferred_constraints.clear();
        self.defer_all_constraints = false;
        self.deferred_foreign_keys_dirty = false;
        abort(&mut state, xid);
        self.db.condvar.notify_all();
        Ok(())
    }
    fn abort_transaction(&mut self) {
        if let Some(SessionTransaction::Active(transaction)) = self.transaction {
            self.transaction = Some(SessionTransaction::Aborted {
                xid: transaction.xid,
                implicit_batch: transaction.implicit_batch,
            });
        }
    }
    fn transaction_is_implicit_batch(&self) -> bool {
        match self.transaction {
            Some(SessionTransaction::Active(transaction)) => transaction.implicit_batch,
            Some(SessionTransaction::Aborted { implicit_batch, .. }) => implicit_batch,
            None => false,
        }
    }
    fn rollback_ddl(&mut self, state: &mut DatabaseState) {
        for undo in self.ddl_undo.drain(..).rev() {
            match undo {
                DdlUndo::DropCreated(name) => {
                    if let Ok(schema) = state.catalog.drop_table(&name) {
                        state.tables.remove(&schema.id);
                    }
                }
                DdlUndo::RestoreDropped(schema, table) => {
                    state.tables.insert(schema.id, table);
                    state.catalog.restore_table(schema);
                }
            }
        }
    }
    fn failed<T>(&mut self, error: PgError) -> Result<T> {
        self.abort_transaction();
        Err(error)
    }
    fn run_statement(&mut self, statement: parser::Statement) -> Result<StatementResult> {
        match &statement {
            parser::Statement::Set(Set::SetTimeZone { local: _, value }) => {
                self.timezone = parse_timezone(value)?;
                return Ok(StatementResult::Affected(0));
            }
            parser::Statement::ShowVariable { variable }
                if variable.len() == 1 && variable[0].value.eq_ignore_ascii_case("timezone") =>
            {
                return Ok(StatementResult::Query(QueryResult {
                    columns: vec![ColumnMeta {
                        name: "TimeZone".into(),
                        type_oid: crate::value::BaseType::Text.oid(),
                        typmod: -1,
                    }],
                    rows: vec![vec![Value::Text(self.timezone.clone())]],
                }));
            }
            parser::Statement::StartTransaction { modes, .. } => {
                return match self.transaction {
                    None => {
                        let isolation =
                            isolation_from_modes(modes)?.unwrap_or(self.default_isolation);
                        self.start_transaction(isolation, false);
                        Ok(StatementResult::Affected(0))
                    }
                    Some(SessionTransaction::Active(mut transaction))
                        if transaction.implicit_batch =>
                    {
                        if let Some(isolation) = isolation_from_modes(modes)? {
                            if transaction.statement_started && isolation != transaction.isolation {
                                return self.failed(PgError::new(
                                    SqlState::ActiveSqlTransaction,
                                    "transaction isolation level must be set before any query",
                                ));
                            }
                            transaction.isolation = isolation;
                        }
                        transaction.implicit_batch = false;
                        self.transaction = Some(SessionTransaction::Active(transaction));
                        Ok(StatementResult::Affected(0))
                    }
                    Some(SessionTransaction::Active(_)) => Ok(StatementResult::Affected(0)),
                    Some(SessionTransaction::Aborted { .. }) => Err(PgError::new(
                        SqlState::InFailedSqlTransaction,
                        "current transaction is aborted",
                    )),
                };
            }
            parser::Statement::Set(Set::SetTransaction {
                modes,
                snapshot,
                session,
            }) => {
                if matches!(self.transaction, Some(SessionTransaction::Aborted { .. })) {
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
                    return Ok(StatementResult::Affected(0));
                }
                let Some(SessionTransaction::Active(mut transaction)) = self.transaction else {
                    return Ok(StatementResult::Affected(0));
                };
                if transaction.statement_started && isolation != transaction.isolation {
                    return self.failed(PgError::new(
                        SqlState::ActiveSqlTransaction,
                        "transaction isolation level must be set before any query",
                    ));
                }
                transaction.isolation = isolation;
                self.transaction = Some(SessionTransaction::Active(transaction));
                return Ok(StatementResult::Affected(0));
            }
            parser::Statement::Set(Set::SingleAssignment {
                scope,
                hivevar,
                variable,
                values,
            }) => {
                if variable.to_string().eq_ignore_ascii_case("timezone") {
                    if *hivevar || values.len() != 1 {
                        return self.failed(PgError::new(
                            SqlState::FeatureNotSupported,
                            "TimeZone setting variant is not implemented",
                        ));
                    }
                    self.timezone = parse_timezone(&values[0])?;
                    return Ok(StatementResult::Affected(0));
                }
                if variable.to_string().eq_ignore_ascii_case("lock_timeout") {
                    if matches!(self.transaction, Some(SessionTransaction::Aborted { .. })) {
                        return Err(PgError::new(
                            SqlState::InFailedSqlTransaction,
                            "current transaction is aborted",
                        ));
                    }
                    if *scope == Some(ContextModifier::Local) || *hivevar || values.len() != 1 {
                        return self.failed(PgError::new(
                            SqlState::FeatureNotSupported,
                            "lock_timeout setting variant is not implemented",
                        ));
                    }
                    self.lock_timeout = match parse_lock_timeout(&values[0]) {
                        Ok(timeout) => timeout,
                        Err(error) => return self.failed(error),
                    };
                    return Ok(StatementResult::Affected(0));
                }
            }
            parser::Statement::Commit { chain, .. } => {
                if *chain {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "COMMIT AND CHAIN is not implemented",
                    ));
                }
                self.finish_transaction(true)?;
                return Ok(StatementResult::Affected(0));
            }
            parser::Statement::Rollback { chain, savepoint } => {
                if *chain || savepoint.is_some() {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "ROLLBACK variant is not implemented",
                    ));
                }
                self.finish_transaction(false)?;
                return Ok(StatementResult::Affected(0));
            }
            _ => {}
        }
        if matches!(self.transaction, Some(SessionTransaction::Aborted { .. })) {
            return Err(PgError::new(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        if matches!(
            self.transaction,
            Some(SessionTransaction::Active(transaction)) if !transaction.implicit_batch
        ) && matches!(parser::classify(&statement), parser::StatementKind::Ddl)
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
            let context = executor::ExecutionContext {
                transaction_timestamp: transaction.transaction_timestamp,
                statement_timestamp: self.db.now(),
                clock_timestamp: self.db.now(),
                rng: self.db.rng.clone(),
            };
            if transaction.implicit_batch
                && matches!(parser::classify(&statement), parser::StatementKind::Ddl)
            {
                let undo = match ddl_undo_for_statement(&state, &statement) {
                    Ok(undo) => undo,
                    Err(error) => return self.failed(error),
                };
                self.ddl_undo.extend(undo);
            }
            let (mut state, snapshot) = match acquire_row_locks(
                &condvar,
                self.lock_timeout,
                state,
                &statement,
                transaction.xid,
                transaction.isolation,
                snapshot,
                &context,
            ) {
                Ok(acquired) => acquired,
                Err(error) => return self.failed(error),
            };
            return match executor::dispatch(
                &mut state,
                &statement,
                transaction.xid,
                &snapshot,
                &self.deferred_constraints,
                self.defer_all_constraints,
                &context,
            ) {
                Ok(result) => {
                    if contains_dml(&statement)
                        && executor::contains_deferred_foreign_keys(
                            &state,
                            &self.deferred_constraints,
                            self.defer_all_constraints,
                        )
                    {
                        self.deferred_foreign_keys_dirty = true;
                    }
                    Ok(result)
                }
                Err(error) => {
                    drop(state);
                    self.failed(error)
                }
            };
        }
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        let xid = state.transactions.begin();
        let snapshot = Snapshot::new(&state.transactions);
        let now = self.db.now();
        let context = executor::ExecutionContext {
            transaction_timestamp: now,
            statement_timestamp: now,
            clock_timestamp: now,
            rng: self.db.rng.clone(),
        };
        let (mut state, snapshot) = match acquire_row_locks(
            &self.db.condvar,
            self.lock_timeout,
            state,
            &statement,
            xid,
            self.default_isolation,
            snapshot,
            &context,
        ) {
            Ok(acquired) => acquired,
            Err(error) => {
                let mut state = self.db.state.lock().expect("database mutex is poisoned");
                abort(&mut state, xid);
                self.db.condvar.notify_all();
                return Err(error);
            }
        };
        match executor::dispatch(
            &mut state,
            &statement,
            xid,
            &snapshot,
            &self.deferred_constraints,
            self.defer_all_constraints,
            &context,
        ) {
            Ok(result) => {
                if contains_dml(&statement)
                    && executor::contains_deferred_foreign_keys(
                        &state,
                        &self.deferred_constraints,
                        self.defer_all_constraints,
                    )
                    && let Err(error) = executor::validate_deferred_foreign_keys(&state, xid)
                {
                    abort(&mut state, xid);
                    self.db.condvar.notify_all();
                    return Err(error);
                }
                state.transactions.commit(xid);
                state.row_locks.release(xid);
                state.wait_for.remove_transaction(xid);
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

fn contains_dml(statement: &parser::Statement) -> bool {
    matches!(
        statement,
        parser::Statement::Insert(_) | parser::Statement::Update(_) | parser::Statement::Delete(_)
    )
}

impl Statement {
    pub fn parameter_types(&self) -> &[crate::value::BaseType] {
        &self.parameter_types
    }

    pub fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }
}

impl Transaction<'_> {
    pub fn execute(&mut self, sql: &str) -> Result<Vec<StatementResult>> {
        self.session.execute(sql)
    }
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        self.session.execute_params(sql, params)
    }
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.session.query(sql, params)
    }
    pub fn prepare(&mut self, sql: &str) -> Result<Statement> {
        self.session.prepare(sql)
    }
    pub fn execute_prepared(&mut self, statement: &Statement, params: &[Value]) -> Result<u64> {
        self.session.execute_prepared(statement, params)
    }
    pub fn query_prepared(
        &mut self,
        statement: &Statement,
        params: &[Value],
    ) -> Result<QueryResult> {
        self.session.query_prepared(statement, params)
    }
    pub fn commit(mut self) -> Result<()> {
        self.session.finish_transaction(true)?;
        self.finished = true;
        Ok(())
    }
    pub fn rollback(mut self) -> Result<()> {
        self.session.finish_transaction(false)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.session.finish_transaction(false);
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

    fn affected(rows: u64) -> Vec<StatementResult> {
        vec![StatementResult::Affected(rows)]
    }

    #[test]
    fn foreign_keys_enforce_keys_and_keep_failed_multi_row_writes_atomic() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        session.execute("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents)").unwrap();
        let error = session
            .execute("INSERT INTO children VALUES (1, 99), (2, 99)")
            .unwrap_err();
        assert_eq!(error.sqlstate, SqlState::ForeignKeyViolation);
        assert!(
            session
                .query("SELECT * FROM children", &[])
                .unwrap()
                .rows
                .is_empty()
        );
        session.execute("INSERT INTO parents VALUES (99)").unwrap();
        session
            .execute("INSERT INTO children VALUES (1, 99)")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT parent_id FROM children", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(99)]]
        );
    }

    #[test]
    fn foreign_key_actions_apply_to_updates_and_deletes() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY, replacement INTEGER)")
            .unwrap();
        session.execute("CREATE TABLE cascade_children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE CASCADE ON UPDATE CASCADE)").unwrap();
        session.execute("CREATE TABLE null_children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parents(id) ON DELETE SET NULL ON UPDATE CASCADE)").unwrap();
        session.execute("CREATE TABLE default_children (id INTEGER PRIMARY KEY, parent_id INTEGER DEFAULT 7 REFERENCES parents(id) ON DELETE SET DEFAULT ON UPDATE CASCADE)").unwrap();
        session
            .execute("INSERT INTO parents VALUES (7, NULL), (1, NULL)")
            .unwrap();
        session
            .execute("INSERT INTO cascade_children VALUES (1, 1)")
            .unwrap();
        session
            .execute("INSERT INTO null_children VALUES (1, 1)")
            .unwrap();
        session
            .execute("INSERT INTO default_children VALUES (1, 1)")
            .unwrap();
        session
            .execute("UPDATE parents SET id = 2 WHERE id = 1")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT parent_id FROM cascade_children", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
        session.execute("DELETE FROM parents WHERE id = 2").unwrap();
        assert!(
            session
                .query("SELECT * FROM cascade_children", &[])
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            session
                .query("SELECT parent_id FROM null_children", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Null]]
        );
        assert_eq!(
            session
                .query("SELECT parent_id FROM default_children", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(7)]]
        );
    }

    #[test]
    fn deferred_foreign_keys_validate_at_commit_and_can_be_repaired() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        session.execute("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER CONSTRAINT children_parent_fkey REFERENCES parents DEFERRABLE INITIALLY DEFERRED)").unwrap();
        session.execute("BEGIN").unwrap();
        session
            .execute("INSERT INTO children VALUES (1, 2)")
            .unwrap();
        session.execute("INSERT INTO parents VALUES (2)").unwrap();
        session.execute("COMMIT").unwrap();
        session.execute("BEGIN").unwrap();
        session
            .execute("INSERT INTO children VALUES (3, 4)")
            .unwrap();
        let error = session.execute("COMMIT").unwrap_err();
        assert_eq!(error.sqlstate, SqlState::ForeignKeyViolation);
        assert_eq!(
            session.query("SELECT id FROM children", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    fn set_constraints_changes_deferrable_foreign_key_timing() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        session.execute("CREATE TABLE children (id INTEGER PRIMARY KEY, parent_id INTEGER CONSTRAINT children_parent_fkey REFERENCES parents DEFERRABLE)").unwrap();
        session.execute("BEGIN").unwrap();
        session
            .execute("SET CONSTRAINTS children_parent_fkey DEFERRED")
            .unwrap();
        session
            .execute("INSERT INTO children VALUES (1, 2)")
            .unwrap();
        session.execute("INSERT INTO parents VALUES (2)").unwrap();
        session.execute("SET CONSTRAINTS ALL IMMEDIATE").unwrap();
        session.execute("COMMIT").unwrap();
    }

    #[test]
    fn self_references_and_match_simple_nulls_are_valid() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE nodes (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES nodes)",
            )
            .unwrap();
        session.execute("INSERT INTO nodes VALUES (1, 1)").unwrap();
        session
            .execute("CREATE TABLE parents (first_id INTEGER, second_id INTEGER, PRIMARY KEY (first_id, second_id))")
            .unwrap();
        session
            .execute("CREATE TABLE children (id INTEGER PRIMARY KEY, first_id INTEGER, second_id INTEGER, FOREIGN KEY (first_id, second_id) REFERENCES parents(first_id, second_id))")
            .unwrap();
        session
            .execute("INSERT INTO children VALUES (1, NULL, 2), (2, 1, NULL), (3, NULL, NULL)")
            .unwrap();
    }

    #[test]
    fn uuid_values_parse_compare_and_generate() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id UUID PRIMARY KEY)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES ('{A0EEBC99-9C0B-4EF8-BBA9-6A6C0F3B0AF7}')")
            .unwrap();
        assert_eq!(
            session
                .query(
                    "SELECT id FROM items WHERE id = 'a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7'",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Uuid(
                uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bba9-6a6c0f3b0af7").unwrap()
            )]]
        );
        let generated = session
            .query("SELECT gen_random_uuid(), uuidv4() FROM items", &[])
            .unwrap();
        assert!(matches!(generated.rows[0][0], Value::Uuid(_)));
        assert_ne!(generated.rows[0][0], generated.rows[0][1]);
    }

    #[test]
    fn seeded_uuid_generation_is_reproducible_and_supports_v7() {
        let initial = chrono::DateTime::parse_from_rfc3339("2024-02-29T12:34:56Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let generate = |db: &Db| {
            db.set_time(initial).unwrap();
            let mut session = db.session();
            session.execute("CREATE TABLE source (id INTEGER)").unwrap();
            session.execute("INSERT INTO source VALUES (1)").unwrap();
            session
                .query(
                    "SELECT gen_random_uuid(), uuidv4(), uuidv7() FROM source",
                    &[],
                )
                .unwrap()
                .rows
        };
        let first = generate(&Db::builder().mock_time(true).seed(42).build());
        let second = generate(&Db::builder().mock_time(true).seed(42).build());
        assert_eq!(first, second);
        let Value::Uuid(v4) = first[0][0] else {
            panic!("uuid generator must return uuid")
        };
        let Value::Uuid(v7) = first[0][2] else {
            panic!("uuidv7 must return uuid")
        };
        assert_eq!(v4.get_version(), Some(uuid::Version::Random));
        assert_eq!(v7.get_version(), Some(uuid::Version::SortRand));
    }

    #[test]
    fn timestamp_values_and_timezone_setting_work() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE events (plain TIMESTAMP(3), instant TIMESTAMPTZ)")
            .unwrap();
        session
            .execute("INSERT INTO events VALUES ('2024-02-29 12:34:56.789123', '2024-02-29T12:34:56+03:00')")
            .unwrap();
        let result = session
            .query("SELECT plain, instant FROM events", &[])
            .unwrap();
        assert_eq!(
            result.columns[0].type_oid,
            crate::value::BaseType::Timestamp.oid()
        );
        assert_eq!(
            result.columns[1].type_oid,
            crate::value::BaseType::TimestampTz.oid()
        );
        assert_eq!(result.rows[0][0].to_text(), "2024-02-29 12:34:56.789");
        assert_eq!(result.rows[0][1].to_text(), "2024-02-29 09:34:56+00");
        session.execute("SET TIME ZONE 'UTC'").unwrap();
        assert_eq!(
            session.query("SHOW TimeZone", &[]).unwrap().rows,
            vec![vec![Value::Text("UTC".into())]]
        );
    }

    #[test]
    fn intervals_preserve_calendar_and_clock_parts() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE events (started TIMESTAMP, duration INTERVAL)")
            .unwrap();
        session
            .execute("INSERT INTO events VALUES ('2024-01-31 12:00:00', '1 month 2 days 03:04:05')")
            .unwrap();
        let result = session
            .query("SELECT started + duration, duration * 2 FROM events", &[])
            .unwrap();
        assert_eq!(
            result.columns[0].type_oid,
            crate::value::BaseType::Timestamp.oid()
        );
        assert_eq!(
            result.columns[1].type_oid,
            crate::value::BaseType::Interval.oid()
        );
        assert_eq!(result.rows[0][0].to_text(), "2024-03-02 15:04:05");
        assert_eq!(result.rows[0][1].to_text(), "2 mons 4 days 06:08:10");
    }

    #[test]
    fn mock_clock_is_frozen_and_publicly_controllable() {
        let db = Db::builder().mock_time(true).build();
        let initial = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        db.set_time(initial).unwrap();
        assert_eq!(db.now(), initial);
        db.advance_time(chrono::Duration::minutes(90)).unwrap();
        assert_eq!(db.now(), initial + chrono::Duration::minutes(90));
        assert!(Db::new().set_time(initial).is_err());
        assert!(
            Db::new()
                .advance_time(chrono::Duration::seconds(1))
                .is_err()
        );
    }

    #[test]
    fn timestamp_functions_observe_transaction_statement_and_clock_boundaries() {
        let db = Db::builder().mock_time(true).build();
        let initial = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        db.set_time(initial).unwrap();
        let mut session = db.session();
        session
            .execute("CREATE TABLE clock_source (id INTEGER)")
            .unwrap();
        session
            .execute("INSERT INTO clock_source VALUES (1)")
            .unwrap();
        session.execute("BEGIN").unwrap();
        let first = session
            .query(
                "SELECT now(), statement_timestamp(), clock_timestamp() FROM clock_source",
                &[],
            )
            .unwrap();
        db.advance_time(chrono::Duration::seconds(1)).unwrap();
        let second = session
            .query(
                "SELECT now(), statement_timestamp(), clock_timestamp() FROM clock_source",
                &[],
            )
            .unwrap();
        assert_eq!(first.rows[0][0], second.rows[0][0]);
        assert_ne!(first.rows[0][1], second.rows[0][1]);
        assert_ne!(first.rows[0][2], second.rows[0][2]);
        session.execute("COMMIT").unwrap();
    }

    #[test]
    fn date_and_time_values_preserve_postgres_special_forms() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE values_table (day DATE, moment TIME(6))")
            .unwrap();
        session
            .execute("INSERT INTO values_table VALUES ('infinity', '24:00:00')")
            .unwrap();
        let result = session
            .query("SELECT day, moment FROM values_table", &[])
            .unwrap();
        assert_eq!(result.rows[0][0].to_text(), "infinity");
        assert_eq!(result.rows[0][1].to_text(), "24:00:00");
        assert_eq!(result.columns[0].type_oid, BaseType::Date.oid());
        assert_eq!(result.columns[1].type_oid, BaseType::Time.oid());
    }

    #[test]
    fn match_full_rejects_partially_null_composite_keys() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE parents (first_id INTEGER, second_id INTEGER, PRIMARY KEY (first_id, second_id))")
            .unwrap();
        session
            .execute("CREATE TABLE children (id INTEGER PRIMARY KEY, first_id INTEGER, second_id INTEGER, FOREIGN KEY (first_id, second_id) REFERENCES parents(first_id, second_id) MATCH FULL)")
            .unwrap();
        session
            .execute("INSERT INTO children VALUES (1, NULL, NULL)")
            .unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO children VALUES (2, NULL, 1)")
                .unwrap_err()
                .sqlstate,
            SqlState::ForeignKeyViolation
        );
    }

    #[test]
    fn autocommit_creates_and_drops_tables() {
        let db = Db::new();
        let mut session = db.session();
        assert_eq!(session.execute("CREATE TABLE items (id INTEGER NOT NULL, name VARCHAR(12), amount NUMERIC(8, 2))").unwrap(), affected(0));
        let state = db.state.lock().unwrap();
        let table = state.catalog.table("items").unwrap();
        assert_eq!(table.columns[0].data_type.base, BaseType::Int4);
        assert_eq!(table.columns[1].data_type.typmod, 16);
        assert_eq!(table.columns[2].data_type.typmod, (8 << 16) + 2 + 4);
        drop(state);
        assert_eq!(session.execute("DROP TABLE items").unwrap(), affected(1));
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
    fn parameters_and_prepared_statements_bind_typed_values() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, name TEXT, amount SMALLINT)")
            .unwrap();

        let insert = session
            .prepare("INSERT INTO items VALUES ($1, $2, $3)")
            .unwrap();
        assert_eq!(
            session.execute_prepared(
                &insert,
                &[Value::Int4(1), Value::Text("first".into()), Value::Int2(10),],
            ),
            Ok(1)
        );
        assert_eq!(
            session.execute_prepared(
                &insert,
                &[
                    Value::Int4(2),
                    Value::Text("second".into()),
                    Value::Int2(20),
                ],
            ),
            Ok(1)
        );
        assert_eq!(
            session.execute_params(
                "UPDATE items SET amount = $1 WHERE id = $2",
                &[Value::Int2(11), Value::Int4(1)],
            ),
            Ok(1)
        );

        let select = session
            .prepare("SELECT name, amount FROM items WHERE id = $1")
            .unwrap();
        assert_eq!(
            session
                .query_prepared(&select, &[Value::Int4(1)])
                .unwrap()
                .rows,
            vec![vec![Value::Text("first".into()), Value::Int2(11)]]
        );
        assert_eq!(
            session
                .query_prepared(&select, &[Value::Int4(2)])
                .unwrap()
                .rows,
            vec![vec![Value::Text("second".into()), Value::Int2(20)]]
        );
        assert!(
            session
                .query(
                    "SELECT id FROM items WHERE name = $1 AND amount = $2",
                    &[Value::Text("missing".into()), Value::Null],
                )
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[test]
    fn parameter_validation_matches_prepared_statement_contract() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();
        let skipped = session
            .prepare("SELECT id FROM items WHERE id = $2 OR id = $2")
            .unwrap();

        assert_eq!(
            session
                .query_prepared(&skipped, &[Value::Text("unused".into())])
                .unwrap_err()
                .sqlstate,
            SqlState::ProtocolViolation
        );
        assert_eq!(
            session
                .query_prepared(
                    &skipped,
                    &[Value::Text("unused".into()), Value::Text("wrong".into())],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::CannotCoerce
        );
        assert_eq!(
            session
                .query_prepared(&skipped, &[Value::Text("unused".into()), Value::Int4(1)],)
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query("SELECT id FROM items WHERE id = $1", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::ProtocolViolation
        );
        assert_eq!(
            session
                .execute_params(
                    "INSERT INTO items VALUES ($1); INSERT INTO items VALUES ($1)",
                    &[Value::Int4(2)],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::SyntaxError
        );
        assert_eq!(
            session
                .prepare("SELECT missing FROM items WHERE id = $1")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedColumn
        );
        assert_eq!(
            session
                .prepare("SELECT id + TRUE FROM items WHERE id = $1")
                .unwrap_err()
                .sqlstate,
            SqlState::DatatypeMismatch
        );
        assert_eq!(
            session
                .prepare("SELECT id FROM missing WHERE id = $1")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session
                .prepare("SELECT id FROM items WHERE id = $0")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedParameter
        );

        let prepared = session
            .prepare("SELECT id FROM items WHERE id = $1")
            .unwrap();
        session.execute("DROP TABLE items").unwrap();
        assert_eq!(
            session
                .query_prepared(&prepared, &[Value::Int4(1)])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    fn execute_returns_each_multi_statement_result() {
        let db = Db::new();
        let mut session = db.session();

        let results = session
            .execute(
                "CREATE TABLE items (id INTEGER, name TEXT); \
                 INSERT INTO items VALUES (1, 'one'), (2, 'two'); \
                 UPDATE items SET name = upper(name) WHERE id = 2; \
                 SELECT id, name FROM items ORDER BY id",
            )
            .unwrap();

        assert_eq!(
            results,
            vec![
                StatementResult::Affected(0),
                StatementResult::Affected(2),
                StatementResult::Affected(1),
                StatementResult::Query(QueryResult {
                    columns: vec![
                        ColumnMeta {
                            name: "id".into(),
                            type_oid: BaseType::Int4.oid(),
                            typmod: -1,
                        },
                        ColumnMeta {
                            name: "name".into(),
                            type_oid: BaseType::Text.oid(),
                            typmod: -1,
                        },
                    ],
                    rows: vec![
                        vec![Value::Int4(1), Value::Text("one".into())],
                        vec![Value::Int4(2), Value::Text("TWO".into())],
                    ],
                }),
            ]
        );
        assert!(session.execute(" ; ; ").unwrap().is_empty());
    }

    #[test]
    fn implicit_batches_roll_back_and_stop_at_first_error() {
        let db = Db::new();
        let mut session = db.session();
        let original_timeout = session.lock_timeout;
        assert_eq!(
            session
                .execute(
                    "SET lock_timeout = '2s'; \
                     CREATE TABLE discarded (id INTEGER); \
                     INSERT INTO discarded VALUES (1); \
                     INSERT INTO discarded VALUES ('bad'); \
                     INSERT INTO discarded VALUES (2)",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTextRepresentation
        );
        assert_eq!(session.lock_timeout, original_timeout);
        assert_eq!(
            session
                .query("SELECT * FROM discarded", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );

        session.execute("CREATE TABLE kept (id INTEGER)").unwrap();
        assert_eq!(
            session
                .execute(
                    "INSERT INTO kept VALUES (1); \
                     INSERT INTO kept VALUES ('bad'); \
                     INSERT INTO kept VALUES (2)",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTextRepresentation
        );
        assert!(
            session
                .query("SELECT * FROM kept", &[])
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[test]
    fn explicit_controls_split_simple_query_transactions_like_postgres() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();

        assert_eq!(
            session
                .execute(
                    "INSERT INTO items VALUES (1); \
                     BEGIN; \
                     INSERT INTO items VALUES (2); \
                     COMMIT; \
                     INSERT INTO items VALUES (3); \
                     INSERT INTO items VALUES ('bad')",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTextRepresentation
        );
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
        );

        assert_eq!(
            session
                .execute(
                    "BEGIN; \
                     INSERT INTO items VALUES (4); \
                     INSERT INTO items VALUES ('bad'); \
                     ROLLBACK",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTextRepresentation
        );
        assert_eq!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InFailedSqlTransaction
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
        );

        assert_eq!(
            session
                .execute("INSERT INTO items VALUES (5); COMMIT; SELCT missing")
                .unwrap_err()
                .sqlstate,
            SqlState::SyntaxError
        );
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
        );
    }

    #[test]
    fn query_metadata_covers_every_phase_one_type() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE types (
                    flag BOOLEAN,
                    small_value SMALLINT,
                    int_value INTEGER,
                    big_value BIGINT,
                    real_value REAL,
                    double_value DOUBLE PRECISION,
                    numeric_value NUMERIC(5, 2),
                    text_value TEXT,
                    varying_value VARCHAR(3),
                    fixed_value CHAR(2),
                    bytes BYTEA
                )",
            )
            .unwrap();

        let metadata = session.query("SELECT * FROM types", &[]).unwrap().columns;
        assert_eq!(
            metadata
                .iter()
                .map(|column| (column.type_oid, column.typmod))
                .collect::<Vec<_>>(),
            vec![
                (BaseType::Bool.oid(), -1),
                (BaseType::Int2.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Int8.oid(), -1),
                (BaseType::Float4.oid(), -1),
                (BaseType::Float8.oid(), -1),
                (BaseType::Numeric.oid(), (5 << 16) + 2 + 4),
                (BaseType::Text.oid(), -1),
                (BaseType::Varchar.oid(), 3 + 4),
                (BaseType::Bpchar.oid(), 2 + 4),
                (BaseType::Bytea.oid(), -1),
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
            Ok(affected(1))
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
            Ok(affected(1))
        );
        handle.join().unwrap();
        assert_eq!(
            first.query("SELECT amount FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
    }

    #[test]
    fn deadlock_aborts_newest_transaction_and_survivor_proceeds() {
        let db = Db::builder().lock_timeout(Duration::from_secs(2)).build();
        let mut setup = db.session();
        let mut first = db.session();
        let mut second = db.session();
        setup
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        setup
            .execute("INSERT INTO items VALUES (1, 0), (2, 0)")
            .unwrap();

        first.execute("BEGIN").unwrap();
        first
            .execute("UPDATE items SET amount = 10 WHERE id = 1")
            .unwrap();
        second.execute("BEGIN").unwrap();
        second
            .execute("UPDATE items SET amount = 20 WHERE id = 2")
            .unwrap();

        let (victim_sender, victim_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let error = second
                .execute("UPDATE items SET amount = 11 WHERE id = 1")
                .unwrap_err();
            let failed = second.query("SELECT * FROM items", &[]).unwrap_err();
            second.execute("ROLLBACK").unwrap();
            victim_sender
                .send((error.sqlstate, failed.sqlstate))
                .unwrap();
        });
        wait_until_blocked(&db);

        assert_eq!(
            first.execute("UPDATE items SET amount = 1 WHERE id = 2"),
            Ok(affected(1))
        );
        assert_eq!(
            victim_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            (SqlState::DeadlockDetected, SqlState::InFailedSqlTransaction)
        );
        handle.join().unwrap();
        first.execute("COMMIT").unwrap();
        assert_eq!(
            setup
                .query("SELECT amount FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(10)], vec![Value::Int4(1)]]
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
            Ok(affected(1))
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
            Ok(affected(1))
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
        assert_eq!(session.execute("DELETE FROM items").unwrap(), affected(1));
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
            affected(1)
        );
        assert_eq!(
            session.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Null],
            ]
        );
        assert_eq!(session.execute("DELETE FROM items").unwrap(), affected(2));
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
        assert_eq!(
            writer.execute("DELETE FROM items WHERE id = 1").unwrap(),
            affected(1)
        );
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
            affected(2)
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
