use sqlparser::ast;

use crate::{
    catalog::TablePersistence,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{self, DatabaseState},
    txn::{Snapshot, Xid},
    value::Value,
};

use super::{PreparedStatement, QueryResult, Session, StatementResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadCommitted,
    RepeatableRead,
}

#[derive(Clone, Copy)]
pub(super) enum SessionTransactionState {
    Active(ActiveTransaction),
    Aborted { xid: Xid, implicit_batch: bool },
}

#[derive(Clone, Copy)]
pub(super) struct ActiveTransaction {
    pub(super) xid: Xid,
    pub(super) isolation: IsolationLevel,
    pub(super) snapshot: Option<Snapshot>,
    pub(super) statement_started: bool,
    pub(super) read_only: bool,
    pub(super) next_command_id: u64,
    pub(super) implicit_batch: bool,
    pub(super) transaction_timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct Transaction<'session> {
    session: &'session mut Session,
    finished: bool,
}

impl Session {
    pub(super) fn try_execute_transaction_command(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<Option<StatementResult>> {
        match statement {
            ast::Statement::Lock(_)
                if matches!(
                    self.transaction,
                    Some(SessionTransactionState::Active(ActiveTransaction {
                        implicit_batch: true,
                        ..
                    }))
                ) =>
            {
                return self.abort_with_error(PgError::create(
                    SqlState::NoActiveSqlTransaction,
                    "LOCK TABLE can only be used in transaction blocks",
                ));
            }
            ast::Statement::StartTransaction { modes, .. } => {
                return match self.transaction {
                    None => {
                        let isolation =
                            parse_isolation_level(modes)?.unwrap_or(self.default_isolation);
                        self.start_transaction(isolation, false);
                        Ok(Some(StatementResult::Affected(0)))
                    }
                    Some(SessionTransactionState::Active(mut transaction))
                        if transaction.implicit_batch =>
                    {
                        if let Some(isolation) = parse_isolation_level(modes)? {
                            if transaction.statement_started && isolation != transaction.isolation {
                                return self.abort_with_error(PgError::create(
                                    SqlState::ActiveSqlTransaction,
                                    "transaction isolation level must be set before any query",
                                ));
                            }
                            transaction.isolation = isolation;
                        }
                        transaction.implicit_batch = false;
                        self.transaction = Some(SessionTransactionState::Active(transaction));
                        Ok(Some(StatementResult::Affected(0)))
                    }
                    Some(SessionTransactionState::Active(_)) => {
                        Ok(Some(StatementResult::Affected(0)))
                    }
                    Some(SessionTransactionState::Aborted { .. }) => Err(PgError::create(
                        SqlState::InFailedSqlTransaction,
                        "current transaction is aborted",
                    )),
                };
            }
            ast::Statement::Set(ast::Set::SetTransaction {
                modes,
                snapshot,
                session,
            }) => {
                if matches!(
                    self.transaction,
                    Some(SessionTransactionState::Aborted { .. })
                ) {
                    return Err(PgError::create(
                        SqlState::InFailedSqlTransaction,
                        "current transaction is aborted",
                    ));
                }
                if snapshot.is_some() {
                    return self.abort_with_error(PgError::create(
                        SqlState::FeatureNotSupported,
                        "transaction snapshots are not implemented",
                    ));
                }
                let isolation = match parse_isolation_level(modes) {
                    Ok(isolation) => isolation,
                    Err(error) => return self.abort_with_error(error),
                };
                let Some(isolation) = isolation else {
                    return self.abort_with_error(PgError::create(
                        SqlState::SyntaxError,
                        "transaction isolation level is required",
                    ));
                };
                if *session {
                    self.default_isolation = isolation;
                    self.settings_on_commit
                        .as_mut()
                        .expect("SET runs in a transaction")
                        .default_isolation = isolation;
                    return Ok(Some(StatementResult::Affected(0)));
                }
                let Some(SessionTransactionState::Active(mut transaction)) = self.transaction
                else {
                    return Ok(Some(StatementResult::Affected(0)));
                };
                if transaction.statement_started && isolation != transaction.isolation {
                    return self.abort_with_error(PgError::create(
                        SqlState::ActiveSqlTransaction,
                        "transaction isolation level must be set before any query",
                    ));
                }
                transaction.isolation = isolation;
                self.transaction = Some(SessionTransactionState::Active(transaction));
                return Ok(Some(StatementResult::Affected(0)));
            }
            ast::Statement::Commit { chain, .. } => {
                if *chain {
                    return self.abort_with_error(PgError::create(
                        SqlState::FeatureNotSupported,
                        "COMMIT AND CHAIN is not implemented",
                    ));
                }
                self.commit_transaction()?;
                return Ok(Some(StatementResult::Affected(0)));
            }
            ast::Statement::Rollback { chain, savepoint } => {
                if *chain || savepoint.is_some() {
                    return self.abort_with_error(PgError::create(
                        SqlState::FeatureNotSupported,
                        "ROLLBACK variant is not implemented",
                    ));
                }
                self.rollback_transaction()?;
                return Ok(Some(StatementResult::Affected(0)));
            }
            _ => {}
        }
        Ok(None)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn begin(&mut self) -> Result<Transaction<'_>> {
        self.begin_with(self.default_isolation)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn begin_with(&mut self, isolation: IsolationLevel) -> Result<Transaction<'_>> {
        if self.transaction.is_some() {
            return Err(PgError::create(
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

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn start_transaction(&mut self, isolation: IsolationLevel, implicit_batch: bool) {
        assert!(self.settings_undo.is_none());
        assert!(self.settings_on_commit.is_none());
        self.deferred_constraints.clear();
        self.defer_all_constraints = false;
        self.deferred_foreign_keys_dirty = false;
        let settings = self.capture_settings();
        self.settings_undo = Some(settings.clone());
        self.settings_on_commit = Some(settings);
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        self.transaction = Some(SessionTransactionState::Active(ActiveTransaction {
            xid: state.transactions.begin(),
            isolation,
            snapshot: None,
            statement_started: false,
            read_only: true,
            next_command_id: 0,
            implicit_batch,
            transaction_timestamp: self.db.read_clock(),
        }));
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn commit_transaction(&mut self) -> Result<()> {
        let Some(transaction) = self.transaction.take() else {
            return Ok(());
        };
        let SessionTransactionState::Active(mut transaction) = transaction else {
            return self.rollback_transaction_state(transaction);
        };
        let state_lock = self.db.state.clone();
        let mut state = state_lock.lock().expect("database mutex is poisoned");
        let snapshot = transaction
            .snapshot
            .unwrap_or_else(|| Snapshot::create(&state.transactions))
            .use_command(crate::txn::CommandId(transaction.next_command_id));
        state.load_catalog(
            Some(transaction.xid),
            snapshot,
            Some(self.temporary_schema_id),
        );
        let on_commit_drop = state
            .catalog
            .iterate_tables()
            .filter_map(|table| {
                matches!(
                    table.persistence,
                    TablePersistence::Temporary {
                        on_commit_drop: true
                    }
                )
                .then_some(table.id)
            })
            .collect::<Vec<_>>();
        if !on_commit_drop.is_empty() {
            let previous = state.catalog.clone();
            for table_id in on_commit_drop {
                state.catalog.drop_table_by_id(table_id)?;
                state.catalog.drop_owned_sequences(table_id);
            }
            state.record_catalog_changes(
                &previous,
                transaction.xid,
                crate::txn::CommandId(transaction.next_command_id),
            );
            transaction.read_only = false;
        }
        if transaction.read_only {
            assert!(!self.deferred_foreign_keys_dirty);
            assert!(!state.has_touched_tables(transaction.xid));
            state.transactions.finish_read_only(transaction.xid);
            state
                .relation_locks
                .release_transaction_locks(transaction.xid);
            state.wait_for.remove_transaction(transaction.xid);
            prune_database_versions(&mut state);
            self.settings_undo = None;
            let settings = self
                .settings_on_commit
                .take()
                .expect("active transaction has commit settings");
            self.restore_settings(settings);
            self.deferred_constraints.clear();
            self.defer_all_constraints = false;
            self.db.condvar.notify_all();
            return Ok(());
        }
        if self.deferred_foreign_keys_dirty
            && let Err(error) = executor::validate_deferred_foreign_keys(&state, transaction.xid)
        {
            let settings = self
                .settings_undo
                .take()
                .expect("active transaction has rollback settings");
            self.settings_on_commit = None;
            self.restore_settings(settings);
            self.deferred_constraints.clear();
            self.defer_all_constraints = false;
            self.deferred_foreign_keys_dirty = false;
            abort_database_transaction(&mut state, transaction.xid);
            self.db.condvar.notify_all();
            return Err(error);
        }
        let commit_seq = state.commit_loaded_catalog_transaction(
            transaction.xid,
            snapshot,
            Some(self.temporary_schema_id),
        );
        for table_id in state.take_touched_tables(transaction.xid) {
            let has_reclamation = state
                .tables
                .get_mut(&table_id)
                .expect("touched table must exist at commit")
                .commit_transaction_versions(transaction.xid, commit_seq);
            if has_reclamation {
                state.mark_table_reclaimable(table_id);
            }
        }
        prune_database_versions(&mut state);
        state.row_locks.release_transaction_locks(transaction.xid);
        state
            .relation_locks
            .release_transaction_locks(transaction.xid);
        state.wait_for.remove_transaction(transaction.xid);
        self.settings_undo = None;
        let settings = self
            .settings_on_commit
            .take()
            .expect("active transaction has commit settings");
        self.restore_settings(settings);
        self.deferred_constraints.clear();
        self.defer_all_constraints = false;
        self.deferred_foreign_keys_dirty = false;
        self.db.condvar.notify_all();
        Ok(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn rollback_transaction(&mut self) -> Result<()> {
        let Some(transaction) = self.transaction.take() else {
            return Ok(());
        };
        self.rollback_transaction_state(transaction)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rollback_transaction_state(&mut self, transaction: SessionTransactionState) -> Result<()> {
        let xid = match transaction {
            SessionTransactionState::Active(transaction) => transaction.xid,
            SessionTransactionState::Aborted { xid, .. } => xid,
        };
        let state_lock = self.db.state.clone();
        let mut state = state_lock.lock().expect("database mutex is poisoned");
        let snapshot = match transaction {
            SessionTransactionState::Active(transaction) => transaction
                .snapshot
                .unwrap_or_else(|| Snapshot::create(&state.transactions))
                .use_command(crate::txn::CommandId(transaction.next_command_id)),
            SessionTransactionState::Aborted { .. } => {
                Snapshot::create(&state.transactions).use_command(crate::txn::CommandId(u64::MAX))
            }
        };
        state.load_catalog(Some(xid), snapshot, Some(self.temporary_schema_id));
        let settings = self
            .settings_undo
            .take()
            .expect("active transaction has rollback settings");
        self.settings_on_commit = None;
        self.restore_settings(settings);
        self.deferred_constraints.clear();
        self.defer_all_constraints = false;
        self.deferred_foreign_keys_dirty = false;
        abort_database_transaction(&mut state, xid);
        self.db.condvar.notify_all();
        Ok(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn mark_transaction_aborted(&mut self) {
        if let Some(SessionTransactionState::Active(transaction)) = self.transaction {
            self.transaction = Some(SessionTransactionState::Aborted {
                xid: transaction.xid,
                implicit_batch: transaction.implicit_batch,
            });
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn is_transaction_implicit_batch(&self) -> bool {
        match self.transaction {
            Some(SessionTransactionState::Active(transaction)) => transaction.implicit_batch,
            Some(SessionTransactionState::Aborted { implicit_batch, .. }) => implicit_batch,
            None => false,
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn abort_with_error<T>(&mut self, error: PgError) -> Result<T> {
        self.mark_transaction_aborted();
        Err(error)
    }
}

impl Transaction<'_> {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute(&mut self, sql: &str) -> Result<Vec<StatementResult>> {
        self.session.execute(sql)
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        self.session.execute_params(sql, params)
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.session.query(sql, params)
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn prepare(&mut self, sql: &str) -> Result<PreparedStatement> {
        self.session.prepare(sql)
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_prepared(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<u64> {
        self.session.execute_prepared(statement, params)
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn query_prepared(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<QueryResult> {
        self.session.query_prepared(statement, params)
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn commit(mut self) -> Result<()> {
        self.session.commit_transaction()?;
        self.finished = true;
        Ok(())
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn rollback(mut self) -> Result<()> {
        self.session.rollback_transaction()?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.session.rollback_transaction();
        }
    }
}

impl Drop for Session {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn drop(&mut self) {
        if self.transaction.is_some() {
            let _ = self.rollback_transaction();
        }
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        let reclaimed = state
            .catalog_history
            .drop_temporary_schema(self.temporary_schema_id);
        for table_id in reclaimed.tables {
            state.tables.remove(&table_id);
        }
        let mut sequence_values = state
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");
        for sequence_id in reclaimed.sequences {
            sequence_values.remove(&sequence_id);
        }
        drop(sequence_values);
        let snapshot = Snapshot::create(&state.transactions);
        state.load_catalog(None, snapshot, None);
        self.db.condvar.notify_all();
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_isolation_level(modes: &[ast::TransactionMode]) -> Result<Option<IsolationLevel>> {
    let mut isolation = None;
    for mode in modes {
        let level = match mode {
            ast::TransactionMode::IsolationLevel(
                ast::TransactionIsolationLevel::ReadUncommitted
                | ast::TransactionIsolationLevel::ReadCommitted,
            ) => IsolationLevel::ReadCommitted,
            ast::TransactionMode::IsolationLevel(
                ast::TransactionIsolationLevel::RepeatableRead,
            ) => IsolationLevel::RepeatableRead,
            ast::TransactionMode::IsolationLevel(ast::TransactionIsolationLevel::Serializable) => {
                return reject_unsupported("SERIALIZABLE isolation is not implemented");
            }
            ast::TransactionMode::IsolationLevel(ast::TransactionIsolationLevel::Snapshot) => {
                return reject_unsupported("SNAPSHOT isolation is not implemented");
            }
            ast::TransactionMode::AccessMode(_) => {
                return reject_unsupported("transaction access modes are not implemented");
            }
        };
        if isolation.replace(level).is_some() {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "isolation level specified more than once",
            ));
        }
    }
    Ok(isolation)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn abort_database_transaction(state: &mut DatabaseState, xid: Xid) {
    let reclaimed = state.catalog_history.discard_transaction(xid);
    for table_id in reclaimed.tables {
        state.tables.remove(&table_id);
    }
    let mut sequence_values = state
        .sequence_values
        .lock()
        .expect("sequence storage is poisoned");
    for sequence_id in reclaimed.sequences {
        sequence_values.remove(&sequence_id);
    }
    drop(sequence_values);
    state.transactions.abort(xid);
    for table_id in state.take_touched_tables(xid) {
        if let Some(table) = state.tables.get_mut(&table_id) {
            table.discard_transaction_versions(xid);
        }
    }
    prune_database_versions(state);
    state.row_locks.release_transaction_locks(xid);
    state.relation_locks.release_transaction_locks(xid);
    state.wait_for.remove_transaction(xid);
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn prune_database_versions(state: &mut DatabaseState) {
    let horizon = state.transactions.find_reclamation_horizon();
    for table_id in state.collect_reclaimable_table_ids() {
        let Some(table) = state.tables.get_mut(&table_id) else {
            state.clear_table_reclaimable(table_id);
            continue;
        };
        table.prune_versions(horizon, &state.transactions);
        if !table.has_reclaimable_versions() {
            state.clear_table_reclaimable(table_id);
        }
    }
    let protected_tables = state.collect_touched_tables();
    let reclaimed = state
        .catalog_history
        .prune(horizon, &state.transactions, &protected_tables);
    for table_id in reclaimed.tables {
        state.tables.remove(&table_id);
    }
    let mut sequence_values = state
        .sequence_values
        .lock()
        .expect("sequence storage is poisoned");
    for sequence_id in reclaimed.sequences {
        sequence_values.remove(&sequence_id);
    }
}
