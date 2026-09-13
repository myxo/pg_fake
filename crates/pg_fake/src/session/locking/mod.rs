use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Condvar, MutexGuard},
    time::{Duration, Instant},
};

use sqlparser::ast;

use crate::{
    catalog::{ConstraintId, SchemaId},
    error::{PgError, Result, SqlState},
    executor::{self, DatabaseState},
    parser,
    txn::{
        RelationLockAttempt, RelationLockMode, RowLockAttempt, Snapshot, TransactionStatus, Xid,
    },
};

use super::{IsolationLevel, StatementResult, catalog_dependencies::CatalogDependency};

mod advisory;
pub(super) use advisory::acquire_advisory_lock;
mod ddl;
mod foreign_keys;
mod relations;

pub(super) use relations::collect_relation_locks;

#[derive(Clone, Copy)]
pub(super) enum RowLockTarget<'a> {
    Ctes(&'a ast::Statement),
    Statement(&'a ast::Statement),
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn acquire_relation_locks<'a>(
    condvar: &Condvar,
    timeout: Duration,
    statement_deadline: Option<Instant>,
    mut state: MutexGuard<'a, DatabaseState>,
    statement: &ast::Statement,
    prepared_dependencies: Option<&[CatalogDependency]>,
    prepared_locks: Option<&[(String, RelationLockMode)]>,
    xid: Xid,
    temporary_schema_id: SchemaId,
    isolation: IsolationLevel,
    mut snapshot: Snapshot,
) -> Result<(MutexGuard<'a, DatabaseState>, Snapshot)> {
    let ddl = matches!(parser::classify(statement), parser::StatementKind::Ddl);
    if !ddl && prepared_locks.is_some_and(|locks| state.relation_locks.can_reuse_locks(locks, xid))
    {
        state.wait_for.clear_wait(xid);
        condvar.notify_all();
        return Ok((state, snapshot));
    }
    let lock_deadline = (timeout != Duration::ZERO).then(|| Instant::now() + timeout);
    loop {
        if ddl {
            snapshot = Snapshot::create(&state.transactions).use_command(snapshot.command_id);
        }
        state.load_catalog(Some(xid), snapshot, Some(temporary_schema_id));
        let discovered_locks;
        let locks = if let Some(locks) = prepared_locks {
            locks
        } else {
            discovered_locks =
                match collect_relation_locks(&state, statement, prepared_dependencies) {
                    Ok(locks) => locks,
                    Err(error) => {
                        state.relation_locks.cancel_transaction_waits(xid);
                        state.wait_for.clear_wait(xid);
                        condvar.notify_all();
                        return Err(error);
                    }
                };
            discovered_locks.as_slice()
        };
        let conflicts = match state.relation_locks.acquire_many(locks, xid) {
            RelationLockAttempt::Acquired => {
                state.wait_for.clear_wait(xid);
                condvar.notify_all();
                return Ok((state, snapshot));
            }
            RelationLockAttempt::Blocked(conflicts) => conflicts,
        };
        if state
            .wait_for
            .register_wait_dependencies(xid, &conflicts)
            .is_some()
        {
            condvar.notify_all();
        }
        if state.wait_for.take_victim(xid) {
            state.relation_locks.cancel_transaction_waits(xid);
            state.wait_for.clear_wait(xid);
            condvar.notify_all();
            return Err(create_deadlock_error());
        }
        let mut timed_out = false;
        let deadline = match (lock_deadline, statement_deadline) {
            (Some(lock), Some(statement)) => Some(lock.min(statement)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        state = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.relation_locks.cancel_transaction_waits(xid);
                state.wait_for.clear_wait(xid);
                condvar.notify_all();
                return Err(
                    if statement_deadline.is_some_and(|statement| statement <= deadline) {
                        create_statement_timeout_error()
                    } else {
                        create_lock_timeout_error()
                    },
                );
            }
            let (state, wait_result) = condvar
                .wait_timeout(state, remaining)
                .expect("database mutex is poisoned");
            timed_out = wait_result.timed_out();
            state
        } else {
            condvar.wait(state).expect("database mutex is poisoned")
        };
        state.wait_for.clear_wait(xid);
        if state.wait_for.take_victim(xid) {
            state.relation_locks.cancel_transaction_waits(xid);
            condvar.notify_all();
            return Err(create_deadlock_error());
        }
        if timed_out {
            state.relation_locks.cancel_transaction_waits(xid);
            condvar.notify_all();
            return Err(
                if statement_deadline.is_some_and(|statement| {
                    statement <= deadline.expect("timed wait has deadline")
                }) {
                    create_statement_timeout_error()
                } else {
                    create_lock_timeout_error()
                },
            );
        }
        if !ddl
            && isolation == IsolationLevel::ReadCommitted
            && conflicts.iter().any(|holder| {
                !matches!(
                    state.transactions.get_status(*holder),
                    Some(TransactionStatus::InFlight)
                )
            })
        {
            snapshot = Snapshot::create(&state.transactions).use_command(snapshot.command_id);
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn acquire_row_locks<'a>(
    condvar: &Condvar,
    timeout: Duration,
    statement_deadline: Option<Instant>,
    mut state: MutexGuard<'a, DatabaseState>,
    target: RowLockTarget<'_>,
    xid: Xid,
    temporary_schema_id: SchemaId,
    isolation: IsolationLevel,
    mut snapshot: Snapshot,
    context: &executor::StatementContext,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
) -> Result<(
    MutexGuard<'a, DatabaseState>,
    Snapshot,
    Vec<executor::RequiredRowLock>,
)> {
    let lock_deadline = (timeout != Duration::ZERO).then(|| Instant::now() + timeout);
    let mut acquired = Vec::<executor::RequiredRowLock>::new();
    let mut acquired_indexes = BTreeMap::<_, usize>::new();
    loop {
        state.load_catalog(Some(xid), snapshot, Some(temporary_schema_id));
        let required = match target {
            RowLockTarget::Ctes(statement) => {
                executor::collect_required_cte_row_locks(&state, statement, xid, &snapshot, context)
            }
            RowLockTarget::Statement(statement) => {
                executor::collect_required_row_locks(&state, statement, xid, &snapshot, context)
            }
        };
        let required = match required {
            Ok(required) => required,
            Err(error)
                if error.sqlstate == SqlState::InternalError
                    && error.message == executor::LOCK_PENDING =>
            {
                context.take_row_lock_recheck_locks()
            }
            Err(error) => return Err(error),
        };
        let pending = *context
            .advisory
            .pending
            .lock()
            .expect("pending advisory mutex is poisoned");
        if let crate::advisory::PendingAdvisory::Waiting(request) = pending {
            state = advisory::acquire_advisory_lock(
                condvar,
                timeout,
                statement_deadline,
                state,
                request,
                context,
            )?;
            continue;
        }
        let mut blocked = None;
        for required_lock in &required {
            match state
                .row_locks
                .acquire(required_lock.key, xid, required_lock.mode)
            {
                RowLockAttempt::Acquired => {
                    match acquired_indexes.entry((required_lock.key, required_lock.mode)) {
                        std::collections::btree_map::Entry::Occupied(entry) => {
                            let acquired = &mut acquired[*entry.get()];
                            if acquired.mutation_candidate.is_none() {
                                acquired.mutation_candidate =
                                    required_lock.mutation_candidate.clone();
                            }
                        }
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(acquired.len());
                            acquired.push(required_lock.clone());
                        }
                    }
                    condvar.notify_all();
                }
                RowLockAttempt::Blocked(conflicts) => {
                    if state
                        .wait_for
                        .register_wait_dependencies(xid, &conflicts)
                        .is_some()
                    {
                        condvar.notify_all();
                    }
                    blocked = Some((required_lock.key, conflicts));
                    break;
                }
            }
        }
        let Some((key, conflicts)) = blocked else {
            if context.take_row_lock_recheck() {
                continue;
            }
            if let Some(pending) = context.take_pending_cte_mutation() {
                let result = loop {
                    let result = executor::execute_statement(
                        &mut state,
                        &pending.statement,
                        xid,
                        &snapshot,
                        deferred_constraints,
                        defer_all,
                        context,
                        None,
                    );
                    if result.as_ref().is_err_and(|error| {
                        error.sqlstate == SqlState::InternalError
                            && error.message == executor::LOCK_PENDING
                    }) {
                        let advisory = *context
                            .advisory
                            .pending
                            .lock()
                            .expect("pending advisory mutex is poisoned");
                        if let crate::advisory::PendingAdvisory::Waiting(request) = advisory {
                            state = acquire_advisory_lock(
                                condvar,
                                timeout,
                                statement_deadline,
                                state,
                                request,
                                context,
                            )?;
                            state.load_catalog(Some(xid), snapshot, Some(temporary_schema_id));
                            continue;
                        }
                    }
                    break result?;
                };
                let StatementResult::Query(result) = result else {
                    return Err(PgError::create(
                        SqlState::FeatureNotSupported,
                        "WITH query does not have a RETURNING clause",
                    ));
                };
                context.set_executed_cte_result(pending.occurrence, pending.name, result);
                continue;
            }
            state.wait_for.clear_wait(xid);
            return Ok((state, snapshot, acquired));
        };
        if state.wait_for.take_victim(xid) {
            state.row_locks.cancel_wait(key, xid);
            state.wait_for.clear_wait(xid);
            return Err(create_deadlock_error());
        }
        let mut timed_out = false;
        let deadline = match (lock_deadline, statement_deadline) {
            (Some(lock), Some(statement)) => Some(lock.min(statement)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        state = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.row_locks.cancel_wait(key, xid);
                state.wait_for.clear_wait(xid);
                return Err(
                    if statement_deadline.is_some_and(|statement| statement <= deadline) {
                        create_statement_timeout_error()
                    } else {
                        create_lock_timeout_error()
                    },
                );
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
            return Err(create_deadlock_error());
        }
        if timed_out {
            return Err(
                if statement_deadline.is_some_and(|statement| {
                    statement <= deadline.expect("timed wait has deadline")
                }) {
                    create_statement_timeout_error()
                } else {
                    create_lock_timeout_error()
                },
            );
        }
        if isolation == IsolationLevel::RepeatableRead
            && state.tables.get(&key.table_id).is_some_and(|table| {
                table.iterate_version_chains().find(|(row_id, _)| *row_id == key.row_id)
                    .and_then(|(_, chain)| crate::txn::find_visible_version(chain, &snapshot, xid, &state.transactions))
                    .is_some_and(|version| version.xmax.is_some_and(|writer| matches!(state.transactions.get_status(writer), Some(TransactionStatus::Committed(commit_seq)) if commit_seq > snapshot.commit_seq)))
            })
        {
            return Err(PgError::create(SqlState::SerializationFailure, "could not serialize access due to concurrent update"));
        }
        if isolation == IsolationLevel::ReadCommitted
            && conflicts.iter().any(|holder| {
                !matches!(
                    state.transactions.get_status(*holder),
                    Some(TransactionStatus::InFlight)
                )
            })
        {
            snapshot = Snapshot::create(&state.transactions).use_command(snapshot.command_id);
            *context
                .prepared_subquery_results
                .lock()
                .expect("prepared subqueries mutex is poisoned") = Default::default();
        }
    }
}

pub(super) fn check_statement_timeout(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(create_statement_timeout_error())
    } else {
        Ok(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_lock_timeout_error() -> PgError {
    PgError::create(
        SqlState::LockNotAvailable,
        "canceling statement due to lock timeout",
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_statement_timeout_error() -> PgError {
    PgError::create(
        SqlState::QueryCanceled,
        "canceling statement due to statement timeout",
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_deadlock_error() -> PgError {
    PgError::create(SqlState::DeadlockDetected, "deadlock detected")
}
