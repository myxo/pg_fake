use crate::{
    error::{PgError, Result, SqlState},
    executor,
    txn::Snapshot,
};

use super::{Session, SessionTransactionState, StatementResult};

impl Session {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn try_execute_set_constraints(
        &mut self,
        sql: &str,
    ) -> Option<Result<StatementResult>> {
        let sql = sql.trim().trim_end_matches(';').trim();
        let upper = sql.to_ascii_uppercase();
        let rest = upper.strip_prefix("SET CONSTRAINTS ")?;
        let deferred = if rest.strip_suffix(" DEFERRED").is_some() {
            true
        } else if rest.strip_suffix(" IMMEDIATE").is_some() {
            false
        } else {
            return Some(Err(PgError::create(
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
        if matches!(
            self.transaction,
            Some(SessionTransactionState::Aborted { .. })
        ) {
            return Some(Err(PgError::create(
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
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        let transaction = match self.transaction {
            Some(SessionTransactionState::Active(transaction)) => transaction,
            _ => unreachable!(),
        };
        let snapshot = transaction
            .snapshot
            .unwrap_or_else(|| Snapshot::create(&state.transactions))
            .use_command(crate::txn::CommandId(transaction.next_command_id));
        state.load_catalog(
            Some(transaction.xid),
            snapshot,
            Some(self.temporary_schema_id),
        );
        let constraints = state
            .catalog
            .iterate_tables()
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
                                PgError::create(
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
                        return Some(self.abort_with_error(error));
                    }
                }
            }
        };
        if selected.iter().any(|foreign_key| !foreign_key.deferrable) {
            drop(state);
            return Some(self.abort_with_error(PgError::create(
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
                    self.deferred_constraints.insert(foreign_key.id);
                } else {
                    self.deferred_constraints.remove(&foreign_key.id);
                }
            }
        }
        if !deferred && self.deferred_foreign_keys_dirty {
            let mut state = self.db.state.lock().expect("database mutex is poisoned");
            let transaction = match self.transaction {
                Some(SessionTransactionState::Active(transaction)) => transaction,
                _ => unreachable!(),
            };
            state.load_catalog(
                Some(transaction.xid),
                snapshot,
                Some(self.temporary_schema_id),
            );
            if let Err(error) = executor::validate_deferred_foreign_keys(&state, transaction.xid) {
                drop(state);
                return Some(self.abort_with_error(error));
            }
        }
        if self.is_transaction_implicit_batch() {
            return Some(
                self.commit_transaction()
                    .map(|()| StatementResult::Affected(0)),
            );
        }
        Some(Ok(StatementResult::Affected(0)))
    }
}
