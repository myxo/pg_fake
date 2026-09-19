use std::collections::{BTreeMap, BTreeSet};

use sqlparser::ast;

use crate::{
    advisory::{AdvisoryKey, AdvisoryMode},
    catalog::{ConstraintId, SchemaId},
    error::{PgError, Result, SqlState},
    txn::{CommandId, RelationLockMode, RowLockKey, RowLockMode, Snapshot},
};

use super::{Session, SessionSettings, SessionTransactionState, StatementResult};

pub(super) struct Savepoint {
    name: String,
    boundary: CommandId,
    settings: SessionSettings,
    settings_on_commit: SessionSettings,
    deferred_constraints: BTreeSet<ConstraintId>,
    defer_all_constraints: bool,
    deferred_foreign_keys_dirty: bool,
    row_locks: BTreeMap<RowLockKey, RowLockMode>,
    relation_locks: BTreeMap<String, RelationLockMode>,
    advisory_locks: BTreeSet<(AdvisoryKey, SchemaId, AdvisoryMode)>,
}

impl Session {
    pub(super) fn try_execute_savepoint_command(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<Option<StatementResult>> {
        let (name, rollback, create) = match statement {
            ast::Statement::Savepoint { name } => (name, false, true),
            ast::Statement::ReleaseSavepoint { name } => (name, false, false),
            ast::Statement::Rollback {
                savepoint: Some(name),
                chain: false,
            } => (name, true, false),
            _ => return Ok(None),
        };
        let transaction = match self.transaction {
            Some(SessionTransactionState::Active(transaction)) => transaction,
            Some(SessionTransactionState::Aborted { transaction }) if rollback => transaction,
            Some(SessionTransactionState::Aborted { .. }) => {
                return Err(PgError::create(
                    SqlState::InFailedSqlTransaction,
                    "current transaction is aborted",
                ));
            }
            None => {
                return Err(PgError::create(
                    SqlState::NoActiveSqlTransaction,
                    "savepoints require a transaction block",
                ));
            }
        };
        if transaction.implicit_batch {
            return self.abort_with_error(PgError::create(
                SqlState::NoActiveSqlTransaction,
                "savepoints require a transaction block",
            ));
        }
        let name = crate::executor::normalize_identifier(name);
        if create {
            let state = self.db.state.lock().expect("database mutex is poisoned");
            self.savepoints.push(Savepoint {
                name,
                boundary: CommandId(transaction.next_command_id),
                settings: self.capture_settings(),
                settings_on_commit: self
                    .settings_on_commit
                    .clone()
                    .expect("transaction has commit settings"),
                deferred_constraints: self.deferred_constraints.clone(),
                defer_all_constraints: self.defer_all_constraints,
                deferred_foreign_keys_dirty: self.deferred_foreign_keys_dirty,
                row_locks: state.row_locks.capture_transaction_locks(transaction.xid),
                relation_locks: state
                    .relation_locks
                    .capture_transaction_locks(transaction.xid),
                advisory_locks: state
                    .advisory_locks
                    .lock()
                    .expect("advisory lock mutex is poisoned")
                    .capture_transaction_locks(transaction.xid),
            });
        } else {
            let Some(index) = self
                .savepoints
                .iter()
                .rposition(|savepoint| savepoint.name == name)
            else {
                return self.abort_with_error(PgError::create(
                    SqlState::InvalidSavepointSpecification,
                    format!("savepoint {name:?} does not exist"),
                ));
            };
            if rollback {
                self.rollback_savepoint_state(index);
                self.savepoints.truncate(index + 1);
                self.transaction = Some(SessionTransactionState::Active(transaction));
            } else {
                self.savepoints.truncate(index);
            }
        }
        Ok(Some(StatementResult::Affected(0)))
    }

    pub(super) fn rollback_savepoint_state(&mut self, index: usize) {
        let transaction = match self.transaction.expect("savepoint has a transaction") {
            SessionTransactionState::Active(transaction)
            | SessionTransactionState::Aborted { transaction } => transaction,
        };
        let savepoint = &self.savepoints[index];
        let state_lock = self.db.state.clone();
        let mut state = state_lock.lock().expect("database mutex is poisoned");
        state.rollback_sequence_resets_since(transaction.xid, savepoint.boundary);
        let reclaimed = state
            .catalog_history
            .discard_versions_since(transaction.xid, savepoint.boundary);
        for table in reclaimed.tables {
            state.tables.remove(&table);
        }
        {
            let mut values = state
                .sequence_values
                .lock()
                .expect("sequence storage is poisoned");
            for sequence in reclaimed.sequences {
                values.remove(&sequence);
            }
        }
        for table in state.take_touched_tables(transaction.xid) {
            if let Some(storage) = state.tables.get_mut(&table) {
                storage.discard_versions_since(transaction.xid, savepoint.boundary);
                state.mark_table_touched(transaction.xid, table);
            }
        }
        state
            .row_locks
            .restore_transaction_locks(transaction.xid, &savepoint.row_locks);
        state
            .relation_locks
            .restore_transaction_locks(transaction.xid, &savepoint.relation_locks);
        state
            .advisory_locks
            .lock()
            .expect("advisory lock mutex is poisoned")
            .restore_transaction_locks(transaction.xid, &savepoint.advisory_locks);
        state.wait_for.clear_wait(transaction.xid);
        let snapshot = transaction
            .snapshot
            .unwrap_or_else(|| Snapshot::create(&state.transactions))
            .use_command(CommandId(transaction.next_command_id));
        state.load_catalog(
            Some(transaction.xid),
            snapshot,
            Some(self.temporary_schema_id),
        );
        self.settings_on_commit = Some(savepoint.settings_on_commit.clone());
        self.deferred_constraints = savepoint.deferred_constraints.clone();
        self.defer_all_constraints = savepoint.defer_all_constraints;
        self.deferred_foreign_keys_dirty = savepoint.deferred_foreign_keys_dirty;
        let settings = savepoint.settings.clone();
        self.restore_settings(settings);
        self.db.condvar.notify_all();
    }
}
