use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use sqlparser::ast;

use crate::{
    catalog::{ConstraintId, SchemaId},
    error::{PgError, Result, SqlState},
    executor, parser,
    txn::Snapshot,
    value::Value,
};

use super::{ColumnMeta, Db, QueryResult, StatementResult};

mod catalog_dependencies;
mod constraint_timing;
mod do_block;
mod locking;
mod prepared;
mod savepoints;
mod settings;
mod transactions;

pub use prepared::PreparedStatement;
pub use transactions::{IsolationLevel, Transaction};

use catalog_dependencies::validate_catalog_dependencies;
use do_block::DoBlockContext;
use locking::{RowLockTarget, acquire_relation_locks, acquire_row_locks, check_statement_timeout};
use settings::SessionSettings;
use transactions::SessionTransactionState;

pub struct Session {
    db: Db,
    temporary_schema_id: SchemaId,
    transaction: Option<SessionTransactionState>,
    savepoints: Vec<savepoints::Savepoint>,
    settings: SessionSettings,
    default_lock_timeout: Duration,
    settings_undo: Option<SessionSettings>,
    settings_on_commit: Option<SessionSettings>,
    deferred_constraints: BTreeSet<ConstraintId>,
    defer_all_constraints: bool,
    deferred_foreign_keys_dirty: bool,
    sequence_session: executor::SequenceSessionStorage,
}

impl Session {
    pub(super) fn create(db: Db, lock_timeout: Duration) -> Self {
        let temporary_schema_id = db
            .state
            .lock()
            .expect("database mutex is poisoned")
            .catalog_history
            .create_temporary_schema_id();
        Session {
            db,
            temporary_schema_id,
            transaction: None,
            savepoints: Vec::new(),
            settings: SessionSettings::create(lock_timeout),
            default_lock_timeout: lock_timeout,
            settings_undo: None,
            settings_on_commit: None,
            deferred_constraints: BTreeSet::new(),
            defer_all_constraints: false,
            deferred_foreign_keys_dirty: false,
            sequence_session: Arc::new(Mutex::new(executor::SequenceSessionState::default())),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute(&mut self, sql: &str) -> Result<Vec<StatementResult>> {
        if let Some(result) = self.try_execute_set_constraints(sql) {
            return result.map(|result| vec![result]);
        }
        let statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => return self.abort_with_error(error),
        };
        let mut results = Vec::with_capacity(statements.len());
        for statement in statements {
            if self.transaction.is_none() {
                self.start_transaction(self.settings.default_isolation, true);
            }
            match self.execute_statement(&statement, None, None, None) {
                Ok(result) => results.push(result),
                Err(error) => {
                    if self.is_transaction_implicit_batch() {
                        let _ = self.rollback_transaction();
                    }
                    return Err(error);
                }
            }
        }
        if self.is_transaction_implicit_batch() {
            self.commit_transaction()?;
        }
        Ok(results)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn execute_statement(
        &mut self,
        statement: &ast::Statement,
        prepared_query: Option<(&executor::PreparedQueryPlan, &[Value], &[ColumnMeta])>,
        prepared_statement: Option<&PreparedStatement>,
        procedural: Option<DoBlockContext>,
    ) -> Result<StatementResult> {
        if matches!(
            self.transaction,
            Some(SessionTransactionState::Aborted { .. })
        ) && !matches!(
            statement,
            ast::Statement::Commit { .. } | ast::Statement::Rollback { .. }
        ) {
            return Err(PgError::create(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        let prepared_dependencies =
            prepared_statement.map(|statement| statement.catalog_dependencies.as_slice());
        if matches!(statement, ast::Statement::Analyze(_)) && !self.db.strict {
            return Ok(StatementResult::Affected(0));
        }
        match self.try_execute_setting(statement) {
            Ok(Some(result)) => return Ok(result),
            Ok(None) => {}
            Err(error) => return self.abort_with_error(error),
        }
        if let Some(result) = self.try_execute_transaction_command(statement)? {
            return Ok(result);
        }

        if matches!(
            self.transaction,
            Some(SessionTransactionState::Aborted { .. })
        ) {
            return Err(PgError::create(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        if let ast::Statement::Do(statement) = statement {
            let procedural = procedural.unwrap_or_else(|| DoBlockContext {
                deadline: (self.settings.statement_timeout != Duration::ZERO)
                    .then(|| Instant::now() + self.settings.statement_timeout),
                statement_timestamp: self.db.read_clock(),
            });
            return match self.execute_do_block(statement, procedural) {
                Ok(result) => match check_statement_timeout(procedural.deadline) {
                    Ok(()) => Ok(result),
                    Err(error) => self.abort_with_error(error),
                },
                Err(error) => self.abort_with_error(error),
            };
        }
        let Some(SessionTransactionState::Active(mut transaction)) = self.transaction else {
            unreachable!("transaction must be active while executing a statement")
        };
        let statement_deadline = procedural.map_or_else(
            || {
                (self.settings.statement_timeout != Duration::ZERO)
                    .then(|| Instant::now() + self.settings.statement_timeout)
            },
            |procedural| procedural.deadline,
        );
        let statement_timestamp = prepared_query.is_none().then(|| {
            procedural.map_or_else(
                || self.db.read_clock(),
                |procedural| procedural.statement_timestamp,
            )
        });
        let was_read_only = transaction.read_only;
        transaction.read_only &=
            prepared_query.is_some() || is_plain_read_only_statement(statement);
        let state_lock = self.db.state.clone();
        let condvar = self.db.condvar.clone();
        let mut state = state_lock.lock().expect("database mutex is poisoned");
        let command_id = crate::txn::CommandId(transaction.next_command_id);
        transaction.next_command_id += 1;
        let mut snapshot = match transaction.isolation {
            IsolationLevel::ReadCommitted => Snapshot::create(&state.transactions),
            IsolationLevel::RepeatableRead => *transaction
                .snapshot
                .get_or_insert_with(|| Snapshot::create(&state.transactions)),
        }
        .use_command(command_id);
        if transaction.isolation == IsolationLevel::RepeatableRead {
            state
                .transactions
                .retain_snapshot(transaction.xid, snapshot);
        }
        state.load_catalog(
            Some(transaction.xid),
            snapshot,
            Some(self.temporary_schema_id),
        );
        state.catalog.set_search_path(&self.settings.search_path);
        let acquired = match acquire_relation_locks(
            &condvar,
            self.settings.lock_timeout,
            statement_deadline,
            state,
            statement,
            prepared_dependencies,
            prepared_statement.and_then(|statement| statement.relation_locks.as_deref()),
            transaction.xid,
            self.temporary_schema_id,
            transaction.isolation,
            snapshot,
        ) {
            Ok(acquired) => acquired,
            Err(error) => return self.abort_with_error(error),
        };
        state = acquired.0;
        snapshot = acquired.1;
        if let Some(prepared) = prepared_statement
            && !state.catalog.matches_identity(&prepared.catalog_identity)
            && let Err(error) =
                validate_catalog_dependencies(&state.catalog, &prepared.catalog_dependencies)
        {
            drop(state);
            return self.abort_with_error(error);
        }
        transaction.statement_started = true;
        self.transaction = Some(SessionTransactionState::Active(transaction));
        if let Some((plan, parameters, columns)) = prepared_query {
            return match executor::execute_prepared_query(
                &state,
                plan,
                parameters,
                transaction.xid,
                &snapshot,
                statement_deadline,
            ) {
                Ok(rows) => match check_statement_timeout(statement_deadline) {
                    Ok(()) => Ok(StatementResult::Query(QueryResult {
                        columns: columns.to_vec(),
                        rows,
                    })),
                    Err(error) => {
                        drop(state);
                        self.abort_with_error(error)
                    }
                },
                Err(error) => {
                    drop(state);
                    self.abort_with_error(error)
                }
            };
        }
        let one_shot_plan = match executor::build_prepared_query_plan(&state, statement, &[], None)
        {
            Ok(plan) => plan,
            Err(error) => {
                drop(state);
                return self.abort_with_error(error);
            }
        };
        if let Some(plan) = one_shot_plan {
            return match executor::execute_prepared_query(
                &state,
                &plan,
                &[],
                transaction.xid,
                &snapshot,
                statement_deadline,
            ) {
                Ok(rows) => match check_statement_timeout(statement_deadline) {
                    Ok(()) => Ok(StatementResult::Query(QueryResult {
                        columns: plan.columns().to_vec(),
                        rows,
                    })),
                    Err(error) => {
                        drop(state);
                        self.abort_with_error(error)
                    }
                },
                Err(error) => {
                    drop(state);
                    self.abort_with_error(error)
                }
            };
        }
        let statement_contains_dml = contains_dml(statement);
        let sequences = executor::SequenceExecutionContext::create(
            &state.catalog,
            state.sequence_values.clone(),
            self.sequence_session.clone(),
        );
        let context = executor::StatementContext {
            command_id,
            transaction_timestamp: transaction.transaction_timestamp,
            statement_timestamp: statement_timestamp.expect("fallback captures statement time"),
            clock_timestamp: self.db.read_clock(),
            timezone: self.settings.timezone.clone(),
            deadline: statement_deadline,
            rng: self.db.rng.clone(),
            sequences,
            advisory: crate::advisory::AdvisoryExecutionContext {
                enabled: crate::advisory::contains_advisory_function(statement),
                locks: state.advisory_locks.clone(),
                session: self.temporary_schema_id,
                xid: transaction.xid,
                condvar: condvar.clone(),
                pending: Default::default(),
            },
            source_state: contains_triggered_insert(&state, statement)
                .then(|| Arc::new(state.clone())),
            source_snapshot: snapshot,
            pending_insert_sources: Default::default(),
            prepared_inserts: Arc::new(Mutex::new(Default::default())),
            prepared_update_inputs: Default::default(),
            prepared_writes: Default::default(),
            prepared_updates: Arc::new(Mutex::new(Default::default())),
            prepared_mutation_targets: Arc::new(Mutex::new(Default::default())),
            prepared_cte_results: Arc::new(Mutex::new(Vec::new())),
            executed_ctes: Arc::new(Mutex::new(Vec::new())),
            pending_cte_mutations: Arc::new(Mutex::new(Vec::new())),
            prepared_subquery_results: Arc::new(Mutex::new(Default::default())),
            next_subquery_invocation: Default::default(),
            pending_expressions: Default::default(),
            pending_operations: Default::default(),
            pending_evaluations: Default::default(),
            evaluation_cursor: None,
            lateral_initplans: Arc::new(Mutex::new(executor::collect_lateral_initplans(
                &state.catalog,
                statement,
            ))),
            lateral_invocation: false,
            retain_row_origins: false,
            query_row_demand: None,
            query_invocation: Vec::new(),
            query_source_state: Default::default(),
            inherited_row_lock: None,
            source_row_locks: Vec::new(),
            cte_query_barriers: Default::default(),
            prepared_plain_rows: Default::default(),
            prepared_groups: Default::default(),
            prepared_group_outputs: Default::default(),
            prepared_limits: Default::default(),
            prepared_values: Default::default(),
            capture_lock_queries: false,
            select_row_locks: Default::default(),
            prepared_select_rows: Default::default(),
            prepared_lock_queries: Default::default(),
            prepares_subquery_results: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            row_lock_recheck: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            row_lock_recheck_locks: Arc::new(Mutex::new(Vec::new())),
        };
        let (contains_cte, contains_subquery) = executor::detect_statement_features(statement);
        let mut acquired_row_locks = false;
        let cte_statement = if contains_cte {
            let (acquired_state, acquired_snapshot, locked_rows) = match acquire_row_locks(
                &condvar,
                self.settings.lock_timeout,
                statement_deadline,
                state,
                RowLockTarget::Ctes(statement),
                transaction.xid,
                self.temporary_schema_id,
                transaction.isolation,
                snapshot,
                &context,
                &self.deferred_constraints,
                self.defer_all_constraints,
            ) {
                Ok(acquired) => acquired,
                Err(error) => return self.abort_with_error(error),
            };
            acquired_row_locks = !locked_rows.is_empty();
            state = acquired_state;
            snapshot = acquired_snapshot;
            Some(
                match executor::materialize_statement_ctes(
                    &mut state,
                    statement,
                    transaction.xid,
                    &snapshot,
                    &self.deferred_constraints,
                    self.defer_all_constraints,
                    &context,
                ) {
                    Ok(statement) => statement,
                    Err(error) => return self.abort_with_error(error),
                },
            )
        } else {
            None
        };
        let statement = cte_statement.as_ref().unwrap_or(statement);
        let subquery_statement = if contains_subquery {
            Some(
                match executor::materialize_uncorrelated_subqueries(
                    &state,
                    statement,
                    transaction.xid,
                    &snapshot,
                    &context,
                ) {
                    Ok(statement) => statement,
                    Err(error) => return self.abort_with_error(error),
                },
            )
        } else {
            None
        };
        let statement = subquery_statement.as_ref().unwrap_or(statement);
        let catalog_before = matches!(parser::classify(statement), parser::StatementKind::Ddl)
            .then(|| state.catalog.clone());
        let (mut state, snapshot, locked_rows) = match acquire_row_locks(
            &condvar,
            self.settings.lock_timeout,
            statement_deadline,
            state,
            RowLockTarget::Statement(statement),
            transaction.xid,
            self.temporary_schema_id,
            transaction.isolation,
            snapshot,
            &context,
            &self.deferred_constraints,
            self.defer_all_constraints,
        ) {
            Ok(acquired) => acquired,
            Err(error) => return self.abort_with_error(error),
        };
        acquired_row_locks |= !locked_rows.is_empty();
        let mutation_targets =
            executor::mutation_locks_cover_targets(statement).then_some(locked_rows);
        let result = loop {
            let result = executor::execute_statement(
                &mut state,
                statement,
                transaction.xid,
                &snapshot,
                &self.deferred_constraints,
                self.defer_all_constraints,
                &context,
                mutation_targets.clone(),
            )
            .and_then(|result| {
                context.check_timeout()?;
                Ok(result)
            });
            if result.as_ref().is_err_and(|error| {
                error.sqlstate == crate::error::SqlState::InternalError
                    && error.message == executor::LOCK_PENDING
            }) {
                let pending = *context
                    .advisory
                    .pending
                    .lock()
                    .expect("pending advisory mutex is poisoned");
                if let crate::advisory::PendingAdvisory::Waiting(request) = pending {
                    state = match locking::acquire_advisory_lock(
                        &condvar,
                        self.settings.lock_timeout,
                        statement_deadline,
                        state,
                        request,
                        &context,
                    ) {
                        Ok(state) => state,
                        Err(error) => return self.abort_with_error(error),
                    };
                    state.load_catalog(
                        Some(transaction.xid),
                        snapshot,
                        Some(self.temporary_schema_id),
                    );
                    continue;
                }
            }
            break result;
        };
        match result {
            Ok(result) => {
                if let Some(catalog_before) = catalog_before {
                    state.record_catalog_changes(
                        &catalog_before,
                        transaction.xid,
                        context.command_id,
                    );
                }
                let has_writes = state.has_touched_tables(transaction.xid);
                if statement_contains_dml
                    && has_writes
                    && executor::contains_deferred_foreign_keys(
                        &state,
                        &self.deferred_constraints,
                        self.defer_all_constraints,
                    )
                {
                    self.deferred_foreign_keys_dirty = true;
                }
                if statement_contains_dml && was_read_only && !has_writes && !acquired_row_locks {
                    let Some(SessionTransactionState::Active(transaction)) = &mut self.transaction
                    else {
                        unreachable!("statement transaction remains active")
                    };
                    transaction.read_only = true;
                }
                Ok(result)
            }
            Err(error) => {
                drop(state);
                self.abort_with_error(error)
            }
        }
    }
}

fn contains_dml(statement: &ast::Statement) -> bool {
    match statement {
        ast::Statement::Insert(_)
        | ast::Statement::Update(_)
        | ast::Statement::Delete(_)
        | ast::Statement::Truncate(_) => true,
        ast::Statement::Query(query) => {
            matches!(
                query.body.as_ref(),
                ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
            ) || query.with.as_ref().is_some_and(|with| {
                with.cte_tables
                    .iter()
                    .any(|cte| contains_dml(&ast::Statement::Query(cte.query.clone())))
            })
        }
        _ => false,
    }
}

fn contains_triggered_insert(state: &executor::DatabaseState, statement: &ast::Statement) -> bool {
    match statement {
        ast::Statement::Insert(insert) => executor::resolve_insert_table_name(&insert.table)
            .ok()
            .and_then(|name| state.catalog.require_named_table(&name).ok())
            .is_some_and(|table| {
                table.triggers.iter().any(|trigger| {
                    trigger
                        .definition
                        .events
                        .iter()
                        .any(|event| matches!(event, ast::TriggerEvent::Insert))
                })
            }),
        ast::Statement::Query(query) => {
            matches!(
                query.body.as_ref(),
                ast::SetExpr::Insert(statement) if contains_triggered_insert(state, statement)
            ) || query.with.as_ref().is_some_and(|with| {
                with.cte_tables.iter().any(|cte| {
                    contains_triggered_insert(
                        state,
                        &ast::Statement::Query(Box::new((*cte.query).clone())),
                    )
                })
            })
        }
        _ => false,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_plain_read_only_statement(statement: &ast::Statement) -> bool {
    let ast::Statement::Query(query) = statement else {
        return false;
    };
    query.with.is_none()
        && query.locks.is_empty()
        && query.for_clause.is_none()
        && !matches!(
            query.body.as_ref(),
            ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
        )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn contains_sequence_function(statement: &ast::Statement) -> bool {
    let mut found = false;
    let _ = ast::visit_expressions(statement, |expression| {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        if executor::normalize_function_name(&function.name).is_ok_and(|name| {
            matches!(
                name.as_str(),
                "nextval" | "currval" | "lastval" | "setval" | "pg_get_serial_sequence"
            )
        }) {
            found = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}

#[cfg(test)]
mod advisory_tests;
#[cfg(test)]
mod tests;
