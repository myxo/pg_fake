use std::{
    collections::BTreeSet,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use rand_chacha::{ChaCha12Rng, rand_core::SeedableRng};
use sqlparser::ast::{self, Visit as _};

use crate::{
    analyzer,
    catalog::{
        ConstraintId, RelationName, ResolvedRelationName, SchemaId, SequenceSchema, TEMP_SCHEMA,
        TableId, TablePersistence, TableSchema, ViewDependency, ViewSchema,
    },
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{self, DatabaseState},
    parser,
    txn::{
        RelationLockAttempt, RelationLockMode, RowLockAttempt, Snapshot, TransactionStatus, Xid,
    },
    value::{Oid, Value},
};

#[cfg(test)]
use crate::storage::Table;

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
pub struct PreparedStatement {
    statement: ast::Statement,
    parameter_types: Vec<crate::value::BaseType>,
    columns: Vec<ColumnMeta>,
    query_plan: Option<executor::PreparedQueryPlan>,
    catalog_dependencies: Vec<PreparedCatalogDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreparedCatalogDependency {
    Table {
        name: RelationName,
        schema: TableSchema,
    },
    Sequence {
        name: RelationName,
        schema: SequenceSchema,
    },
    Constraint {
        table: TableId,
        id: ConstraintId,
    },
    View {
        name: RelationName,
        schema: ViewSchema,
    },
}

fn extract_prepared_sequence_name(expression: &ast::Expr) -> Option<&str> {
    match expression {
        ast::Expr::Cast { expr, .. } | ast::Expr::Nested(expr) => {
            extract_prepared_sequence_name(expr)
        }
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

fn extract_runtime_sequence_name(expression: &ast::Expr) -> Option<&str> {
    match expression {
        ast::Expr::Cast { expr, .. } | ast::Expr::Nested(expr) => {
            extract_runtime_sequence_name(expr)
        }
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

struct PreparedDependencyCollector<'catalog> {
    catalog: &'catalog crate::catalog::Catalog,
    dependencies: Vec<PreparedCatalogDependency>,
    cte_scopes: Vec<PreparedCteScope>,
    error: Option<PgError>,
}

#[derive(Clone)]
struct PreparedCteScope {
    body_mask: Vec<String>,
    cte_queries: Vec<Box<ast::Query>>,
    cte_masks: Vec<Vec<String>>,
    next_cte: usize,
}

fn enter_prepared_cte_scope(stack: &mut Vec<PreparedCteScope>, query: &ast::Query) {
    let inherited = stack.last_mut().map_or_else(Vec::new, |parent| {
        if parent
            .cte_queries
            .get(parent.next_cte)
            .is_some_and(|candidate| candidate.as_ref() == query)
        {
            let mask = parent.cte_masks[parent.next_cte].clone();
            parent.next_cte += 1;
            mask
        } else {
            parent.body_mask.clone()
        }
    });
    let cte_queries = query
        .with
        .as_ref()
        .map(|with| {
            with.cte_tables
                .iter()
                .map(|cte| cte.query.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let names = query
        .with
        .as_ref()
        .map(|with| {
            with.cte_tables
                .iter()
                .map(|cte| executor::normalize_identifier(&cte.alias.name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let recursive = query.with.as_ref().is_some_and(|with| with.recursive);
    let cte_masks = names
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let mut mask = inherited.clone();
            mask.extend(if recursive {
                names.iter().cloned()
            } else {
                names[..index].iter().cloned()
            });
            mask
        })
        .collect();
    let mut body_mask = inherited;
    body_mask.extend(names);
    stack.push(PreparedCteScope {
        body_mask,
        cte_queries,
        cte_masks,
        next_cte: 0,
    });
}

impl PreparedDependencyCollector<'_> {
    fn add_dependency(&mut self, dependency: PreparedCatalogDependency) {
        if !self.dependencies.contains(&dependency) {
            self.dependencies.push(dependency);
        }
    }

    fn collect_relation(&mut self, relation: &ast::ObjectName) -> Result<()> {
        let name = executor::normalize_relation_name(relation)?;
        let table = match self.catalog.require_named_table(&name) {
            Ok(table) => table.clone(),
            Err(error) if error.sqlstate == SqlState::WrongObjectType => {
                let view = self.catalog.require_named_view(&name)?.clone();
                self.add_dependency(PreparedCatalogDependency::View {
                    name,
                    schema: view.clone(),
                });
                let _ = view.query.visit(self);
                if let Some(error) = self.error.take() {
                    return Err(error);
                }
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.add_dependency(PreparedCatalogDependency::Table {
            name,
            schema: table.clone(),
        });
        for dependency in
            table
                .constraints
                .iter()
                .map(|constraint| PreparedCatalogDependency::Constraint {
                    table: table.id,
                    id: constraint.get_id(),
                })
        {
            self.add_dependency(dependency);
        }
        for sequence_name in table
            .columns
            .iter()
            .filter_map(|column| column.default_sequence.as_ref())
        {
            let name = RelationName::create(
                Some(
                    self.catalog
                        .get_schema_name(sequence_name.schema_id)
                        .to_owned(),
                ),
                sequence_name.name.clone(),
            );
            let sequence = self.catalog.require_named_sequence(&name)?.clone();
            self.add_dependency(PreparedCatalogDependency::Sequence {
                name,
                schema: sequence,
            });
        }
        Ok(())
    }
}

impl ast::Visitor for PreparedDependencyCollector<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        enter_prepared_cte_scope(&mut self.cte_scopes, query);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte_scopes
            .pop()
            .expect("visited query pushed a CTE scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_relation(
        &mut self,
        relation: &ast::ObjectName,
    ) -> std::ops::ControlFlow<Self::Break> {
        if executor::normalize_relation_name(relation).is_ok_and(|name| {
            name.schema.is_none()
                && self
                    .cte_scopes
                    .last()
                    .is_some_and(|scope| scope.body_mask.contains(&name.name))
        }) {
            return std::ops::ControlFlow::Continue(());
        }
        if let Err(error) = self.collect_relation(relation) {
            self.error = Some(error);
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = executor::normalize_unqualified_object_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(name.as_str(), "nextval" | "currval" | "setval") {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::FunctionArguments::List(arguments) = &function.args else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))) =
            arguments.args.first()
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(name) = extract_prepared_sequence_name(argument) else {
            return std::ops::ControlFlow::Continue(());
        };
        let name = match executor::normalize_sequence_name(name) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        match self.catalog.require_named_sequence(&name) {
            Ok(sequence) => {
                self.add_dependency(PreparedCatalogDependency::Sequence {
                    name,
                    schema: sequence.clone(),
                });
                std::ops::ControlFlow::Continue(())
            }
            Err(error) => {
                self.error = Some(error);
                std::ops::ControlFlow::Break(())
            }
        }
    }
}

fn collect_prepared_catalog_dependencies(
    catalog: &crate::catalog::Catalog,
    statements: impl IntoIterator<Item = ast::Statement>,
) -> Result<Vec<PreparedCatalogDependency>> {
    let mut collector = PreparedDependencyCollector {
        catalog,
        dependencies: Vec::new(),
        cte_scopes: Vec::new(),
        error: None,
    };
    for statement in statements {
        match &statement {
            ast::Statement::Query(_)
            | ast::Statement::Insert(_)
            | ast::Statement::Update(_)
            | ast::Statement::Delete(_) => {
                let _ = statement.visit(&mut collector);
            }
            ast::Statement::Drop {
                object_type: ast::ObjectType::Table,
                names,
                ..
            } => {
                for name in names {
                    let Ok(name) = executor::normalize_relation_name(name) else {
                        continue;
                    };
                    if let Ok(table) = catalog.require_named_table(&name) {
                        collector.add_dependency(PreparedCatalogDependency::Table {
                            name,
                            schema: table.clone(),
                        });
                    }
                }
            }
            ast::Statement::Drop {
                object_type: ast::ObjectType::Sequence,
                names,
                ..
            } => {
                for name in names {
                    let Ok(name) = executor::normalize_relation_name(name) else {
                        continue;
                    };
                    if let Ok(sequence) = catalog.require_named_sequence(&name) {
                        collector.add_dependency(PreparedCatalogDependency::Sequence {
                            name,
                            schema: sequence.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
        if let Some(error) = collector.error.take() {
            return Err(error);
        }
        if let ast::Statement::Insert(insert) = &statement
            && let Some(ast::OnInsert::OnConflict(ast::OnConflict {
                conflict_target: Some(ast::ConflictTarget::OnConstraint(name)),
                ..
            })) = &insert.on
        {
            let table_name = executor::resolve_insert_table_name(&insert.table)?;
            let table = catalog.require_named_table(&table_name)?;
            let constraint_name = executor::normalize_unqualified_object_name(name)?;
            let constraint = table
                .constraints
                .iter()
                .find(|constraint| constraint.get_name() == Some(constraint_name.as_str()))
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedObject,
                        format!(
                            "constraint {constraint_name:?} for table {:?} does not exist",
                            table.name
                        ),
                    )
                })?;
            collector.add_dependency(PreparedCatalogDependency::Constraint {
                table: table.id,
                id: constraint.get_id(),
            });
        }
    }
    Ok(collector.dependencies)
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
    clock: Arc<Mutex<DatabaseClock>>,
    rng: Arc<Mutex<ChaCha12Rng>>,
    strict: bool,
}
pub struct DbBuilder {
    lock_timeout: Duration,
    mock_time: bool,
    seed: Option<u64>,
    strict: bool,
}
#[derive(Clone, Copy)]
enum DatabaseClock {
    Real,
    Mock(chrono::DateTime<chrono::Utc>),
}
pub struct Session {
    db: Db,
    temporary_schema_id: SchemaId,
    transaction: Option<SessionTransactionState>,
    default_isolation: IsolationLevel,
    lock_timeout: Duration,
    timezone: String,
    settings_undo: Option<(IsolationLevel, Duration, String)>,
    deferred_constraints: BTreeSet<ConstraintId>,
    defer_all_constraints: bool,
    deferred_foreign_keys_dirty: bool,
    sequence_session: executor::SequenceSessionStorage,
}
#[derive(Clone, Copy)]
enum SessionTransactionState {
    Active(ActiveTransaction),
    Aborted { xid: Xid, implicit_batch: bool },
}
#[derive(Clone, Copy)]
struct ActiveTransaction {
    xid: Xid,
    isolation: IsolationLevel,
    snapshot: Option<Snapshot>,
    statement_started: bool,
    read_only: bool,
    next_command_id: u64,
    implicit_batch: bool,
    transaction_timestamp: chrono::DateTime<chrono::Utc>,
}
pub struct Transaction<'session> {
    session: &'session mut Session,
    finished: bool,
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
    for table_id in state.reclaimable_table_ids() {
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_invalid_lock_timeout_error() -> PgError {
    PgError::create(
        SqlState::InvalidParameterValue,
        "invalid value for parameter lock_timeout",
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_lock_timeout(expression: &ast::Expr) -> Result<Duration> {
    let text = match expression {
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Number(value, _) => value.as_str(),
            ast::Value::SingleQuotedString(value) => value.trim(),
            _ => return Err(create_invalid_lock_timeout_error()),
        },
        _ => return Err(create_invalid_lock_timeout_error()),
    };
    let lower = text.to_ascii_lowercase();
    if let Some(milliseconds) = lower.strip_suffix("ms") {
        return milliseconds
            .trim()
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| create_invalid_lock_timeout_error());
    }
    if let Some(seconds) = lower.strip_suffix('s') {
        return seconds
            .trim()
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| create_invalid_lock_timeout_error());
    }
    lower
        .trim()
        .parse::<u64>()
        .map(Duration::from_millis)
        .map_err(|_| create_invalid_lock_timeout_error())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_timezone(expression: &ast::Expr) -> Result<String> {
    let value = match expression {
        ast::Expr::Value(value) => {
            let ast::Value::SingleQuotedString(value) = &value.value else {
                return Err(PgError::create(
                    SqlState::InvalidParameterValue,
                    "invalid value for parameter TimeZone",
                ));
            };
            value
        }
        ast::Expr::Identifier(ast::Ident { value, .. }) => value,
        _ => {
            return Err(PgError::create(
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
        Err(PgError::create(
            SqlState::InvalidParameterValue,
            "invalid value for parameter TimeZone",
        ))
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
fn create_deadlock_error() -> PgError {
    PgError::create(SqlState::DeadlockDetected, "deadlock detected")
}

#[derive(Clone, Copy)]
enum RowLockTarget<'a> {
    Ctes(&'a ast::Statement),
    Statement(&'a ast::Statement),
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
fn collect_ddl_relation_locks(
    catalog: &crate::catalog::Catalog,
    statement: &ast::Statement,
) -> Result<Vec<(String, RelationLockMode)>> {
    let mut locks = std::collections::BTreeMap::new();
    match statement {
        ast::Statement::CreateTable(create) => {
            let relation_name = executor::normalize_relation_name(&create.name)?;
            let temporary =
                create.temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
            let table_name = catalog.resolve_creation_name(&relation_name, temporary)?;
            locks.insert(table_name.get_lock_name(), RelationLockMode::Exclusive);
            let mut generated_sequences = Vec::new();
            for column in &create.columns {
                let column_name = executor::normalize_identifier(&column.name);
                let serial = matches!(
                    column.data_type.to_string().to_ascii_lowercase().as_str(),
                    "smallserial" | "serial2" | "serial" | "serial4" | "bigserial" | "serial8"
                );
                let identity = column
                    .options
                    .iter()
                    .any(|option| matches!(option.option, ast::ColumnOption::Generated { .. }));
                if serial || identity {
                    let base = format!("{}_{column_name}_seq", table_name.name);
                    let mut number = 0;
                    loop {
                        let candidate = if number == 0 {
                            base.clone()
                        } else {
                            format!("{base}{number}")
                        };
                        if !catalog.has_resolved_relation(&ResolvedRelationName {
                            schema_id: table_name.schema_id,
                            name: candidate.clone(),
                        }) && !generated_sequences.contains(&candidate)
                        {
                            locks.insert(
                                ResolvedRelationName {
                                    schema_id: table_name.schema_id,
                                    name: candidate.clone(),
                                }
                                .get_lock_name(),
                                RelationLockMode::Exclusive,
                            );
                            generated_sequences.push(candidate);
                            break;
                        }
                        number += 1;
                    }
                }
                for option in &column.options {
                    if let ast::ColumnOption::ForeignKey(foreign_key) = &option.option {
                        let name = catalog.resolve_relation_name(
                            &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                        )?;
                        locks
                            .entry(name.get_lock_name())
                            .or_insert(RelationLockMode::Shared);
                    }
                }
            }
            for constraint in &create.constraints {
                if let ast::TableConstraint::ForeignKey(foreign_key) = constraint {
                    let name = catalog.resolve_relation_name(
                        &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                    )?;
                    locks
                        .entry(name.get_lock_name())
                        .or_insert(RelationLockMode::Shared);
                }
            }
        }
        ast::Statement::CreateSequence {
            temporary,
            name,
            owned_by,
            ..
        } => {
            let relation_name = executor::normalize_relation_name(name)?;
            let temporary = *temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
            locks.insert(
                catalog
                    .resolve_creation_name(&relation_name, temporary)?
                    .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            if let Some(owned_by) = owned_by
                && matches!(owned_by.0.len(), 2 | 3)
            {
                let table = ast::ObjectName(owned_by.0[..owned_by.0.len() - 1].to_vec());
                locks
                    .entry(
                        catalog
                            .resolve_relation_name(&executor::normalize_relation_name(&table)?)?
                            .get_lock_name(),
                    )
                    .or_insert(RelationLockMode::Shared);
            }
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Table,
            names: objects,
            ..
        } => {
            for object in objects {
                let name = executor::normalize_relation_name(object)?;
                locks.insert(
                    catalog.resolve_relation_name(&name)?.get_lock_name(),
                    RelationLockMode::Exclusive,
                );
                if let Ok(table) = catalog.require_named_table(&name) {
                    for (referencing, _) in catalog.referencing_foreign_keys(table.id) {
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: referencing.schema_id,
                                    name: referencing.name,
                                }
                                .get_lock_name(),
                            )
                            .or_insert(RelationLockMode::Shared);
                    }
                    for sequence in catalog.iterate_sequences().filter(|sequence| {
                        sequence.owned_by.as_ref().map(|(owner, _)| *owner) == Some(table.id)
                    }) {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: sequence.schema_id,
                                name: sequence.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                    for sequence in table
                        .columns
                        .iter()
                        .filter_map(|column| column.default_sequence.as_ref())
                    {
                        locks
                            .entry(sequence.get_lock_name())
                            .or_insert(RelationLockMode::Shared);
                    }
                }
            }
        }
        ast::Statement::AlterTable(alter) => {
            let name = executor::normalize_relation_name(&alter.name)?;
            let table = match catalog.require_named_table(&name) {
                Ok(table) => table,
                Err(error) if alter.if_exists && error.sqlstate == SqlState::UndefinedTable => {
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            };
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            if alter.operations.iter().any(|operation| {
                matches!(
                    operation,
                    ast::AlterTableOperation::RenameColumn { .. }
                        | ast::AlterTableOperation::RenameTable { .. }
                        | ast::AlterTableOperation::DropColumn { .. }
                )
            }) {
                for view in catalog
                    .iterate_views()
                    .filter(|view| view.dependencies.contains(&ViewDependency::Table(table.id)))
                {
                    locks.insert(
                        ResolvedRelationName {
                            schema_id: view.schema_id,
                            name: view.name.clone(),
                        }
                        .get_lock_name(),
                        RelationLockMode::Exclusive,
                    );
                }
            }
            for operation in &alter.operations {
                if let ast::AlterTableOperation::AddColumn { column_def, .. } = operation {
                    for option in &column_def.options {
                        if let ast::ColumnOption::ForeignKey(foreign_key) = &option.option {
                            let parent = catalog.resolve_relation_name(
                                &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                            )?;
                            locks
                                .entry(parent.get_lock_name())
                                .or_insert(RelationLockMode::Shared);
                        }
                    }
                    let serial = matches!(
                        column_def
                            .data_type
                            .to_string()
                            .to_ascii_lowercase()
                            .as_str(),
                        "smallserial" | "serial2" | "serial" | "serial4" | "bigserial" | "serial8"
                    );
                    let identity = column_def
                        .options
                        .iter()
                        .any(|option| matches!(option.option, ast::ColumnOption::Generated { .. }));
                    if serial || identity {
                        let column_name = executor::normalize_identifier(&column_def.name);
                        let base = format!("{}_{column_name}_seq", table.name);
                        let mut number = 0;
                        loop {
                            let candidate = if number == 0 {
                                base.clone()
                            } else {
                                format!("{base}{number}")
                            };
                            let candidate = ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: candidate,
                            };
                            if !catalog.has_resolved_relation(&candidate) {
                                locks
                                    .insert(candidate.get_lock_name(), RelationLockMode::Exclusive);
                                break;
                            }
                            number += 1;
                        }
                    }
                }
                if let ast::AlterTableOperation::AddConstraint {
                    constraint: ast::TableConstraint::ForeignKey(foreign_key),
                    ..
                } = operation
                {
                    let parent = catalog.resolve_relation_name(
                        &executor::normalize_relation_name(&foreign_key.foreign_table)?,
                    )?;
                    locks
                        .entry(parent.get_lock_name())
                        .or_insert(RelationLockMode::Shared);
                }
                if let ast::AlterTableOperation::RenameTable { table_name } = operation {
                    let target = match table_name {
                        ast::RenameTableNameKind::To(name) | ast::RenameTableNameKind::As(name) => {
                            name
                        }
                    };
                    let target = executor::normalize_relation_name(target)?;
                    let temporary = matches!(table.persistence, TablePersistence::Temporary { .. });
                    let resolved = catalog.resolve_creation_name(&target, temporary)?;
                    locks.insert(resolved.get_lock_name(), RelationLockMode::Exclusive);
                }
                if matches!(
                    operation,
                    ast::AlterTableOperation::DropColumn {
                        drop_behavior: Some(ast::DropBehavior::Cascade),
                        ..
                    } | ast::AlterTableOperation::DropConstraint {
                        drop_behavior: Some(ast::DropBehavior::Cascade),
                        ..
                    }
                ) {
                    for (referencing, _) in catalog.referencing_foreign_keys(table.id) {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: referencing.schema_id,
                                name: referencing.name,
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                }
            }
        }
        ast::Statement::CreateIndex(create) => {
            let table_name = executor::normalize_relation_name(&create.table_name)?;
            let table = catalog.require_named_table(&table_name)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            let Some(name) = &create.name else {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "index name is required",
                ));
            };
            let name = executor::normalize_relation_name(name)?;
            let schema_id = match name.schema.as_deref() {
                Some(schema) => catalog.require_schema(schema)?.id,
                None => table.schema_id,
            };
            locks.insert(
                ResolvedRelationName {
                    schema_id,
                    name: name.name,
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::AlterIndex {
            if_exists,
            name,
            operation,
        } => {
            let name = executor::normalize_relation_name(name)?;
            match catalog.require_named_index(&name) {
                Ok((table, index)) => {
                    locks.insert(
                        ResolvedRelationName {
                            schema_id: table.schema_id,
                            name: table.name.clone(),
                        }
                        .get_lock_name(),
                        RelationLockMode::Exclusive,
                    );
                    locks.insert(
                        ResolvedRelationName {
                            schema_id: table.schema_id,
                            name: index.name.clone(),
                        }
                        .get_lock_name(),
                        RelationLockMode::Exclusive,
                    );
                    let ast::AlterIndexOperation::RenameIndex { index_name } = operation;
                    let target_name = executor::normalize_relation_name(index_name)?;
                    if target_name.schema.is_none() {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: target_name.name,
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                }
                Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedObject => {}
                Err(error) => return Err(error),
            }
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Index,
            names,
            if_exists,
            ..
        } => {
            for name in names {
                let name = executor::normalize_relation_name(name)?;
                match catalog.require_named_index(&name) {
                    Ok((table, index)) => {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: table.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: index.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedObject => {}
                    Err(error) => return Err(error),
                }
            }
        }
        ast::Statement::CreateView(create) => {
            let name = executor::normalize_relation_name(&create.name)?;
            let temporary = create.temporary || name.schema.as_deref() == Some(TEMP_SCHEMA);
            locks.insert(
                catalog
                    .resolve_creation_name(&name, temporary)?
                    .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            for dependency in collect_prepared_catalog_dependencies(
                catalog,
                [ast::Statement::Query(create.query.clone())],
            )? {
                match dependency {
                    PreparedCatalogDependency::Table { schema, .. } => {
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: schema.schema_id,
                                    name: schema.name,
                                }
                                .get_lock_name(),
                            )
                            .or_insert(RelationLockMode::Shared);
                    }
                    PreparedCatalogDependency::View { schema, .. } => {
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: schema.schema_id,
                                    name: schema.name,
                                }
                                .get_lock_name(),
                            )
                            .or_insert(RelationLockMode::Shared);
                    }
                    PreparedCatalogDependency::Sequence { .. }
                    | PreparedCatalogDependency::Constraint { .. } => {}
                }
            }
        }
        ast::Statement::CreateTrigger(_) => {}
        ast::Statement::AlterTrigger { table_name, .. } => {
            let table =
                catalog.require_named_table(&executor::normalize_relation_name(table_name)?)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::Comment {
            object_type: ast::CommentObject::View,
            object_name,
            ..
        } => {
            let view =
                catalog.require_named_view(&executor::normalize_relation_name(object_name)?)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: view.schema_id,
                    name: view.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::View,
            names,
            ..
        } => {
            for name in names {
                locks.insert(
                    catalog
                        .resolve_relation_name(&executor::normalize_relation_name(name)?)?
                        .get_lock_name(),
                    RelationLockMode::Exclusive,
                );
            }
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Sequence,
            names: objects,
            ..
        } => {
            for object in objects {
                locks.insert(
                    catalog
                        .resolve_relation_name(&executor::normalize_relation_name(object)?)?
                        .get_lock_name(),
                    RelationLockMode::Exclusive,
                );
            }
        }
        _ => {}
    }
    let mut sequence_error = None;
    let _ = ast::visit_expressions(statement, |expression| -> std::ops::ControlFlow<()> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(function_name) = executor::normalize_unqualified_object_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(function_name.as_str(), "nextval" | "currval" | "setval") {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::FunctionArguments::List(arguments) = &function.args else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))) =
            arguments.args.first()
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(name) = extract_runtime_sequence_name(argument) else {
            sequence_error = Some(PgError::create(
                SqlState::FeatureNotSupported,
                "computed sequence names are not implemented",
            ));
            return std::ops::ControlFlow::Break(());
        };
        match executor::normalize_sequence_name(name) {
            Ok(name) => match catalog.resolve_relation_name(&name) {
                Ok(name) => {
                    locks
                        .entry(name.get_lock_name())
                        .or_insert(RelationLockMode::Shared);
                    std::ops::ControlFlow::Continue(())
                }
                Err(error) => {
                    sequence_error = Some(error);
                    std::ops::ControlFlow::Break(())
                }
            },
            Err(error) => {
                sequence_error = Some(error);
                std::ops::ControlFlow::Break(())
            }
        }
    });
    if let Some(error) = sequence_error {
        return Err(error);
    }
    Ok(locks.into_iter().collect())
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ForeignKeyMutation {
    Delete(TableId),
    Update {
        table: TableId,
        columns: Vec<String>,
    },
}

fn collect_assignment_columns(assignments: &[ast::Assignment]) -> Result<Vec<String>> {
    let mut columns = BTreeSet::new();
    for assignment in assignments {
        let ast::AssignmentTarget::ColumnName(column) = &assignment.target else {
            return reject_unsupported("UPDATE tuple assignment is not implemented");
        };
        columns.insert(executor::normalize_unqualified_object_name(column)?);
    }
    Ok(columns.into_iter().collect())
}

fn collect_foreign_key_relation_locks(
    state: &DatabaseState,
    statements: impl IntoIterator<Item = ast::Statement>,
    locks: &mut std::collections::BTreeMap<String, RelationLockMode>,
) -> Result<()> {
    let mut pending = Vec::new();
    for statement in statements {
        match statement {
            ast::Statement::Insert(insert) => {
                let name = executor::resolve_insert_table_name(&insert.table)?;
                if state.catalog.require_named_view(&name).is_ok() {
                    continue;
                }
                let table = state.catalog.require_named_table(&name)?;
                for constraint in &table.constraints {
                    if let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint {
                        let parent = state
                            .catalog
                            .require_table_by_id(foreign_key.foreign_table_id)?;
                        locks
                            .entry(
                                ResolvedRelationName {
                                    schema_id: parent.schema_id,
                                    name: parent.name.clone(),
                                }
                                .get_lock_name(),
                            )
                            .or_insert(RelationLockMode::Shared);
                    }
                }
                if let Some(ast::OnInsert::OnConflict(ast::OnConflict {
                    action: ast::OnConflictAction::DoUpdate(update),
                    ..
                })) = insert.on
                {
                    pending.push(ForeignKeyMutation::Update {
                        table: table.id,
                        columns: collect_assignment_columns(&update.assignments)?,
                    });
                }
            }
            ast::Statement::Update(update) => {
                let ast::TableFactor::Table { name, .. } = update.table.relation else {
                    continue;
                };
                let name = executor::normalize_relation_name(&name)?;
                if state.catalog.require_named_view(&name).is_ok() {
                    continue;
                }
                let table = state.catalog.require_named_table(&name)?;
                pending.push(ForeignKeyMutation::Update {
                    table: table.id,
                    columns: collect_assignment_columns(&update.assignments)?,
                });
            }
            ast::Statement::Delete(delete) => {
                let ast::FromTable::WithFromKeyword(from) = delete.from else {
                    continue;
                };
                let Some(ast::TableWithJoins {
                    relation: ast::TableFactor::Table { name, .. },
                    ..
                }) = from.first()
                else {
                    continue;
                };
                let name = executor::normalize_relation_name(name)?;
                if state.catalog.require_named_view(&name).is_ok() {
                    continue;
                }
                let table = state.catalog.require_named_table(&name)?;
                pending.push(ForeignKeyMutation::Delete(table.id));
            }
            _ => {}
        }
    }
    let mut visited = BTreeSet::new();
    while let Some(mutation) = pending.pop() {
        if !visited.insert(mutation.clone()) {
            continue;
        }
        let (table_id, updated_columns) = match &mutation {
            ForeignKeyMutation::Delete(table) => (*table, None),
            ForeignKeyMutation::Update { table, columns } => (*table, Some(columns.as_slice())),
        };
        let table = state.catalog.require_table_by_id(table_id)?;
        if let Some(updated_columns) = updated_columns {
            for constraint in &table.constraints {
                let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
                    continue;
                };
                if foreign_key
                    .columns
                    .iter()
                    .any(|column| updated_columns.contains(column))
                {
                    let parent = state
                        .catalog
                        .require_table_by_id(foreign_key.foreign_table_id)?;
                    locks
                        .entry(
                            ResolvedRelationName {
                                schema_id: parent.schema_id,
                                name: parent.name.clone(),
                            }
                            .get_lock_name(),
                        )
                        .or_insert(RelationLockMode::Shared);
                }
            }
        }
        for (child, foreign_key) in state.catalog.referencing_foreign_keys(table_id) {
            if let Some(updated_columns) = updated_columns {
                let referred_columns = if foreign_key.referred_columns.is_empty() {
                    table
                        .constraints
                        .iter()
                        .find_map(|constraint| match constraint {
                            crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns),
                            _ => None,
                        })
                        .expect("foreign key definition was validated")
                } else {
                    &foreign_key.referred_columns
                };
                if !referred_columns
                    .iter()
                    .any(|column| updated_columns.contains(column))
                {
                    continue;
                }
            }
            locks
                .entry(
                    ResolvedRelationName {
                        schema_id: child.schema_id,
                        name: child.name.clone(),
                    }
                    .get_lock_name(),
                )
                .or_insert(RelationLockMode::Shared);
            let action = if updated_columns.is_some() {
                foreign_key.on_update
            } else {
                foreign_key.on_delete
            };
            match action {
                crate::catalog::ForeignKeyAction::Cascade if updated_columns.is_none() => {
                    pending.push(ForeignKeyMutation::Delete(child.id));
                }
                crate::catalog::ForeignKeyAction::Cascade
                | crate::catalog::ForeignKeyAction::SetNull
                | crate::catalog::ForeignKeyAction::SetDefault => {
                    pending.push(ForeignKeyMutation::Update {
                        table: child.id,
                        columns: foreign_key.columns,
                    });
                }
                crate::catalog::ForeignKeyAction::NoAction
                | crate::catalog::ForeignKeyAction::Restrict => {}
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn collect_relation_locks(
    state: &DatabaseState,
    statement: &ast::Statement,
    prepared_dependencies: Option<&[PreparedCatalogDependency]>,
) -> Result<Vec<(String, RelationLockMode)>> {
    if matches!(parser::classify(statement), parser::StatementKind::Ddl) {
        return collect_ddl_relation_locks(&state.catalog, statement);
    }
    let (expanded_statement, mutations) = executor::expand_ctes_for_analysis(statement, state)?;
    let dependencies = match prepared_dependencies {
        Some(dependencies) => dependencies.to_vec(),
        None => collect_prepared_catalog_dependencies(
            &state.catalog,
            std::iter::once(expanded_statement.clone()).chain(mutations.iter().cloned()),
        )?,
    };
    let mut locks = std::collections::BTreeMap::new();
    for dependency in dependencies {
        match dependency {
            PreparedCatalogDependency::Table { schema: table, .. } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name,
                    }
                    .get_lock_name(),
                    RelationLockMode::Shared,
                );
            }
            PreparedCatalogDependency::Sequence {
                schema: sequence, ..
            } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: sequence.schema_id,
                        name: sequence.name,
                    }
                    .get_lock_name(),
                    RelationLockMode::Shared,
                );
            }
            PreparedCatalogDependency::View { schema: view, .. } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: view.schema_id,
                        name: view.name,
                    }
                    .get_lock_name(),
                    RelationLockMode::Shared,
                );
            }
            PreparedCatalogDependency::Constraint { .. } => {}
        }
    }
    collect_foreign_key_relation_locks(
        state,
        std::iter::once(expanded_statement).chain(mutations),
        &mut locks,
    )?;
    let mut sequence_error = None;
    let _ = ast::visit_expressions(statement, |expression| -> std::ops::ControlFlow<()> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = executor::normalize_unqualified_object_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(name.as_str(), "nextval" | "currval" | "setval") {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::FunctionArguments::List(arguments) = &function.args else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))) =
            arguments.args.first()
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(name) = extract_runtime_sequence_name(argument) else {
            sequence_error = Some(PgError::create(
                SqlState::FeatureNotSupported,
                "computed sequence names are not implemented",
            ));
            return std::ops::ControlFlow::Break(());
        };
        match executor::normalize_sequence_name(name) {
            Ok(name) => match state.catalog.resolve_relation_name(&name) {
                Ok(name) => {
                    locks.insert(name.get_lock_name(), RelationLockMode::Shared);
                    std::ops::ControlFlow::Continue(())
                }
                Err(error) => {
                    sequence_error = Some(error);
                    std::ops::ControlFlow::Break(())
                }
            },
            Err(error) => {
                sequence_error = Some(error);
                std::ops::ControlFlow::Break(())
            }
        }
    });
    if let Some(error) = sequence_error {
        return Err(error);
    }
    Ok(locks.into_iter().collect())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn acquire_relation_locks<'a>(
    condvar: &Condvar,
    timeout: Duration,
    mut state: MutexGuard<'a, DatabaseState>,
    statement: &ast::Statement,
    prepared_dependencies: Option<&[PreparedCatalogDependency]>,
    xid: Xid,
    temporary_schema_id: SchemaId,
    isolation: IsolationLevel,
    mut snapshot: Snapshot,
) -> Result<(MutexGuard<'a, DatabaseState>, Snapshot)> {
    let deadline = (timeout != Duration::ZERO).then(|| Instant::now() + timeout);
    let ddl = matches!(parser::classify(statement), parser::StatementKind::Ddl);
    loop {
        if ddl {
            snapshot = Snapshot::create(&state.transactions).use_command(snapshot.command_id);
        }
        state.load_catalog(Some(xid), snapshot, Some(temporary_schema_id));
        let locks = collect_relation_locks(&state, statement, prepared_dependencies)?;
        let mut blocked = None;
        for (name, mode) in locks {
            match state.relation_locks.acquire(&name, xid, mode) {
                RelationLockAttempt::Acquired => condvar.notify_all(),
                RelationLockAttempt::Blocked(conflicts) => {
                    if state
                        .wait_for
                        .register_wait_dependencies(xid, &conflicts)
                        .is_some()
                    {
                        condvar.notify_all();
                    }
                    blocked = Some((name, conflicts));
                    break;
                }
            }
        }
        let Some((name, conflicts)) = blocked else {
            state.wait_for.clear_wait(xid);
            return Ok((state, snapshot));
        };
        if state.wait_for.take_victim(xid) {
            state.relation_locks.cancel_wait(&name, xid);
            state.wait_for.clear_wait(xid);
            condvar.notify_all();
            return Err(create_deadlock_error());
        }
        let mut timed_out = false;
        state = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.relation_locks.cancel_wait(&name, xid);
                state.wait_for.clear_wait(xid);
                condvar.notify_all();
                return Err(create_lock_timeout_error());
            }
            let (state, wait_result) = condvar
                .wait_timeout(state, remaining)
                .expect("database mutex is poisoned");
            timed_out = wait_result.timed_out();
            state
        } else {
            condvar.wait(state).expect("database mutex is poisoned")
        };
        state.relation_locks.cancel_wait(&name, xid);
        state.wait_for.clear_wait(xid);
        condvar.notify_all();
        if state.wait_for.take_victim(xid) {
            return Err(create_deadlock_error());
        }
        if timed_out {
            return Err(create_lock_timeout_error());
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
fn acquire_row_locks<'a>(
    condvar: &Condvar,
    timeout: Duration,
    mut state: MutexGuard<'a, DatabaseState>,
    target: RowLockTarget<'_>,
    xid: Xid,
    temporary_schema_id: SchemaId,
    isolation: IsolationLevel,
    mut snapshot: Snapshot,
    context: &executor::StatementExecutionContext,
) -> Result<(
    MutexGuard<'a, DatabaseState>,
    Snapshot,
    Vec<executor::RequiredRowLock>,
)> {
    let deadline = (timeout != Duration::ZERO).then(|| Instant::now() + timeout);
    loop {
        state.load_catalog(Some(xid), snapshot, Some(temporary_schema_id));
        let required = match target {
            RowLockTarget::Ctes(statement) => executor::collect_required_cte_row_locks(
                &state, statement, xid, &snapshot, context,
            )?,
            RowLockTarget::Statement(statement) => {
                executor::collect_required_row_locks(&state, statement, xid, &snapshot, context)?
            }
        };
        let mut blocked = None;
        for required_lock in &required {
            match state
                .row_locks
                .acquire(required_lock.key, xid, required_lock.mode)
            {
                RowLockAttempt::Acquired => condvar.notify_all(),
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
            state.wait_for.clear_wait(xid);
            return Ok((state, snapshot, required));
        };
        if state.wait_for.take_victim(xid) {
            state.row_locks.cancel_wait(key, xid);
            state.wait_for.clear_wait(xid);
            return Err(create_deadlock_error());
        }
        let mut timed_out = false;
        state = if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.row_locks.cancel_wait(key, xid);
                state.wait_for.clear_wait(xid);
                return Err(create_lock_timeout_error());
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
            return Err(create_lock_timeout_error());
        }
        if isolation == IsolationLevel::RepeatableRead
            && conflicts.iter().any(|holder| {
                matches!(
                    state.transactions.get_status(*holder),
                    Some(TransactionStatus::Committed(_))
                )
            })
        {
            return Err(PgError::create(
                SqlState::SerializationFailure,
                "could not serialize access due to concurrent update",
            ));
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
        }
    }
}

impl Db {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn create() -> Self {
        Db::create_builder().build()
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn create_builder() -> DbBuilder {
        DbBuilder {
            lock_timeout: Duration::from_secs(1),
            mock_time: false,
            seed: None,
            strict: false,
        }
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn create_session(&self) -> Session {
        let temporary_schema_id = self
            .state
            .lock()
            .expect("database mutex is poisoned")
            .catalog_history
            .create_temporary_schema_id();
        Session {
            db: self.clone(),
            temporary_schema_id,
            transaction: None,
            default_isolation: IsolationLevel::ReadCommitted,
            lock_timeout: self.default_lock_timeout,
            timezone: "UTC".into(),
            settings_undo: None,
            deferred_constraints: BTreeSet::new(),
            defer_all_constraints: false,
            deferred_foreign_keys_dirty: false,
            sequence_session: Arc::new(Mutex::new(executor::SequenceSessionState::default())),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn seed_trigger_catalog_for_test(&self, definition: &str) -> Result<()> {
        let statements = parser::parse(definition)?;
        let create = match statements.as_slice() {
            [ast::Statement::CreateTrigger(create)] => create,
            _ => {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "trigger fixture must be exactly one CREATE TRIGGER statement",
                ));
            }
        };
        let mut state = self.state.lock().expect("database mutex is poisoned");
        let xid = state.transactions.begin();
        let snapshot = Snapshot::create(&state.transactions).use_command(crate::txn::CommandId(0));
        state.load_catalog(Some(xid), snapshot, None);
        let previous = state.catalog.clone();
        if let Err(error) = executor::seed_trigger_catalog_for_test(&mut state, create) {
            abort_database_transaction(&mut state, xid);
            self.condvar.notify_all();
            return Err(error);
        }
        state.record_catalog_changes(&previous, xid, crate::txn::CommandId(0));
        state.transactions.commit(xid);
        prune_database_versions(&mut state);
        self.condvar.notify_all();
        Ok(())
    }
}
impl DbBuilder {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
    /// Enable a frozen, deterministic database clock. It begins at the Unix
    /// epoch and can subsequently be controlled through `Db::set_time` and
    /// `Db::advance_time`.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_mock_time_enabled(mut self, enabled: bool) -> Self {
        self.mock_time = enabled;
        self
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_random_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_strict_mode_enabled(mut self, enabled: bool) -> Self {
        self.strict = enabled;
        self
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn build(self) -> Db {
        Db {
            state: Arc::new(Mutex::new(DatabaseState::create())),
            condvar: Arc::new(Condvar::new()),
            default_lock_timeout: self.lock_timeout,
            clock: Arc::new(Mutex::new(if self.mock_time {
                DatabaseClock::Mock(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
            } else {
                DatabaseClock::Real
            })),
            rng: Arc::new(Mutex::new(match self.seed {
                Some(seed) => ChaCha12Rng::seed_from_u64(seed),
                None => ChaCha12Rng::from_os_rng(),
            })),
            strict: self.strict,
        }
    }
}
impl Default for Db {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn default() -> Self {
        Self::create()
    }
}
impl Db {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn read_clock(&self) -> chrono::DateTime<chrono::Utc> {
        match *self.clock.lock().expect("clock mutex is poisoned") {
            DatabaseClock::Real => chrono::Utc::now(),
            DatabaseClock::Mock(value) => value,
        }
    }

    /// ast::Set the frozen mock clock. Real-clock databases reject the operation.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_time(&self, time: chrono::DateTime<chrono::Utc>) -> Result<()> {
        let mut clock = self.clock.lock().expect("clock mutex is poisoned");
        match &mut *clock {
            DatabaseClock::Mock(value) => {
                *value = time;
                Ok(())
            }
            DatabaseClock::Real => Err(PgError::create(
                SqlState::InvalidParameterValue,
                "mock time is disabled",
            )),
        }
    }

    /// Advance the frozen mock clock by `duration`. Real-clock databases reject
    /// the operation.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn advance_time(&self, duration: chrono::Duration) -> Result<()> {
        let mut clock = self.clock.lock().expect("clock mutex is poisoned");
        match &mut *clock {
            DatabaseClock::Mock(value) => {
                *value = value.checked_add_signed(duration).ok_or_else(|| {
                    PgError::create(SqlState::NumericValueOutOfRange, "clock time out of range")
                })?;
                Ok(())
            }
            DatabaseClock::Real => Err(PgError::create(
                SqlState::InvalidParameterValue,
                "mock time is disabled",
            )),
        }
    }
}
impl Session {
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
                self.start_transaction(self.default_isolation, true);
            }
            match self.execute_statement(&statement, None, None) {
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
    fn try_execute_set_constraints(&mut self, sql: &str) -> Option<Result<StatementResult>> {
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

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        let statement = self.prepare(sql)?;
        self.execute_prepared(&statement, params)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let statement = self.prepare(sql)?;
        self.query_prepared(&statement, params)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn prepare(&mut self, sql: &str) -> Result<PreparedStatement> {
        let mut statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => return self.abort_with_error(error),
        };
        if statements.len() != 1 {
            return self.abort_with_error(PgError::create(
                SqlState::SyntaxError,
                "prepared statements require exactly one statement",
            ));
        }
        let statement = statements.pop().expect("statement count was checked");
        let parameter_count = match analyzer::count_parameters(&statement) {
            Ok(count) => count,
            Err(error) => return self.abort_with_error(error),
        };
        if matches!(statement, ast::Statement::CreateView(_)) && parameter_count != 0 {
            return self.abort_with_error(PgError::create(
                SqlState::UndefinedParameter,
                "there is no parameter in CREATE VIEW",
            ));
        }
        if matches!(
            self.transaction,
            Some(SessionTransactionState::Aborted { .. })
        ) && !matches!(
            &statement,
            ast::Statement::Commit { .. } | ast::Statement::Rollback { .. }
        ) {
            return Err(PgError::create(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        let prepared = {
            let mut state = self.db.state.lock().expect("database mutex is poisoned");
            let (xid, snapshot) = match self.transaction {
                Some(SessionTransactionState::Active(transaction)) => (
                    Some(transaction.xid),
                    transaction
                        .snapshot
                        .unwrap_or_else(|| Snapshot::create(&state.transactions))
                        .use_command(crate::txn::CommandId(transaction.next_command_id)),
                ),
                Some(SessionTransactionState::Aborted { .. }) => unreachable!(),
                None => (None, Snapshot::create(&state.transactions)),
            };
            state.load_catalog(xid, snapshot, Some(self.temporary_schema_id));
            analyzer::count_parameters(&statement)
                .and_then(|parameter_count| {
                    executor::expand_ctes_for_analysis(&statement, &state)
                        .map(|(statement, mutations)| (statement, mutations, parameter_count))
                })
                .and_then(|(statement, mutations, parameter_count)| {
                    let catalog_dependencies = collect_prepared_catalog_dependencies(
                        &state.catalog,
                        std::iter::once(statement.clone()).chain(mutations.iter().cloned()),
                    )?;
                    analyzer::substitute_typed_subqueries(&statement, &state.catalog).map(
                        |statement| (statement, mutations, parameter_count, catalog_dependencies),
                    )
                })
                .and_then(
                    |(statement, mutations, parameter_count, catalog_dependencies)| {
                        mutations
                            .iter()
                            .map(|mutation| {
                                analyzer::substitute_typed_subqueries(mutation, &state.catalog)
                            })
                            .collect::<Result<Vec<_>>>()
                            .map(|mutations| {
                                (statement, mutations, parameter_count, catalog_dependencies)
                            })
                    },
                )
                .and_then(
                    |(described, mutations, parameter_count, catalog_dependencies)| {
                        analyzer::infer_parameter_types_with_data_modifying_ctes(
                            &described,
                            &mutations,
                            &state.catalog,
                            parameter_count,
                        )
                        .and_then(|parameter_types| {
                            let described = analyzer::bind_parameters(
                                &described,
                                &parameter_types,
                                &vec![Value::Null; parameter_types.len()],
                            )?;
                            let columns =
                                executor::describe_query_result_columns(&state, &described)?;
                            let query_plan = executor::build_prepared_query_plan(
                                &state,
                                &statement,
                                &parameter_types,
                            )?;
                            Ok((parameter_types, columns, query_plan, catalog_dependencies))
                        })
                    },
                )
        };
        match prepared {
            Ok((parameter_types, columns, query_plan, catalog_dependencies)) => {
                Ok(PreparedStatement {
                    statement,
                    parameter_types,
                    columns,
                    query_plan,
                    catalog_dependencies,
                })
            }
            Err(error) => self.abort_with_error(error),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_prepared(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<u64> {
        match self.execute_prepared_statement(statement, params)? {
            StatementResult::Affected(rows) => Ok(rows),
            StatementResult::Query(_) => {
                reject_unsupported("use query_prepared for row-producing statements")
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn query_prepared(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<QueryResult> {
        match self.execute_prepared_statement(statement, params)? {
            StatementResult::Query(result) => Ok(result),
            StatementResult::Affected(_) => {
                reject_unsupported("query_prepared requires a row-producing statement")
            }
        }
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_prepared_statement(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<StatementResult> {
        let parameters;
        let (bound_statement, prepared_query) = if let Some(query_plan) = &statement.query_plan {
            parameters = match analyzer::coerce_parameters(&statement.parameter_types, params) {
                Ok(parameters) => parameters,
                Err(error) => return self.abort_with_error(error),
            };
            (
                Some(
                    match analyzer::bind_parameters(
                        &statement.statement,
                        &statement.parameter_types,
                        params,
                    ) {
                        Ok(statement) => statement,
                        Err(error) => return self.abort_with_error(error),
                    },
                ),
                Some((
                    query_plan,
                    parameters.as_slice(),
                    statement.columns.as_slice(),
                )),
            )
        } else {
            (
                Some(
                    match analyzer::bind_parameters(
                        &statement.statement,
                        &statement.parameter_types,
                        params,
                    ) {
                        Ok(statement) => statement,
                        Err(error) => return self.abort_with_error(error),
                    },
                ),
                None,
            )
        };
        let execution_statement = bound_statement.as_ref().unwrap_or(&statement.statement);
        let started_implicit_transaction = self.transaction.is_none();
        if started_implicit_transaction {
            self.start_transaction(self.default_isolation, true);
        }
        match self.execute_statement(
            execution_statement,
            prepared_query,
            Some(&statement.catalog_dependencies),
        ) {
            Ok(result) => {
                if started_implicit_transaction && self.is_transaction_implicit_batch() {
                    self.commit_transaction()?;
                }
                Ok(result)
            }
            Err(error) => {
                if started_implicit_transaction && self.is_transaction_implicit_batch() {
                    let _ = self.rollback_transaction();
                }
                Err(error)
            }
        }
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
    fn start_transaction(&mut self, isolation: IsolationLevel, implicit_batch: bool) {
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
    fn commit_transaction(&mut self) -> Result<()> {
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
            self.deferred_constraints.clear();
            self.defer_all_constraints = false;
            self.db.condvar.notify_all();
            return Ok(());
        }
        if self.deferred_foreign_keys_dirty
            && let Err(error) = executor::validate_deferred_foreign_keys(&state, transaction.xid)
        {
            if let Some((default_isolation, lock_timeout, timezone)) = self.settings_undo.take() {
                self.default_isolation = default_isolation;
                self.lock_timeout = lock_timeout;
                self.timezone = timezone;
            }
            self.deferred_constraints.clear();
            self.defer_all_constraints = false;
            self.deferred_foreign_keys_dirty = false;
            abort_database_transaction(&mut state, transaction.xid);
            self.db.condvar.notify_all();
            return Err(error);
        }
        let commit_seq = state.transactions.commit(transaction.xid);
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
        self.deferred_constraints.clear();
        self.defer_all_constraints = false;
        self.deferred_foreign_keys_dirty = false;
        self.db.condvar.notify_all();
        Ok(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rollback_transaction(&mut self) -> Result<()> {
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
        if let Some((default_isolation, lock_timeout, timezone)) = self.settings_undo.take() {
            self.default_isolation = default_isolation;
            self.lock_timeout = lock_timeout;
            self.timezone = timezone;
        }
        self.deferred_constraints.clear();
        self.defer_all_constraints = false;
        self.deferred_foreign_keys_dirty = false;
        abort_database_transaction(&mut state, xid);
        self.db.condvar.notify_all();
        Ok(())
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn mark_transaction_aborted(&mut self) {
        if let Some(SessionTransactionState::Active(transaction)) = self.transaction {
            self.transaction = Some(SessionTransactionState::Aborted {
                xid: transaction.xid,
                implicit_batch: transaction.implicit_batch,
            });
        }
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn is_transaction_implicit_batch(&self) -> bool {
        match self.transaction {
            Some(SessionTransactionState::Active(transaction)) => transaction.implicit_batch,
            Some(SessionTransactionState::Aborted { implicit_batch, .. }) => implicit_batch,
            None => false,
        }
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn abort_with_error<T>(&mut self, error: PgError) -> Result<T> {
        self.mark_transaction_aborted();
        Err(error)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn execute_statement(
        &mut self,
        statement: &ast::Statement,
        prepared_query: Option<(&executor::PreparedQueryPlan, &[Value], &[ColumnMeta])>,
        prepared_dependencies: Option<&[PreparedCatalogDependency]>,
    ) -> Result<StatementResult> {
        match statement {
            ast::Statement::Analyze(_) if !self.db.strict => {
                return Ok(StatementResult::Affected(0));
            }
            ast::Statement::Reset(reset)
                if !self.db.strict && is_tolerated_planner_reset(&reset.reset) =>
            {
                return Ok(StatementResult::Affected(0));
            }
            ast::Statement::Set(ast::Set::SetTimeZone { local: _, value }) => {
                self.timezone = parse_timezone(value)?;
                return Ok(StatementResult::Affected(0));
            }
            ast::Statement::ShowVariable { variable }
                if variable.len() == 1 && variable[0].value.eq_ignore_ascii_case("timezone") =>
            {
                return Ok(StatementResult::Query(QueryResult {
                    columns: vec![ColumnMeta {
                        name: "TimeZone".into(),
                        type_oid: crate::value::BaseType::Text.map_to_oid(),
                        typmod: -1,
                    }],
                    rows: vec![vec![Value::Text(self.timezone.clone())]],
                }));
            }
            ast::Statement::StartTransaction { modes, .. } => {
                return match self.transaction {
                    None => {
                        let isolation =
                            parse_isolation_level(modes)?.unwrap_or(self.default_isolation);
                        self.start_transaction(isolation, false);
                        Ok(StatementResult::Affected(0))
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
                        Ok(StatementResult::Affected(0))
                    }
                    Some(SessionTransactionState::Active(_)) => Ok(StatementResult::Affected(0)),
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
                    return Ok(StatementResult::Affected(0));
                }
                let Some(SessionTransactionState::Active(mut transaction)) = self.transaction
                else {
                    return Ok(StatementResult::Affected(0));
                };
                if transaction.statement_started && isolation != transaction.isolation {
                    return self.abort_with_error(PgError::create(
                        SqlState::ActiveSqlTransaction,
                        "transaction isolation level must be set before any query",
                    ));
                }
                transaction.isolation = isolation;
                self.transaction = Some(SessionTransactionState::Active(transaction));
                return Ok(StatementResult::Affected(0));
            }
            ast::Statement::Set(ast::Set::SingleAssignment {
                scope,
                hivevar,
                variable,
                values,
            }) => {
                if variable.to_string().eq_ignore_ascii_case("search_path") {
                    return self.abort_with_error(PgError::create(
                        SqlState::FeatureNotSupported,
                        "changing search_path is not implemented",
                    ));
                }
                if variable.to_string().eq_ignore_ascii_case("timezone") {
                    if *hivevar || values.len() != 1 {
                        return self.abort_with_error(PgError::create(
                            SqlState::FeatureNotSupported,
                            "TimeZone setting variant is not implemented",
                        ));
                    }
                    self.timezone = parse_timezone(&values[0])?;
                    return Ok(StatementResult::Affected(0));
                }
                if variable.to_string().eq_ignore_ascii_case("lock_timeout") {
                    if matches!(
                        self.transaction,
                        Some(SessionTransactionState::Aborted { .. })
                    ) {
                        return Err(PgError::create(
                            SqlState::InFailedSqlTransaction,
                            "current transaction is aborted",
                        ));
                    }
                    if *scope == Some(ast::ContextModifier::Local) || *hivevar || values.len() != 1
                    {
                        return self.abort_with_error(PgError::create(
                            SqlState::FeatureNotSupported,
                            "lock_timeout setting variant is not implemented",
                        ));
                    }
                    self.lock_timeout = match parse_lock_timeout(&values[0]) {
                        Ok(timeout) => timeout,
                        Err(error) => return self.abort_with_error(error),
                    };
                    return Ok(StatementResult::Affected(0));
                }
                if !self.db.strict && is_tolerated_planner_setting(variable) {
                    return Ok(StatementResult::Affected(0));
                }
            }
            ast::Statement::Commit { chain, .. } => {
                if *chain {
                    return self.abort_with_error(PgError::create(
                        SqlState::FeatureNotSupported,
                        "COMMIT AND CHAIN is not implemented",
                    ));
                }
                self.commit_transaction()?;
                return Ok(StatementResult::Affected(0));
            }
            ast::Statement::Rollback { chain, savepoint } => {
                if *chain || savepoint.is_some() {
                    return self.abort_with_error(PgError::create(
                        SqlState::FeatureNotSupported,
                        "ROLLBACK variant is not implemented",
                    ));
                }
                self.rollback_transaction()?;
                return Ok(StatementResult::Affected(0));
            }
            _ => {}
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
        let Some(SessionTransactionState::Active(mut transaction)) = self.transaction else {
            unreachable!("transaction must be active while executing a statement")
        };
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
        let acquired = match acquire_relation_locks(
            &condvar,
            self.lock_timeout,
            state,
            statement,
            prepared_dependencies,
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
        let prepared_dependency_error = prepared_dependencies.and_then(|dependencies| {
            dependencies.iter().find_map(|dependency| match dependency {
                PreparedCatalogDependency::Table { name, schema } => {
                    match state.catalog.require_named_table(name) {
                        Ok(table) if table.id == schema.id => {}
                        Ok(_) => {
                            return Some(PgError::create(
                                SqlState::FeatureNotSupported,
                                "cached plan must be replanned",
                            ));
                        }
                        Err(error) => return Some(error),
                    }
                    match state.catalog.require_table_by_id(schema.id) {
                        Ok(table) if table == schema => None,
                        Ok(_) => Some(PgError::create(
                            SqlState::FeatureNotSupported,
                            "cached plan must be replanned",
                        )),
                        Err(error) => {
                            let name = ResolvedRelationName {
                                schema_id: schema.schema_id,
                                name: schema.name.clone(),
                            };
                            if state.catalog.has_resolved_relation(&name) {
                                Some(PgError::create(
                                    SqlState::FeatureNotSupported,
                                    "cached plan must be replanned",
                                ))
                            } else {
                                Some(error)
                            }
                        }
                    }
                }
                PreparedCatalogDependency::Sequence { name, schema } => {
                    match state.catalog.require_named_sequence(name) {
                        Ok(sequence) if sequence.id == schema.id => {}
                        Ok(_) => {
                            return Some(PgError::create(
                                SqlState::FeatureNotSupported,
                                "cached plan must be replanned",
                            ));
                        }
                        Err(error) => return Some(error),
                    }
                    match state
                        .catalog
                        .iterate_sequences()
                        .find(|sequence| sequence.id == schema.id)
                    {
                        Some(sequence) if sequence == schema => None,
                        Some(_) => Some(PgError::create(
                            SqlState::FeatureNotSupported,
                            "cached plan must be replanned",
                        )),
                        None => {
                            let name = ResolvedRelationName {
                                schema_id: schema.schema_id,
                                name: schema.name.clone(),
                            };
                            if state.catalog.has_resolved_relation(&name) {
                                Some(PgError::create(
                                    SqlState::FeatureNotSupported,
                                    "cached plan must be replanned",
                                ))
                            } else {
                                Some(PgError::create(
                                    SqlState::UndefinedTable,
                                    format!("relation {:?} does not exist", schema.name),
                                ))
                            }
                        }
                    }
                }
                PreparedCatalogDependency::Constraint { table, id } => {
                    (!state.catalog.has_constraint(*table, *id)).then(|| {
                        PgError::create(
                            SqlState::FeatureNotSupported,
                            "cached plan must be replanned",
                        )
                    })
                }
                PreparedCatalogDependency::View { name, schema } => {
                    match state.catalog.require_named_view(name) {
                        Ok(view)
                            if view.id == schema.id
                                && view.schema_id == schema.schema_id
                                && view.name == schema.name
                                && view.columns == schema.columns
                                && view.query == schema.query
                                && view.dependencies == schema.dependencies
                                && view.column_dependencies == schema.column_dependencies =>
                        {
                            None
                        }
                        Ok(_) => Some(PgError::create(
                            SqlState::FeatureNotSupported,
                            "cached plan must be replanned",
                        )),
                        Err(error) => Some(error),
                    }
                }
            })
        });
        if let Some(error) = prepared_dependency_error {
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
            ) {
                Ok(rows) => Ok(StatementResult::Query(QueryResult {
                    columns: columns.to_vec(),
                    rows,
                })),
                Err(error) => {
                    drop(state);
                    self.abort_with_error(error)
                }
            };
        }
        let one_shot_plan = match executor::build_prepared_query_plan(&state, statement, &[]) {
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
            ) {
                Ok(rows) => Ok(StatementResult::Query(QueryResult {
                    columns: plan.columns().to_vec(),
                    rows,
                })),
                Err(error) => {
                    drop(state);
                    self.abort_with_error(error)
                }
            };
        }
        let statement_contains_dml = contains_dml(statement);
        let sequences = if statement_contains_dml || contains_sequence_function(statement) {
            executor::SequenceExecutionContext::create(
                &state.catalog,
                state.sequence_values.clone(),
                self.sequence_session.clone(),
            )
        } else {
            executor::SequenceExecutionContext::create_empty(
                state.sequence_values.clone(),
                self.sequence_session.clone(),
            )
        };
        let context = executor::StatementExecutionContext {
            command_id,
            transaction_timestamp: transaction.transaction_timestamp,
            statement_timestamp: self.db.read_clock(),
            clock_timestamp: self.db.read_clock(),
            rng: self.db.rng.clone(),
            sequences,
        };
        let (contains_cte, contains_subquery) = executor::detect_statement_features(statement);
        let mut acquired_row_locks = false;
        let cte_statement = if contains_cte {
            let (acquired_state, acquired_snapshot, locked_rows) = match acquire_row_locks(
                &condvar,
                self.lock_timeout,
                state,
                RowLockTarget::Ctes(statement),
                transaction.xid,
                self.temporary_schema_id,
                transaction.isolation,
                snapshot,
                &context,
            ) {
                Ok(acquired) => acquired,
                Err(error) => return self.abort_with_error(error),
            };
            acquired_row_locks = !locked_rows.is_empty();
            state = acquired_state;
            snapshot = acquired_snapshot;
            Some(
                match executor::materialize_ctes(
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
            self.lock_timeout,
            state,
            RowLockTarget::Statement(statement),
            transaction.xid,
            self.temporary_schema_id,
            transaction.isolation,
            snapshot,
            &context,
        ) {
            Ok(acquired) => acquired,
            Err(error) => return self.abort_with_error(error),
        };
        acquired_row_locks |= !locked_rows.is_empty();
        let mutation_targets =
            executor::mutation_locks_cover_targets(statement).then_some(locked_rows);
        let result = executor::execute_statement(
            &mut state,
            statement,
            transaction.xid,
            &snapshot,
            &self.deferred_constraints,
            self.defer_all_constraints,
            &context,
            mutation_targets,
        );
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn contains_dml(statement: &ast::Statement) -> bool {
    match statement {
        ast::Statement::Insert(_) | ast::Statement::Update(_) | ast::Statement::Delete(_) => true,
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
        if executor::normalize_unqualified_object_name(&function.name).is_ok_and(|name| {
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_tolerated_planner_setting(variable: &ast::ObjectName) -> bool {
    let variable = variable.to_string().to_ascii_lowercase();
    matches!(
        variable.as_str(),
        "work_mem"
            | "effective_cache_size"
            | "random_page_cost"
            | "seq_page_cost"
            | "cpu_tuple_cost"
            | "cpu_index_tuple_cost"
            | "cpu_operator_cost"
            | "parallel_setup_cost"
            | "parallel_tuple_cost"
            | "min_parallel_table_scan_size"
            | "min_parallel_index_scan_size"
            | "join_collapse_limit"
            | "from_collapse_limit"
            | "plan_cache_mode"
            | "geqo"
    ) || variable.starts_with("enable_")
        || variable.starts_with("jit_")
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_tolerated_planner_reset(reset: &ast::Reset) -> bool {
    match reset {
        ast::Reset::ALL | ast::Reset::SessionAuthorization => false,
        ast::Reset::ConfigurationParameter(variable) => is_tolerated_planner_setting(variable),
    }
}

impl PreparedStatement {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_parameter_types(&self) -> &[crate::value::BaseType] {
        &self.parameter_types
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_result_columns(&self) -> &[ColumnMeta] {
        &self.columns
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

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread};

    use crate::{
        txn::{Snapshot, find_visible_version},
        value::BaseType,
    };

    use super::*;

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn wait_until_relation_blocked(db: &Db) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if db.state.lock().unwrap().relation_locks.has_waiters() {
                return;
            }
            assert!(Instant::now() < deadline, "transaction did not block");
            thread::yield_now();
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn create_affected_results(rows: u64) -> Vec<StatementResult> {
        vec![StatementResult::Affected(rows)]
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn renames_trigger_catalog_identity_transactionally() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE accounts (id INTEGER)")
            .unwrap();
        db.seed_trigger_catalog_for_test(
            "CREATE TRIGGER audit_changes BEFORE INSERT ON accounts \
               FOR EACH ROW EXECUTE FUNCTION audit_changes()",
        )
        .unwrap();
        let trigger_id = db
            .state
            .lock()
            .unwrap()
            .catalog
            .require_table("accounts")
            .unwrap()
            .triggers[0]
            .id;

        session
            .execute(
                "BEGIN; \
                 ALTER TRIGGER audit_changes ON accounts RENAME TO audit_accounts; \
                 ROLLBACK",
            )
            .unwrap();
        session
            .execute("ALTER TRIGGER audit_changes ON accounts RENAME TO audit_accounts")
            .unwrap();
        let (renamed_id, renamed_name, definition_name) = {
            let state = db.state.lock().unwrap();
            let trigger = &state.catalog.require_table("accounts").unwrap().triggers[0];
            (
                trigger.id,
                trigger.name.clone(),
                executor::normalize_unqualified_object_name(&trigger.definition.name).unwrap(),
            )
        };
        assert_eq!(renamed_id, trigger_id);
        assert_eq!(renamed_name, "audit_accounts");
        assert_eq!(definition_name, "audit_accounts");
        assert_eq!(
            session
                .execute("ALTER TRIGGER audit_changes ON accounts RENAME TO ignored")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedObject
        );
        assert_eq!(
            session
                .execute("INSERT INTO accounts VALUES (1)")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn enforces_foreign_keys_and_keeps_failed_multi_row_writes_atomic() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn applies_foreign_key_actions_to_updates_and_deletes() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn validates_deferred_foreign_keys_at_commit_and_allows_repairs() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn set_constraints_changes_deferrable_foreign_key_timing() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn accepts_self_references_and_match_simple_nulls() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn parses_compares_and_generates_uuid_values() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn reproduces_seeded_uuid_generation_and_supports_v7() {
        let initial = chrono::DateTime::parse_from_rfc3339("2024-02-29T12:34:56Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let generate = |db: &Db| {
            db.set_time(initial).unwrap();
            let mut session = db.create_session();
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
        let first = generate(
            &Db::create_builder()
                .set_mock_time_enabled(true)
                .set_random_seed(42)
                .build(),
        );
        let second = generate(
            &Db::create_builder()
                .set_mock_time_enabled(true)
                .set_random_seed(42)
                .build(),
        );
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn processes_timestamp_values_and_timezone_setting() {
        let db = Db::create();
        let mut session = db.create_session();
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
            crate::value::BaseType::Timestamp.map_to_oid()
        );
        assert_eq!(
            result.columns[1].type_oid,
            crate::value::BaseType::TimestampTz.map_to_oid()
        );
        assert_eq!(
            result.rows[0][0].format_postgres_text(),
            "2024-02-29 12:34:56.789"
        );
        assert_eq!(
            result.rows[0][1].format_postgres_text(),
            "2024-02-29 09:34:56+00"
        );
        session.execute("SET TIME ZONE 'UTC'").unwrap();
        assert_eq!(
            session.query("SHOW TimeZone", &[]).unwrap().rows,
            vec![vec![Value::Text("UTC".into())]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_interval_calendar_and_clock_parts() {
        let db = Db::create();
        let mut session = db.create_session();
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
            crate::value::BaseType::Timestamp.map_to_oid()
        );
        assert_eq!(
            result.columns[1].type_oid,
            crate::value::BaseType::Interval.map_to_oid()
        );
        assert_eq!(
            result.rows[0][0].format_postgres_text(),
            "2024-03-02 15:04:05"
        );
        assert_eq!(
            result.rows[0][1].format_postgres_text(),
            "2 mons 4 days 06:08:10"
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn freezes_and_controls_mock_clock() {
        let db = Db::create_builder().set_mock_time_enabled(true).build();
        let initial = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        db.set_time(initial).unwrap();
        assert_eq!(db.read_clock(), initial);
        db.advance_time(chrono::Duration::minutes(90)).unwrap();
        assert_eq!(db.read_clock(), initial + chrono::Duration::minutes(90));
        assert!(Db::create().set_time(initial).is_err());
        assert!(
            Db::create()
                .advance_time(chrono::Duration::seconds(1))
                .is_err()
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn observes_timestamp_function_boundaries() {
        let db = Db::create_builder().set_mock_time_enabled(true).build();
        let initial = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        db.set_time(initial).unwrap();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_postgres_date_and_time_special_forms() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE values_table (day DATE, moment TIME(6))")
            .unwrap();
        session
            .execute("INSERT INTO values_table VALUES ('infinity', '24:00:00')")
            .unwrap();
        let result = session
            .query("SELECT day, moment FROM values_table", &[])
            .unwrap();
        assert_eq!(result.rows[0][0].format_postgres_text(), "infinity");
        assert_eq!(result.rows[0][1].format_postgres_text(), "24:00:00");
        assert_eq!(result.columns[0].type_oid, BaseType::Date.map_to_oid());
        assert_eq!(result.columns[1].type_oid, BaseType::Time.map_to_oid());
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rejects_partially_null_match_full_keys() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn creates_and_drops_tables_in_autocommit() {
        let db = Db::create();
        let mut session = db.create_session();
        assert_eq!(session.execute("CREATE TABLE items (id INTEGER NOT NULL, name VARCHAR(12), amount NUMERIC(8, 2))").unwrap(), create_affected_results(0));
        let state = db.state.lock().unwrap();
        let table = state.catalog.require_table("items").unwrap();
        assert_eq!(table.columns[0].data_type.base, BaseType::Int4);
        assert_eq!(table.columns[1].data_type.typmod, 16);
        assert_eq!(table.columns[2].data_type.typmod, (8 << 16) + 2 + 4);
        drop(state);
        assert_eq!(
            session.execute("DROP TABLE items").unwrap(),
            create_affected_results(0)
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn resolves_qualified_and_temporary_relations_per_session() {
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first
            .execute("CREATE TABLE public.items (id INTEGER); INSERT INTO public.items VALUES (1)")
            .unwrap();
        first
            .execute("CREATE TEMP TABLE items (id INTEGER) ON COMMIT PRESERVE ROWS")
            .unwrap();
        second
            .execute("CREATE TEMPORARY TABLE pg_temp.items (id INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (2)").unwrap();
        second.execute("INSERT INTO items VALUES (3)").unwrap();

        assert_eq!(
            first.query("SELECT id FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
        assert_eq!(
            first
                .query("SELECT id FROM public.items", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            second
                .query("SELECT id FROM pg_temp.items", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3)]]
        );
        assert_eq!(
            first
                .query(
                    "WITH first_value AS (SELECT id FROM items), \
                     items AS (SELECT 99 AS id) SELECT id FROM first_value",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn applies_temporary_relation_transaction_and_session_lifetimes() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TEMP TABLE retained (id INTEGER); INSERT INTO retained VALUES (1)")
            .unwrap();
        assert_eq!(
            session.query("SELECT id FROM retained", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        {
            let mut transaction = session.begin().unwrap();
            transaction.execute("DROP TABLE retained").unwrap();
            transaction.rollback().unwrap();
        }
        assert_eq!(
            session.query("SELECT id FROM retained", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        {
            let mut transaction = session.begin().unwrap();
            transaction
                .execute("CREATE TEMP TABLE fleeting (id INTEGER) ON COMMIT DROP")
                .unwrap();
            transaction
                .execute("INSERT INTO fleeting VALUES (2)")
                .unwrap();
            transaction.commit().unwrap();
        }
        assert_eq!(
            session
                .query("SELECT * FROM pg_temp.fleeting", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        drop(session);

        let mut replacement = db.create_session();
        assert_eq!(
            replacement
                .query("SELECT * FROM pg_temp.retained", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        replacement
            .execute("CREATE TEMP TABLE retained (id INTEGER)")
            .unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn keeps_qualified_prepared_relations_and_sequences_stable() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(
                "CREATE TABLE public.items (id INTEGER); \
                 INSERT INTO public.items VALUES (1); \
                 CREATE TABLE public.generated (id SERIAL); \
                 CREATE TABLE public.owned (id SERIAL); \
                 CREATE SEQUENCE public.ids START WITH 10; \
                 CREATE TEMP SEQUENCE ids START WITH 20; \
                 CREATE TEMP SEQUENCE generated_id_seq START WITH 100; \
                 CREATE TEMP TABLE owned (id SERIAL)",
            )
            .unwrap();
        let prepared = session.prepare("SELECT id FROM public.items").unwrap();
        session
            .execute("CREATE TEMP TABLE items (id INTEGER); INSERT INTO items VALUES (2)")
            .unwrap();
        assert_eq!(
            session.query_prepared(&prepared, &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session.query("SELECT nextval('ids')", &[]).unwrap().rows,
            vec![vec![Value::Int8(20)]]
        );
        assert_eq!(
            session
                .query("SELECT nextval('public.ids')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(10)]]
        );
        session
            .execute("INSERT INTO public.generated DEFAULT VALUES")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT id FROM public.generated", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query("SELECT nextval('generated_id_seq')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(100)]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT pg_get_serial_sequence('public.owned', 'id'), \
                            pg_get_serial_sequence('pg_temp.owned', 'id')",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![
                Value::Text("public.owned_id_seq".into()),
                Value::Text("pg_temp.owned_id_seq".into()),
            ]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn prevents_prepared_statements_from_retargeting_temporary_shadows() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(
                "CREATE TABLE public.items (id INTEGER); \
                 CREATE TABLE public.dropped (id INTEGER); \
                 CREATE SEQUENCE public.ids START WITH 10",
            )
            .unwrap();
        let insert = session.prepare("INSERT INTO items VALUES (1)").unwrap();
        let aggregate = session.prepare("SELECT count(*) FROM items").unwrap();
        let drop_table = session.prepare("DROP TABLE dropped").unwrap();
        let next_value = session.prepare("SELECT nextval('ids')").unwrap();
        let cast_next_value = session.prepare("SELECT nextval('ids'::text)").unwrap();
        session
            .execute(
                "CREATE TEMP TABLE items (id INTEGER); \
                 CREATE TEMP TABLE dropped (id INTEGER); \
                 CREATE TEMP SEQUENCE ids START WITH 20",
            )
            .unwrap();

        assert_eq!(
            session.execute_prepared(&insert, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .query_prepared(&aggregate, &[])
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute_prepared(&drop_table, &[])
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .query_prepared(&next_value, &[])
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .query_prepared(&cast_next_value, &[])
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert!(session.query("SELECT * FROM public.dropped", &[]).is_ok());
        assert!(session.query("SELECT * FROM pg_temp.dropped", &[]).is_ok());
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn binds_explicit_sequence_defaults_before_temporary_shadowing() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(
                "CREATE SEQUENCE public.ids; \
                 CREATE TABLE public.generated (id BIGINT DEFAULT nextval('ids')); \
                 CREATE TEMP SEQUENCE ids START WITH 100",
            )
            .unwrap();
        assert_eq!(
            session
                .query(
                    "INSERT INTO public.generated DEFAULT VALUES RETURNING id",
                    &[]
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
        assert_eq!(
            session
                .execute("DROP SEQUENCE public.ids")
                .unwrap_err()
                .sqlstate,
            SqlState::DependentObjectsStillExist
        );
        assert_eq!(
            session
                .execute("CREATE TABLE missing_default (id BIGINT DEFAULT nextval('missing'))")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session
                .execute("CREATE TABLE wrong_kind (id BIGINT DEFAULT nextval('generated'))")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute("CREATE TABLE permanent_temp_default (id BIGINT DEFAULT nextval('ids'))")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute(
                    "CREATE TEMP TABLE temporary_public_default \
                     (id BIGINT DEFAULT nextval('public.ids'))",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute(
                    "CREATE TABLE compound_default \
                     (id BIGINT DEFAULT nextval('public.ids') + 1)",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute(
                    "CREATE TABLE cast_default \
                     (id BIGINT DEFAULT nextval('public.ids')::smallint)",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute(
                    "CREATE TABLE currval_default \
                     (id BIGINT DEFAULT currval('public.ids'))",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute(
                    "CREATE TABLE setval_default \
                     (id BIGINT DEFAULT setval('public.ids', 10))",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rejects_unsupported_relation_namespaces_explicitly() {
        let db = Db::create();
        let mut session = db.create_session();
        assert_eq!(
            session
                .execute("CREATE TABLE private.items (id INTEGER)")
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidSchemaName
        );
        assert_eq!(
            session
                .execute("CREATE TEMP TABLE public.items (id INTEGER)")
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTableDefinition
        );
        assert_eq!(
            session
                .execute("SET search_path TO public")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .query("SELECT pg_get_serial_sequence('private.items', 'id')", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidSchemaName
        );
        assert_eq!(
            session
                .query("SELECT pg_get_serial_sequence('public.missing', 'id')", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        session
            .execute("CREATE SEQUENCE public.not_a_table")
            .unwrap();
        assert_eq!(
            session
                .query(
                    "SELECT pg_get_serial_sequence('public.not_a_table', 'id')",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedColumn
        );
        session
            .execute("CREATE TEMP TABLE temporary_parent (id INTEGER PRIMARY KEY)")
            .unwrap();
        assert_eq!(
            session
                .execute(
                    "CREATE TABLE public.permanent_child \
                     (parent_id INTEGER REFERENCES temporary_parent(id))",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTableDefinition
        );
        session
            .execute("CREATE TABLE public.permanent_parent (id INTEGER PRIMARY KEY)")
            .unwrap();
        assert_eq!(
            session
                .execute(
                    "CREATE TEMP TABLE temporary_child \
                     (parent_id INTEGER REFERENCES public.permanent_parent(id))",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTableDefinition
        );
        assert_eq!(
            session
                .execute(
                    "CREATE SEQUENCE public.cross_owned \
                     OWNED BY temporary_parent.id",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::ObjectNotInPrerequisiteState
        );
        assert_eq!(
            session
                .execute(
                    "CREATE TEMP SEQUENCE cross_owned \
                     OWNED BY public.permanent_parent.id",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::ObjectNotInPrerequisiteState
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn selects_projections_with_metadata_in_row_id_order() {
        let db = Db::create();
        let mut session = db.create_session();
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
                    type_oid: BaseType::Text.map_to_oid(),
                    typmod: -1,
                },
                ColumnMeta {
                    name: "id".into(),
                    type_oid: BaseType::Int4.map_to_oid(),
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn binds_typed_parameters_and_prepared_statements() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_comparison_coercion_for_point_lookup_candidates() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(
                "CREATE TABLE indexed_smallints (id SMALLINT PRIMARY KEY); \
                 CREATE TABLE scanned_smallints (id SMALLINT); \
                 CREATE TABLE indexed_integers (id INTEGER PRIMARY KEY); \
                 INSERT INTO indexed_smallints VALUES (1); \
                 INSERT INTO scanned_smallints VALUES (1); \
                 INSERT INTO indexed_integers VALUES (1)",
            )
            .unwrap();

        assert_eq!(
            session
                .query("SELECT id FROM indexed_smallints WHERE id = 1", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int2(1)]]
        );
        assert_eq!(
            session
                .query("SELECT id FROM scanned_smallints WHERE id = 1", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int2(1)]]
        );
        assert_eq!(
            session
                .query("SELECT id FROM indexed_integers WHERE id = 1.0", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn skips_scans_for_missing_prepared_unique_keys() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute(
                "CREATE SEQUENCE point_probe START WITH 1; \
                 CREATE TABLE items (id INTEGER PRIMARY KEY); \
                 INSERT INTO items VALUES (1), (2)",
            )
            .unwrap();
        let statement = session
            .prepare(
                "SELECT id FROM items \
                 WHERE nextval('point_probe') > 0 AND id = $1",
            )
            .unwrap();

        assert!(
            session
                .query_prepared(&statement, &[Value::Int4(99)])
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            session
                .query("SELECT nextval('point_probe')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn finishes_implicit_prepared_transactions() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();
        let insert = session.prepare("INSERT INTO items VALUES ($1)").unwrap();

        assert_eq!(session.execute_prepared(&insert, &[Value::Int4(1)]), Ok(1));
        assert!(session.transaction.is_none());
        assert_eq!(
            session
                .execute_prepared(&insert, &[Value::Int4(1)])
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
        assert!(session.transaction.is_none());
        assert_eq!(session.execute_prepared(&insert, &[Value::Int4(2)]), Ok(1));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn matches_prepared_statement_parameter_contract() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn execute_returns_each_multi_statement_result() {
        let db = Db::create();
        let mut session = db.create_session();

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
                            type_oid: BaseType::Int4.map_to_oid(),
                            typmod: -1,
                        },
                        ColumnMeta {
                            name: "name".into(),
                            type_oid: BaseType::Text.map_to_oid(),
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rolls_back_implicit_batches_at_first_error() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn splits_simple_query_transactions_at_explicit_controls() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn reports_metadata_for_every_phase_one_type() {
        let db = Db::create();
        let mut session = db.create_session();
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
                (BaseType::Bool.map_to_oid(), -1),
                (BaseType::Int2.map_to_oid(), -1),
                (BaseType::Int4.map_to_oid(), -1),
                (BaseType::Int8.map_to_oid(), -1),
                (BaseType::Float4.map_to_oid(), -1),
                (BaseType::Float8.map_to_oid(), -1),
                (BaseType::Numeric.map_to_oid(), (5 << 16) + 2 + 4),
                (BaseType::Text.map_to_oid(), -1),
                (BaseType::Varchar.map_to_oid(), 3 + 4),
                (BaseType::Bpchar.map_to_oid(), 2 + 4),
                (BaseType::Bytea.map_to_oid(), -1),
            ]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn excludes_other_transactions_uncommitted_rows_from_selects() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        let mut state = db.state.lock().unwrap();
        let writer = state.transactions.begin();
        let table_id = state.catalog.require_table("items").unwrap().id;
        state.tables.get_mut(&table_id).unwrap().insert(
            writer,
            crate::txn::CommandId(0),
            vec![Value::Int4(1)],
        );
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn reports_unknown_tables_and_columns_in_selects() {
        let db = Db::create();
        let mut session = db.create_session();

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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn evaluates_arithmetic_and_comparison_projections() {
        let db = Db::create();
        let mut session = db.create_session();
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
                (BaseType::Int4.map_to_oid(), -1),
                (BaseType::Int4.map_to_oid(), -1),
                (BaseType::Int4.map_to_oid(), -1),
                (BaseType::Int4.map_to_oid(), -1),
                (BaseType::Int4.map_to_oid(), -1),
                (BaseType::Bool.map_to_oid(), -1),
                (BaseType::Bool.map_to_oid(), -1),
                (BaseType::Numeric.map_to_oid(), -1),
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn evaluates_case_and_common_scalar_functions() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn accepts_minimum_int4_literal_in_simple_case() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn supports_all_phase_one_numeric_types_in_abs() {
        let db = Db::create();
        let mut session = db.create_session();
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
        let table_id = state.catalog.require_table("numbers").unwrap().id;
        state.tables.get_mut(&table_id).unwrap().insert(
            xid,
            crate::txn::CommandId(0),
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn reports_case_and_function_type_errors() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn coerces_phase_one_types_in_all_cast_contexts() {
        let db = Db::create();
        let mut session = db.create_session();
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
                        real_value + int_value,
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
                Value::Float8(9.0),
                Value::Float8(7.0),
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

        session
            .execute("UPDATE types SET real_value = -0.02")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT real_value IS DISTINCT FROM -0.02 FROM types", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Bool(true)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn reports_postgres_coercion_error_categories() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn orders_rows_by_columns_expressions_and_output_positions() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn applies_postgres_order_by_null_defaults_and_validates_positions() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn applies_limits_and_offsets_after_ordering() {
        let db = Db::create();
        let mut session = db.create_session();
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
                .query(
                    "SELECT id FROM items ORDER BY id DESC LIMIT 2 OFFSET 1",
                    &[]
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(4)], vec![Value::Int4(3)]]
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rejects_negative_limit_and_offset() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn applies_defaults_to_inserted_and_updated_rows() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn enforces_not_null_after_defaults_and_assignments() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn enforces_check_constraints_on_insert_and_update() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn enforces_primary_and_multi_column_unique_constraints() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rebuilds_unique_indexes_after_rollback() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn controls_insert_and_update_visibility_with_explicit_transactions() {
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn controls_snapshot_lifetime_by_isolation_level() {
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn reclaims_deleted_rows_between_autocommit_statements() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();

        for id in 0..100 {
            session
                .execute(&format!("INSERT INTO items VALUES ({id})"))
                .unwrap();
            session
                .execute(&format!("DELETE FROM items WHERE id = {id}"))
                .unwrap();
        }

        let state = db.state.lock().unwrap();
        let table_id = state.catalog.require_table("items").unwrap().id;
        assert_eq!(
            state
                .tables
                .get(&table_id)
                .unwrap()
                .iterate_version_chains()
                .count(),
            0
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn retains_deleted_rows_until_repeatable_read_snapshot_finishes() {
        let db = Db::create();
        let mut reader = db.create_session();
        let mut writer = db.create_session();
        writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
        writer.execute("INSERT INTO items VALUES (1)").unwrap();
        reader
            .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );

        writer.execute("DELETE FROM items WHERE id = 1").unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        {
            let state = db.state.lock().unwrap();
            let table_id = state.catalog.require_table("items").unwrap().id;
            assert_eq!(
                state
                    .tables
                    .get(&table_id)
                    .unwrap()
                    .iterate_version_chains()
                    .count(),
                1
            );
        }

        reader.execute("ROLLBACK").unwrap();
        let state = db.state.lock().unwrap();
        let table_id = state.catalog.require_table("items").unwrap().id;
        assert_eq!(
            state
                .tables
                .get(&table_id)
                .unwrap()
                .iterate_version_chains()
                .count(),
            0
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn retains_dropped_table_until_repeatable_read_snapshot_finishes() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut reader = db.create_session();
        let mut writer = db.create_session();
        writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
        writer.execute("INSERT INTO items VALUES (1)").unwrap();
        let table_id = db
            .state
            .lock()
            .unwrap()
            .catalog
            .require_table("items")
            .unwrap()
            .id;
        reader
            .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(writer.execute("DROP TABLE items"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        assert!(db.state.lock().unwrap().tables.contains_key(&table_id));

        reader.execute("ROLLBACK").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
        assert!(!db.state.lock().unwrap().tables.contains_key(&table_id));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn exposes_catalog_and_rows_atomically_at_commit() {
        let mut state = DatabaseState::create();
        let writer = state.transactions.begin();
        let concurrent_snapshot = Snapshot::create(&state.transactions);
        state.load_catalog(
            Some(writer),
            concurrent_snapshot.use_command(crate::txn::CommandId(0)),
            None,
        );
        let previous = state.catalog.clone();
        let table_id = state
            .catalog
            .create_table(
                "items".into(),
                vec![crate::catalog::ColumnDef {
                    name: "id".into(),
                    data_type: crate::value::PgType::create(BaseType::Int4),
                    nullable: false,
                    default: None,
                    default_sequence: None,
                    identity: None,
                }],
                vec![],
            )
            .unwrap();
        let schema = state.catalog.require_table("items").unwrap().clone();
        let mut table = Table::create(schema);
        table.insert(writer, crate::txn::CommandId(0), vec![Value::Int4(1)]);
        state.tables.insert(table_id, table);
        state.record_catalog_changes(&previous, writer, crate::txn::CommandId(0));
        let concurrent_reader = state.transactions.begin();

        assert!(
            state
                .catalog_history
                .materialize(None, concurrent_snapshot, &state.transactions)
                .require_table("items")
                .is_err()
        );
        assert!(
            crate::txn::find_visible_version(
                state
                    .tables
                    .get(&table_id)
                    .unwrap()
                    .iterate_version_chains()
                    .next()
                    .unwrap()
                    .1,
                &concurrent_snapshot,
                concurrent_reader,
                &state.transactions,
            )
            .is_none()
        );

        state.transactions.commit(writer);
        let committed_snapshot = Snapshot::create(&state.transactions);
        let committed_reader = state.transactions.begin();
        assert_eq!(
            state
                .catalog_history
                .materialize(None, committed_snapshot, &state.transactions)
                .require_table("items")
                .unwrap()
                .id,
            table_id
        );
        assert_eq!(
            crate::txn::find_visible_version(
                state
                    .tables
                    .get(&table_id)
                    .unwrap()
                    .iterate_version_chains()
                    .next()
                    .unwrap()
                    .1,
                &committed_snapshot,
                committed_reader,
                &state.transactions,
            )
            .unwrap()
            .row,
            vec![Value::Int4(1)]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn retains_dropped_table_while_a_read_committed_writer_uses_it() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut writer = db.create_session();
        let mut dropper = db.create_session();
        writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
        writer.execute("INSERT INTO items VALUES (1)").unwrap();
        let table_id = db
            .state
            .lock()
            .unwrap()
            .catalog
            .require_table("items")
            .unwrap()
            .id;

        writer.execute("BEGIN").unwrap();
        writer.execute("UPDATE items SET id = 2").unwrap();
        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP TABLE items"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        assert!(db.state.lock().unwrap().tables.contains_key(&table_id));

        writer.execute("COMMIT").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
        assert!(!db.state.lock().unwrap().tables.contains_key(&table_id));
        assert_eq!(
            writer
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn prepared_query_does_not_retarget_a_recreated_relation() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();
        let prepared = session.prepare("SELECT id FROM items").unwrap();

        session.execute("DROP TABLE items").unwrap();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (2)").unwrap();

        assert_eq!(
            session.query_prepared(&prepared, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn prepared_fallback_queries_and_mutations_do_not_retarget_recreated_relations() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();
        let ordered = session.prepare("SELECT id FROM items ORDER BY id").unwrap();
        let update = session.prepare("UPDATE items SET id = id + 1").unwrap();

        session.execute("DROP TABLE items").unwrap();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (2)").unwrap();

        assert_eq!(
            session.query_prepared(&ordered, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session.execute_prepared(&update, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session.query("SELECT id FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn prepared_queries_reject_changed_table_schema_versions() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        let prepared = session.prepare("SELECT * FROM items").unwrap();
        {
            let mut state = db.state.lock().unwrap();
            let changer = state.transactions.begin();
            let snapshot = Snapshot::create(&state.transactions);
            state.load_catalog(Some(changer), snapshot, None);
            let previous = state.catalog.clone();
            state
                .catalog
                .require_table_mut("items")
                .unwrap()
                .columns
                .push(crate::catalog::ColumnDef {
                    name: "value".into(),
                    data_type: crate::value::PgType::create(BaseType::Text),
                    nullable: true,
                    default: None,
                    default_sequence: None,
                    identity: None,
                });
            state.record_catalog_changes(&previous, changer, crate::txn::CommandId(0));
            state.transactions.commit(changer);
        }

        assert_eq!(
            session.query_prepared(&prepared, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn prepared_subqueries_ddl_sequences_and_constraints_keep_catalog_dependencies() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();
        session
            .execute("CREATE TABLE permissions (item_id INTEGER)")
            .unwrap();
        session.execute("CREATE SEQUENCE ids").unwrap();
        session.execute("CREATE SEQUENCE \"Ids\"").unwrap();
        assert!(session.query("SELECT nextval('IDS')", &[]).is_ok());
        assert!(session.query("SELECT nextval('public.ids')", &[]).is_ok());
        assert!(session.query("SELECT nextval('\"Ids\"')", &[]).is_ok());
        let subquery = session
            .prepare(
                "SELECT id FROM items WHERE EXISTS (SELECT 1 FROM permissions WHERE item_id = items.id)",
            )
            .unwrap();
        let drop_table = session.prepare("DROP TABLE permissions").unwrap();
        let sequence = session.prepare("SELECT nextval('IDS')").unwrap();
        let qualified_sequence = session.prepare("SELECT nextval('public.ids')").unwrap();
        let quoted_sequence = session.prepare("SELECT nextval('\"Ids\"')").unwrap();
        let conflict = session
            .prepare("INSERT INTO items VALUES (1) ON CONFLICT ON CONSTRAINT items_pkey DO NOTHING")
            .unwrap();
        assert!(conflict.catalog_dependencies.iter().any(|dependency| {
            matches!(dependency, PreparedCatalogDependency::Constraint { .. })
        }));

        session.execute("DROP TABLE permissions").unwrap();
        session
            .execute("CREATE TABLE permissions (item_id INTEGER)")
            .unwrap();
        assert_eq!(
            session.query_prepared(&subquery, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .execute_prepared(&drop_table, &[])
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert!(session.prepare("SELECT * FROM permissions").is_ok());

        session.execute("DROP SEQUENCE ids").unwrap();
        session.execute("CREATE SEQUENCE ids").unwrap();
        session.execute("DROP SEQUENCE \"Ids\"").unwrap();
        session.execute("CREATE SEQUENCE \"Ids\"").unwrap();
        assert_eq!(
            session.query_prepared(&sequence, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .query_prepared(&qualified_sequence, &[])
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .query_prepared(&quoted_sequence, &[])
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn follows_postgres_isolation_selection_order() {
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn blocks_and_rechecks_read_committed_writer_after_commit() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();
        assert_eq!(
            first.query("SELECT amount FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(3)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn allows_blocked_writer_after_holder_rollback() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();
        assert_eq!(
            first.query("SELECT amount FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn waits_for_on_conflict_rows_and_rechecks_commit_or_rollback() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();
        first.execute("BEGIN").unwrap();
        first.execute("INSERT INTO items VALUES (1)").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.execute("INSERT INTO items VALUES (1) ON CONFLICT (id) DO NOTHING"))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();

        let mut first = db.create_session();
        let mut second = db.create_session();
        first.execute("BEGIN").unwrap();
        first.execute("INSERT INTO items VALUES (2)").unwrap();
        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.execute("INSERT INTO items VALUES (2) ON CONFLICT (id) DO NOTHING"))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("ROLLBACK").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rechecks_concurrent_on_conflict_updates_for_each_isolation_level() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT UNIQUE)")
            .unwrap();
        first.execute("BEGIN").unwrap();
        first
            .execute("INSERT INTO items VALUES (1, 'old')")
            .unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.execute(
                    "INSERT INTO items VALUES (1, 'committed') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
                ))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();

        let mut first = db.create_session();
        let mut second = db.create_session();
        first.execute("BEGIN").unwrap();
        first
            .execute("INSERT INTO items VALUES (2, 'old')")
            .unwrap();
        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.execute(
                    "INSERT INTO items VALUES (2, 'after rollback') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
                ))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("ROLLBACK").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();

        let mut first = db.create_session();
        let mut second = db.create_session();
        second
            .execute("BEGIN ISOLATION LEVEL REPEATABLE READ")
            .unwrap();
        second.query("SELECT * FROM items", &[]).unwrap();
        first.execute("BEGIN").unwrap();
        first
            .execute("UPDATE items SET value = 'holder' WHERE id = 1")
            .unwrap();
        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let error = second
                .execute(
                    "INSERT INTO items VALUES (1, 'repeatable') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
                )
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

        let mut first = db.create_session();
        let mut second = db.create_session();
        first.execute("BEGIN").unwrap();
        first
            .execute("INSERT INTO items VALUES (3, 'reserved')")
            .unwrap();
        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let error = second
                .execute(
                    "INSERT INTO items VALUES (1, 'reserved') \
                     ON CONFLICT (id) DO UPDATE SET value = excluded.value",
                )
                .unwrap_err();
            result_sender.send(error.sqlstate).unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            SqlState::UniqueViolation
        );
        handle.join().unwrap();

        assert_eq!(
            first
                .query("SELECT id, value FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(1), Value::Text("holder".into())],
                vec![Value::Int4(2), Value::Text("after rollback".into())],
                vec![Value::Int4(3), Value::Text("reserved".into())],
            ]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn aborts_newest_deadlocked_transaction_and_allows_survivor() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut setup = db.create_session();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
            let abort_with_error = second.query("SELECT * FROM items", &[]).unwrap_err();
            second.execute("ROLLBACK").unwrap();
            victim_sender
                .send((error.sqlstate, abort_with_error.sqlstate))
                .unwrap();
        });
        wait_until_blocked(&db);

        assert_eq!(
            first.execute("UPDATE items SET amount = 1 WHERE id = 2"),
            Ok(create_affected_results(1))
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn fails_repeatable_read_writer_after_concurrent_commit() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn applies_update_and_share_row_lock_compatibility() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
        let mut third = db.create_session();
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
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();

        first.execute("INSERT INTO items VALUES (2)").unwrap();
        first.execute("BEGIN").unwrap();
        first
            .query("SELECT * FROM items WHERE id = 2 FOR UPDATE", &[])
            .unwrap();
        let mut writer = db.create_session();
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
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn controls_waits_with_builder_and_session_lock_timeouts() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_millis(40))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn restores_row_after_rolled_back_update() {
        let db = Db::create();
        let mut session = db.create_session();
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
        assert_eq!(
            session.execute("DELETE FROM items").unwrap(),
            create_affected_results(1)
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn deletes_matching_rows_and_all_rows() {
        let db = Db::create();
        let mut session = db.create_session();
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
            create_affected_results(1)
        );
        assert_eq!(
            session.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Null],
            ]
        );
        assert_eq!(
            session.execute("DELETE FROM items").unwrap(),
            create_affected_results(2)
        );
        assert!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn matches_delete_visibility_to_transaction_outcome() {
        let db = Db::create();
        let mut writer = db.create_session();
        let mut reader = db.create_session();
        writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
        writer
            .execute("INSERT INTO items VALUES (1), (2), (3)")
            .unwrap();

        writer.execute("BEGIN").unwrap();
        assert_eq!(
            writer.execute("DELETE FROM items WHERE id = 1").unwrap(),
            create_affected_results(1)
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn delete_requires_a_boolean_where_expression() {
        let db = Db::create();
        let mut session = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn aborts_explicit_transactions_after_errors_and_rolls_back_on_drop() {
        let db = Db::create();
        let mut session = db.create_session();
        let mut reader = db.create_session();
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rolls_back_created_tables_rows_constraints_and_sequences() {
        let db = Db::create();
        let mut session = db.create_session();
        let mut reader = db.create_session();

        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .execute("CREATE TABLE items (id SERIAL PRIMARY KEY, value INTEGER UNIQUE)")
                .unwrap(),
            create_affected_results(0)
        );
        session
            .execute("INSERT INTO items (value) VALUES (10)")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT id, value FROM items", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(10)]]
        );
        assert_eq!(
            reader
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session
                .query("SELECT nextval('items_id_seq')", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rolls_back_dropped_tables_and_keeps_sequence_allocations() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id SERIAL PRIMARY KEY, value INTEGER UNIQUE)")
            .unwrap();
        session
            .execute("INSERT INTO items (value) VALUES (10)")
            .unwrap();

        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .query("SELECT nextval('items_id_seq')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(2)]]
        );
        session.execute("DROP TABLE items").unwrap();
        assert_eq!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        session.execute("ROLLBACK").unwrap();

        assert_eq!(
            session
                .query("SELECT id, value FROM items", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(10)]]
        );
        assert_eq!(
            session
                .query("SELECT nextval('items_id_seq')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(3)]]
        );
        assert_eq!(
            session
                .execute("INSERT INTO items (id, value) VALUES (1, 20)")
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rolls_back_ddl_after_a_later_statement_failure() {
        let db = Db::create();
        let mut session = db.create_session();

        session.execute("BEGIN").unwrap();
        session
            .execute("CREATE TABLE transient (id INTEGER)")
            .unwrap();
        assert_eq!(
            session.execute("SELECT 1 / 0").unwrap_err().sqlstate,
            SqlState::DivisionByZero
        );
        assert_eq!(
            session
                .query("SELECT * FROM transient", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InFailedSqlTransaction
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session
                .query("SELECT * FROM transient", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rolls_back_partial_multi_relation_ddl_failure() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE TABLE first (id INTEGER)").unwrap();
        session.execute("INSERT INTO first VALUES (1)").unwrap();

        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .execute("DROP TABLE first, missing")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        session.execute("ROLLBACK").unwrap();

        assert_eq!(
            session.query("SELECT * FROM first", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn serializes_concurrent_relation_creation() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut creator = db.create_session();
        let mut contender = db.create_session();
        creator.execute("BEGIN").unwrap();
        creator.execute("CREATE TABLE items (id INTEGER)").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            contender.execute("BEGIN").unwrap();
            let result = contender.execute("CREATE TABLE items (id INTEGER)");
            contender.execute("ROLLBACK").unwrap();
            result_sender.send(result).unwrap();
        });
        wait_until_relation_blocked(&db);
        creator.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .sqlstate,
            SqlState::DuplicateTable
        );
        handle.join().unwrap();
        assert!(creator.query("SELECT * FROM items", &[]).is_ok());
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn allows_concurrent_creation_after_the_first_creator_rolls_back() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut creator = db.create_session();
        let mut contender = db.create_session();
        creator.execute("BEGIN").unwrap();
        creator.execute("CREATE SEQUENCE ids START 10").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(contender.execute("CREATE SEQUENCE ids START 20"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        creator.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
        assert_eq!(
            creator.query("SELECT nextval('ids')", &[]).unwrap().rows,
            vec![vec![Value::Int8(20)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn serializes_dependency_creation_against_table_drop() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut creator = db.create_session();
        let mut dropper = db.create_session();
        creator
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        creator.execute("BEGIN").unwrap();
        creator
            .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
            .unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP TABLE parents"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        creator.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .sqlstate,
            SqlState::DependentObjectsStillExist
        );
        handle.join().unwrap();
        assert!(creator.query("SELECT * FROM parents", &[]).is_ok());
        assert!(creator.query("SELECT * FROM children", &[]).is_ok());
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn blocks_sequence_drop_while_an_explicit_transaction_uses_it() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut user = db.create_session();
        let mut dropper = db.create_session();
        user.execute("CREATE SEQUENCE ids").unwrap();
        user.execute("BEGIN").unwrap();
        assert_eq!(
            user.query("SELECT nextval('ids')", &[]).unwrap().rows,
            vec![vec![Value::Int8(1)]]
        );

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP SEQUENCE ids"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        user.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
        assert_eq!(
            user.query("SELECT nextval('ids')", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn locks_sequences_used_by_column_defaults() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut user = db.create_session();
        let mut dropper = db.create_session();
        user.execute("CREATE SEQUENCE ids").unwrap();
        user.execute("CREATE TABLE generated (id BIGINT DEFAULT nextval('ids'))")
            .unwrap();
        user.execute("BEGIN").unwrap();
        user.execute("INSERT INTO generated DEFAULT VALUES")
            .unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP SEQUENCE ids"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        user.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .sqlstate,
            SqlState::DependentObjectsStillExist
        );
        handle.join().unwrap();
        assert!(user.query("SELECT nextval('ids')", &[]).is_ok());
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn locks_sequences_while_creating_column_defaults() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut creator = db.create_session();
        let mut dropper = db.create_session();
        creator.execute("CREATE SEQUENCE ids").unwrap();
        creator.execute("BEGIN").unwrap();
        creator
            .execute("CREATE TABLE generated (id BIGINT DEFAULT nextval('ids'))")
            .unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP SEQUENCE ids"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        creator.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .sqlstate,
            SqlState::DependentObjectsStillExist
        );
        handle.join().unwrap();
        assert!(creator.query("SELECT nextval('ids')", &[]).is_ok());
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rejects_cross_session_temporary_default_dependencies() {
        let db = Db::create();
        let mut temporary_user = db.create_session();
        let mut dropper = db.create_session();
        temporary_user
            .execute("CREATE SEQUENCE public.shared_ids")
            .unwrap();

        assert_eq!(
            temporary_user
                .execute(
                    "CREATE TEMP TABLE generated \
                     (id BIGINT DEFAULT nextval('public.shared_ids'))",
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            dropper.execute("DROP SEQUENCE public.shared_ids").unwrap(),
            create_affected_results(0)
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn blocks_sequence_drop_for_late_bound_sequence_names() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut user = db.create_session();
        let mut dropper = db.create_session();
        user.execute("CREATE SEQUENCE ids").unwrap();
        user.execute("BEGIN").unwrap();
        assert_eq!(
            user.query("SELECT nextval('ids'::text)", &[]).unwrap().rows,
            vec![vec![Value::Int8(1)]]
        );

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP SEQUENCE ids"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        user.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn blocks_sequence_drop_for_parameterized_sequence_names() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut user = db.create_session();
        let mut dropper = db.create_session();
        user.execute("CREATE SEQUENCE ids").unwrap();
        let next_value = user.prepare("SELECT nextval($1)").unwrap();
        user.execute("BEGIN").unwrap();
        assert_eq!(
            user.query_prepared(&next_value, &[Value::Text("ids".into())])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP SEQUENCE ids"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        user.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn blocks_parent_drop_while_child_dml_uses_the_foreign_key() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut writer = db.create_session();
        let mut dropper = db.create_session();
        writer
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        writer
            .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
            .unwrap();
        writer.execute("INSERT INTO parents VALUES (1)").unwrap();
        writer.execute("BEGIN").unwrap();
        writer.execute("INSERT INTO children VALUES (1)").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP TABLE parents"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        writer.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .sqlstate,
            SqlState::DependentObjectsStillExist
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn blocks_child_drop_while_parent_dml_cascades_to_it() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut writer = db.create_session();
        let mut dropper = db.create_session();
        writer
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        writer
            .execute(
                "CREATE TABLE children (parent_id INTEGER REFERENCES parents ON DELETE CASCADE)",
            )
            .unwrap();
        writer.execute("INSERT INTO parents VALUES (1)").unwrap();
        writer.execute("INSERT INTO children VALUES (1)").unwrap();
        writer.execute("BEGIN").unwrap();
        writer.execute("DELETE FROM parents WHERE id = 1").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP TABLE children"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        writer.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn does_not_block_child_drop_for_parent_insert() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut writer = db.create_session();
        let mut dropper = db.create_session();
        writer
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        writer
            .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
            .unwrap();
        writer.execute("BEGIN").unwrap();
        writer.execute("INSERT INTO parents VALUES (1)").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP TABLE children"))
                .unwrap();
        });

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        writer.execute("ROLLBACK").unwrap();
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn does_not_block_child_drop_for_unrelated_parent_update() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut writer = db.create_session();
        let mut dropper = db.create_session();
        writer
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY, value INTEGER)")
            .unwrap();
        writer
            .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
            .unwrap();
        writer
            .execute("INSERT INTO parents VALUES (1, 10)")
            .unwrap();
        writer.execute("BEGIN").unwrap();
        writer
            .execute("UPDATE parents SET value = 20 WHERE id = 1")
            .unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP TABLE children"))
                .unwrap();
        });

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        writer.execute("ROLLBACK").unwrap();
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rejects_computed_sequence_names() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE SEQUENCE first_ids").unwrap();
        session.execute("CREATE SEQUENCE second_ids").unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT nextval(CASE WHEN true THEN 'first_ids' ELSE 'second_ids' END)",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        session.execute("DROP SEQUENCE second_ids").unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn lets_parent_drop_continue_after_child_drop_commits() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut child_dropper = db.create_session();
        let mut parent_dropper = db.create_session();
        child_dropper
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        child_dropper
            .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
            .unwrap();
        child_dropper.execute("BEGIN").unwrap();
        child_dropper.execute("DROP TABLE children").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(parent_dropper.execute("DROP TABLE parents"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        child_dropper.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_parent_dependency_after_child_drop_rolls_back() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut child_dropper = db.create_session();
        let mut parent_dropper = db.create_session();
        child_dropper
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        child_dropper
            .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
            .unwrap();
        child_dropper.execute("BEGIN").unwrap();
        child_dropper.execute("DROP TABLE children").unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(parent_dropper.execute("DROP TABLE parents"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        child_dropper.execute("ROLLBACK").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .sqlstate,
            SqlState::DependentObjectsStillExist
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn wakes_table_drop_after_read_only_commit() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(10))
            .build();
        let mut reader = db.create_session();
        let mut dropper = db.create_session();
        reader.execute("CREATE TABLE items (id INTEGER)").unwrap();
        reader.execute("BEGIN").unwrap();
        reader.query("SELECT * FROM items", &[]).unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(dropper.execute("DROP TABLE items"))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        reader.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn serializes_alter_table_with_active_readers() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(10))
            .build();
        let mut reader = db.create_session();
        let mut changer = db.create_session();
        reader
            .execute("CREATE TABLE alter_items (id INTEGER)")
            .unwrap();
        reader
            .execute("INSERT INTO alter_items VALUES (1)")
            .unwrap();
        reader.execute("BEGIN").unwrap();
        reader.query("SELECT * FROM alter_items", &[]).unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(changer.execute(
                    "ALTER TABLE alter_items ADD COLUMN marker INTEGER DEFAULT 7 NOT NULL",
                ))
                .unwrap();
        });
        wait_until_relation_blocked(&db);
        reader.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(0))
        );
        handle.join().unwrap();
        assert_eq!(
            reader
                .query("SELECT id, marker FROM alter_items", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(7)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn drops_foreign_key_related_tables_as_one_set_in_either_order() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        session
            .execute("CREATE TABLE children (parent_id INTEGER REFERENCES parents)")
            .unwrap();

        session.execute("BEGIN").unwrap();
        session.execute("DROP TABLE parents, children").unwrap();
        session.execute("ROLLBACK").unwrap();
        assert!(session.query("SELECT * FROM parents", &[]).is_ok());
        assert!(session.query("SELECT * FROM children", &[]).is_ok());

        session.execute("DROP TABLE children, parents").unwrap();
        assert_eq!(
            session
                .query("SELECT * FROM parents", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session
                .query("SELECT * FROM children", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn aborts_open_transactions_when_sessions_are_dropped() {
        let db = Db::create();
        let mut abandoned = db.create_session();
        abandoned.execute("BEGIN").unwrap();
        abandoned
            .execute("CREATE TABLE abandoned (id INTEGER)")
            .unwrap();
        drop(abandoned);

        let mut successor = db.create_session();
        successor
            .execute("CREATE TABLE abandoned (id INTEGER)")
            .unwrap();
        assert!(successor.query("SELECT * FROM abandoned", &[]).is_ok());
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn restores_prepared_relation_identity_after_ddl_rollback() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();
        let select = session.prepare("SELECT id FROM items").unwrap();
        let drop = session.prepare("DROP TABLE items").unwrap();

        session.execute("BEGIN").unwrap();
        session.execute("DROP TABLE items").unwrap();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (2)").unwrap();
        assert_eq!(
            session.execute_prepared(&drop, &[]).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
        session.execute("ROLLBACK").unwrap();

        assert_eq!(
            session.query_prepared(&select, &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn insert_uses_exact_literal_types_and_commits() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER, name TEXT)")
            .unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO items (name, id) VALUES ('one', 1), ('two', 2)")
                .unwrap(),
            create_affected_results(2)
        );
        let mut state = db.state.lock().unwrap();
        let reader = state.transactions.begin();
        let snapshot = Snapshot::create(&state.transactions);
        let schema = state.catalog.require_table("items").unwrap();
        let table = state.tables.get(&schema.id).unwrap();
        let rows = table
            .iterate_version_chains()
            .map(|(_, chain)| {
                find_visible_version(chain, &snapshot, reader, &state.transactions)
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

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn executes_constant_select_values_and_default_rows() {
        let db = Db::create();
        let mut session = db.create_session();

        assert_eq!(
            session
                .query("SELECT 2 + 1 AS result ORDER BY result LIMIT 1", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3)]]
        );
        let values = session
            .query(
                "VALUES (2), (1), (3) ORDER BY column1 LIMIT 1 OFFSET 1",
                &[],
            )
            .unwrap();
        assert_eq!(values.columns[0].name, "column1");
        assert_eq!(values.rows, vec![vec![Value::Int4(2)]]);
        session
            .execute("CREATE TABLE defaults (id INTEGER DEFAULT 7)")
            .unwrap();
        session
            .execute("INSERT INTO defaults DEFAULT VALUES")
            .unwrap();
        assert_eq!(
            session.query("SELECT id FROM defaults", &[]).unwrap().rows,
            vec![vec![Value::Int4(7)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn binds_single_table_aliases_and_qualified_columns() {
        let db = Db::create();
        let mut session = db.create_session();

        session
            .execute("CREATE TABLE items (id INTEGER, value TEXT)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (1, 'one')")
            .unwrap();
        let statement = session
            .prepare("SELECT item.value AS label, item.* FROM items AS item WHERE item.id = $1 ORDER BY label")
            .unwrap();
        assert_eq!(statement.get_parameter_types(), &[BaseType::Int4]);
        let result = session
            .query_prepared(&statement, &[Value::Int4(1)])
            .unwrap();
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| &column.name)
                .collect::<Vec<_>>(),
            vec!["label", "id", "value"]
        );
        assert_eq!(
            result.rows,
            vec![vec![
                Value::Text("one".into()),
                Value::Int4(1),
                Value::Text("one".into())
            ]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn applies_quoted_aliases_and_reports_scope_errors() {
        let db = Db::create();
        let mut session = db.create_session();

        session
            .execute("CREATE TABLE \"Items\" (\"Value\" VARCHAR(5), other INTEGER)")
            .unwrap();
        session
            .execute("INSERT INTO \"Items\" VALUES ('two', 2)")
            .unwrap();
        let result = session
            .query(
                "SELECT \"I\".\"V\" AS \"Result\", \"I\".* FROM \"Items\" AS \"I\"(\"V\", \"Other\") WHERE \"I\".\"V\" = 'two' ORDER BY \"Result\"",
                &[],
            )
            .unwrap();
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| &column.name)
                .collect::<Vec<_>>(),
            vec!["Result", "V", "Other"]
        );
        assert_eq!(result.columns[0].typmod, result.columns[1].typmod);
        assert_eq!(
            result.rows,
            vec![vec![
                Value::Text("two".into()),
                Value::Text("two".into()),
                Value::Int4(2)
            ]]
        );
        assert_eq!(
            session
                .query("SELECT missing FROM \"Items\"", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedColumn
        );
        assert_eq!(
            session
                .query("SELECT label FROM \"Items\" AS item(label, label)", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::AmbiguousColumn
        );
        assert_eq!(
            session
                .query("SELECT \"Items\".\"Value\" FROM \"Items\" AS item", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn joins_sources_and_merges_using_columns() {
        let db = Db::create();
        let mut session = db.create_session();

        session
            .execute(
                "CREATE TABLE left_rows (id INTEGER, left_value TEXT); \
                 CREATE TABLE right_rows (id INTEGER, right_value TEXT); \
                 INSERT INTO left_rows VALUES (1, 'one'), (2, 'two'); \
                 INSERT INTO right_rows VALUES (1, 'first'), (1, 'second'), (3, 'third')",
            )
            .unwrap();

        let cross = session
            .query(
                "SELECT left_rows.id, right_rows.id FROM left_rows, right_rows ORDER BY 1, 2",
                &[],
            )
            .unwrap();
        assert_eq!(cross.rows.len(), 6);

        let joined = session
            .query(
                "SELECT l.id, r.right_value FROM left_rows l INNER JOIN right_rows r ON l.id = r.id ORDER BY r.right_value",
                &[],
            )
            .unwrap();
        assert_eq!(
            joined.rows,
            vec![
                vec![Value::Int4(1), Value::Text("first".into())],
                vec![Value::Int4(1), Value::Text("second".into())],
            ]
        );

        let using = session
            .query(
                "SELECT * FROM (left_rows JOIN right_rows USING (id)) AS joined_rows ORDER BY id, right_value",
                &[],
            )
            .unwrap();
        assert_eq!(
            using
                .columns
                .iter()
                .map(|column| &column.name)
                .collect::<Vec<_>>(),
            vec!["id", "left_value", "right_value"]
        );
        assert_eq!(using.rows.len(), 2);
        let natural = session
            .query(
                "SELECT l.id, r.id FROM left_rows l NATURAL JOIN right_rows r ORDER BY l.id, r.id",
                &[],
            )
            .unwrap();
        assert_eq!(
            natural.rows,
            vec![
                vec![Value::Int4(1), Value::Int4(1)],
                vec![Value::Int4(1), Value::Int4(1)],
            ]
        );
        assert_eq!(
            session
                .query("SELECT id FROM left_rows CROSS JOIN right_rows", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::AmbiguousColumn
        );

        let left = session
            .query(
                "SELECT l.id, r.id FROM left_rows l LEFT JOIN right_rows r ON l.id = r.id ORDER BY l.id, r.id",
                &[],
            )
            .unwrap();
        assert_eq!(
            left.rows,
            vec![
                vec![Value::Int4(1), Value::Int4(1)],
                vec![Value::Int4(1), Value::Int4(1)],
                vec![Value::Int4(2), Value::Null],
            ]
        );
        let full = session
            .query(
                "SELECT id, left_value, right_value FROM left_rows FULL JOIN right_rows USING (id) ORDER BY id, right_value",
                &[],
            )
            .unwrap();
        assert_eq!(
            full.rows,
            vec![
                vec![
                    Value::Int4(1),
                    Value::Text("one".into()),
                    Value::Text("first".into())
                ],
                vec![
                    Value::Int4(1),
                    Value::Text("one".into()),
                    Value::Text("second".into())
                ],
                vec![Value::Int4(2), Value::Text("two".into()), Value::Null],
                vec![Value::Int4(3), Value::Null, Value::Text("third".into())],
            ]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_single_table_alias_scope_property() {
        for index in 0..32 {
            let db = Db::create();
            let mut session = db.create_session();
            let alias = format!("item_{index}");
            let output = format!("value_{index}");
            session
                .execute("CREATE TABLE items (id INTEGER, value TEXT)")
                .unwrap();
            session
                .execute(&format!(
                    "INSERT INTO items VALUES ({index}, 'value_{index}')"
                ))
                .unwrap();
            let statement = session
                .prepare(&format!(
                    "SELECT {alias}.value AS {output}, {alias}.* FROM items AS {alias} WHERE {alias}.id = $1 ORDER BY {output}"
                ))
                .unwrap();
            assert_eq!(statement.get_parameter_types(), &[BaseType::Int4]);
            let result = session
                .query_prepared(&statement, &[Value::Int4(index)])
                .unwrap();
            assert_eq!(
                result
                    .columns
                    .iter()
                    .map(|column| &column.name)
                    .collect::<Vec<_>>(),
                vec![&output, "id", "value"]
            );
            assert_eq!(
                result.rows,
                vec![vec![
                    Value::Text(format!("value_{index}")),
                    Value::Int4(index),
                    Value::Text(format!("value_{index}"))
                ]]
            );
        }
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn materializes_derived_tables_and_uncorrelated_scalar_subqueries() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER, value INTEGER)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (1, 10), (2, 20), (3, 30)")
            .unwrap();

        let derived = session
            .query(
                "SELECT source.item_id FROM (SELECT id AS item_id FROM items WHERE id > 1) AS source ORDER BY source.item_id",
                &[],
            )
            .unwrap();
        assert_eq!(
            derived.rows,
            vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT nested.item_id FROM (SELECT source.item_id FROM (SELECT id AS item_id FROM items) AS source) AS nested ORDER BY nested.item_id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)], vec![Value::Int4(3)]]
        );

        let scalar = session
            .query(
                "SELECT id FROM items WHERE value < (SELECT 25) ORDER BY (SELECT 100) - id",
                &[],
            )
            .unwrap();
        assert_eq!(
            scalar.rows,
            vec![vec![Value::Int4(2)], vec![Value::Int4(1)]]
        );

        session
            .execute("UPDATE items SET value = (SELECT 99) WHERE id = (SELECT 1)")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT value FROM items WHERE id = 1", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(99)]]
        );
        assert_eq!(
            session
                .query("SELECT (SELECT value FROM items WHERE id > 1)", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::CardinalityViolation
        );
        session.execute("ROLLBACK").unwrap();
        let prepared = session.prepare("SELECT (SELECT 7)").unwrap();
        assert_eq!(
            prepared.get_result_columns()[0].type_oid,
            BaseType::Int4.map_to_oid()
        );
        assert_eq!(
            session.query_prepared(&prepared, &[]).unwrap().rows,
            vec![vec![Value::Int4(7)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn materializes_uncorrelated_subquery_predicates() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER, pair INTEGER)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (1, 1), (2, 2), (NULL, 3)")
            .unwrap();

        let statement = session
            .prepare(
                "SELECT id FROM items WHERE id = $1 AND id IN (SELECT id FROM items) ORDER BY id",
            )
            .unwrap();
        assert_eq!(statement.get_parameter_types(), &[BaseType::Int4]);
        assert_eq!(
            session
                .query_prepared(&statement, &[Value::Int4(2)])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT EXISTS (SELECT 1 FROM items WHERE id = 1), 3 NOT IN (SELECT id FROM items), 3 > ALL (SELECT id FROM items)",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Bool(true), Value::Null, Value::Null]]
        );
        assert_eq!(
            session
                .query("SELECT (1, 1) IN (SELECT id, pair FROM items)", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Bool(true)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn executes_correlated_subqueries_with_lexical_scopes() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE parents (id INTEGER, threshold INTEGER)")
            .unwrap();
        session
            .execute("CREATE TABLE children (id INTEGER, parent_id INTEGER, value INTEGER)")
            .unwrap();
        session
            .execute("INSERT INTO parents VALUES (1, 15), (2, 5), (3, NULL)")
            .unwrap();
        session
            .execute("INSERT INTO children VALUES (10, 1, 10), (11, 1, 20), (12, 2, NULL)")
            .unwrap();

        let result = session
            .query(
                "SELECT p.id, EXISTS (SELECT 1 FROM children AS c WHERE c.parent_id = p.id AND c.value > p.threshold) AS has_match FROM parents AS p ORDER BY p.id",
                &[],
            )
            .unwrap();
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Int4(1), Value::Bool(true)],
                vec![Value::Int4(2), Value::Bool(false)],
                vec![Value::Int4(3), Value::Bool(false)],
            ]
        );

        assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p WHERE p.id IN (SELECT c.parent_id FROM children AS c WHERE c.value > p.threshold) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p WHERE p.threshold < ANY (SELECT c.value FROM children AS c WHERE c.parent_id = p.id) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        let nested = session
            .prepare(
                "SELECT p.id FROM parents AS p WHERE EXISTS (SELECT 1 FROM children AS c WHERE c.parent_id = p.id AND EXISTS (SELECT 1 WHERE c.value > p.threshold)) ORDER BY p.id",
            )
            .unwrap();
        assert_eq!(
            session.query_prepared(&nested, &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p JOIN children AS c ON c.parent_id = p.id AND EXISTS (SELECT 1 WHERE c.value > p.threshold) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p WHERE EXISTS (SELECT 1 FROM children AS p WHERE p.parent_id = p.id) ORDER BY p.id",
                    &[],
                )
                .unwrap()
                .rows,
            Vec::<Vec<Value>>::new()
        );
        assert_eq!(
            session
                .query(
                    "SELECT (SELECT c.value FROM children AS c WHERE c.parent_id = p.id) FROM parents AS p WHERE p.id = 1",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::CardinalityViolation
        );

        session
            .execute("CREATE TABLE empty_parents (id INTEGER)")
            .unwrap();
        assert_eq!(
            session
                .query(
                    "SELECT p.id FROM empty_parents AS p WHERE EXISTS (SELECT 1 WHERE missing = 1)",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedColumn
        );
        assert_eq!(
            session
                .query(
                    "SELECT p.id FROM parents AS p CROSS JOIN parents AS other WHERE EXISTS (SELECT 1 WHERE id = 1)",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::AmbiguousColumn
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn tolerates_only_planner_settings_outside_strict_mode() {
        let db = Db::create();
        let mut session = db.create_session();

        assert_eq!(
            session.execute("ANALYZE").unwrap(),
            create_affected_results(0)
        );
        assert_eq!(
            session.execute("SET enable_hashjoin = off").unwrap(),
            create_affected_results(0)
        );
        assert_eq!(
            session.execute("RESET enable_hashjoin").unwrap(),
            create_affected_results(0)
        );
        let strict = Db::create_builder().set_strict_mode_enabled(true).build();
        assert_eq!(
            strict
                .create_session()
                .execute("ANALYZE")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn materializes_non_recursive_ctes_once_with_aliases_and_empty_results() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (1), (2)")
            .unwrap();

        let result = session
            .query(
                "WITH source(value) AS (SELECT id FROM items), doubled AS (SELECT value * 2 AS value FROM source) SELECT source.value, doubled.value FROM source JOIN doubled ON doubled.value = source.value * 2 ORDER BY source.value",
                &[],
            )
            .unwrap();
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(4)],
            ]
        );

        session.execute("CREATE SEQUENCE samples").unwrap();
        let result = session
            .query(
                "WITH sampled(value) AS (SELECT nextval('samples')) SELECT left_sample.value = right_sample.value FROM sampled AS left_sample CROSS JOIN sampled AS right_sample",
                &[],
            )
            .unwrap();
        assert_eq!(result.rows, vec![vec![Value::Bool(true)]]);

        assert_eq!(
            session
                .query(
                    "WITH values_cte(value) AS (SELECT 1) SELECT (WITH values_cte(value) AS (SELECT 2) SELECT value FROM values_cte) FROM values_cte",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
        assert_eq!(
            session
                .query(
                    "WITH later_value(value) AS (SELECT value FROM first_value), first_value(value) AS (SELECT 1) SELECT value FROM later_value",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );

        let result = session
            .query(
                "WITH empty_values(value) AS (SELECT id FROM items WHERE false) SELECT value FROM empty_values",
                &[],
            )
            .unwrap();
        assert!(result.rows.is_empty());

        let statement = session
            .prepare("WITH parameterized(value) AS (SELECT $1) SELECT value FROM parameterized")
            .unwrap();
        assert_eq!(
            session
                .query_prepared(&statement, &[Value::Text("seven".into())])
                .unwrap()
                .rows,
            vec![vec![Value::Text("seven".into())]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn retains_parameters_from_unreferenced_ctes() {
        let db = Db::create();
        let mut session = db.create_session();
        let statement = session
            .prepare("WITH unused AS (SELECT $1) SELECT 1")
            .unwrap();

        assert_eq!(
            session
                .query_prepared(&statement, &[Value::Text("unused".into())])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn does_not_evaluate_unread_cte_rows() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE SEQUENCE unused_cte_sequence")
            .unwrap();
        session
            .execute("CREATE SEQUENCE limited_cte_sequence")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "WITH unused AS (SELECT nextval('unused_cte_sequence')) SELECT 1",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query("SELECT nextval('unused_cte_sequence')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );

        assert!(
            session
                .query(
                    "WITH limited(value) AS (SELECT nextval('limited_cte_sequence')) SELECT value FROM limited LIMIT 0",
                    &[],
                )
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            session
                .query("SELECT nextval('limited_cte_sequence')", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn acquires_row_locks_requested_by_ctes() {
        let db = Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
        first.execute("BEGIN").unwrap();
        first
            .query(
                "WITH locked AS (SELECT * FROM items WHERE id = 1 FOR UPDATE) SELECT * FROM locked",
                &[],
            )
            .unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.execute("UPDATE items SET value = 2 WHERE id = 1"))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();
        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            Ok(create_affected_results(1))
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn resolves_a_cte_self_name_to_an_existing_table() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (1), (2)")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "WITH items AS (SELECT * FROM items) SELECT * FROM items ORDER BY id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(2)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_quoted_cte_output_column_names() {
        let db = Db::create();
        let mut session = db.create_session();

        let result = session
            .query(
                "WITH c(\"Value\") AS (SELECT 1) SELECT \"Value\" FROM c",
                &[],
            )
            .unwrap();
        assert_eq!(result.columns[0].name, "Value");
        assert_eq!(result.rows, vec![vec![Value::Int4(1)]]);
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn executes_with_prefixed_writes() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
            .unwrap();

        assert_eq!(
            session
                .execute("WITH source AS (SELECT 1, 10) INSERT INTO items SELECT * FROM source")
                .unwrap(),
            create_affected_results(1)
        );
        assert_eq!(
            session
                .query(
                    "WITH source(value) AS (SELECT 20) UPDATE items SET value = source.value FROM source WHERE id = 1 RETURNING items.value",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(20)]]
        );
        assert_eq!(
            session
                .query(
                    "WITH source(id) AS (SELECT 1) DELETE FROM items USING source WHERE items.id = source.id RETURNING items.id",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn executes_data_modifying_ctes_once_with_statement_snapshot_visibility() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
            .unwrap();
        session.execute("INSERT INTO items VALUES (1, 10)").unwrap();

        let result = session
            .query(
                "WITH inserted AS (INSERT INTO items VALUES (2, 20) RETURNING id, value) SELECT inserted.id, inserted.value, (SELECT count(*) FROM items) FROM inserted",
                &[],
            )
            .unwrap();
        assert_eq!(
            result.rows,
            vec![vec![Value::Int4(2), Value::Int4(20), Value::Int8(1)]]
        );
        assert_eq!(
            session
                .query("SELECT id, value FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            vec![
                vec![Value::Int4(1), Value::Int4(10)],
                vec![Value::Int4(2), Value::Int4(20)],
            ]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_data_modifying_cte_snapshot_visibility_after_lock_wait() {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(2))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (1, 1)").unwrap();
        first.execute("BEGIN").unwrap();
        first
            .execute("UPDATE items SET value = 2 WHERE id = 1")
            .unwrap();

        let (result_sender, result_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            result_sender
                .send(second.query(
                    "WITH updated AS (UPDATE items SET value = value + 1 WHERE id = 1 RETURNING value) SELECT updated.value, items.value FROM updated CROSS JOIN items",
                    &[],
                ))
                .unwrap();
        });
        wait_until_blocked(&db);
        first.execute("COMMIT").unwrap();

        assert_eq!(
            result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3), Value::Int4(2)]]
        );
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn evaluates_cte_sources_required_by_mutations_with_zero_limit() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (value INTEGER)")
            .unwrap();

        assert!(
            session
                .query(
                    "WITH source(value) AS (SELECT 1), inserted AS (INSERT INTO items SELECT value FROM source RETURNING value) SELECT * FROM inserted LIMIT 0",
                    &[],
                )
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            session.query("SELECT value FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn evaluates_data_modifying_cte_defaults_once_during_lock_discovery() {
        let db = Db::create();
        let mut session = db.create_session();
        session.execute("CREATE SEQUENCE item_ids").unwrap();
        session.execute("CREATE SEQUENCE parent_ids").unwrap();
        session
            .execute("CREATE TABLE parents (id INTEGER PRIMARY KEY)")
            .unwrap();
        session.execute("INSERT INTO parents VALUES (1)").unwrap();
        session
            .execute(
                "CREATE TABLE items (id BIGINT DEFAULT nextval('item_ids'), parent_id INTEGER REFERENCES parents(id))",
            )
            .unwrap();

        assert_eq!(
            session
                .query(
                    "WITH inserted AS (INSERT INTO items (parent_id) VALUES (1) RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int8(1)]]
        );
        assert_eq!(
            session
                .query(
                    "WITH inserted AS (INSERT INTO items (parent_id) VALUES (nextval('parent_ids')) RETURNING parent_id) SELECT parent_id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn substitutes_typed_subqueries_in_data_modifying_ctes() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
            .unwrap();
        session.execute("INSERT INTO items VALUES (1, 1)").unwrap();

        assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE items SET value = (SELECT 2) RETURNING id, value) SELECT id, value FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(2)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn permits_nonrecursive_mutations_under_with_recursive() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "WITH RECURSIVE inserted AS (INSERT INTO items VALUES (1) RETURNING id) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
        assert_eq!(
            session
                .query(
                    "WITH RECURSIVE inserted AS (INSERT INTO items SELECT value FROM series WHERE value = 2 RETURNING id), series(value) AS (VALUES (1) UNION ALL SELECT value + 1 FROM series WHERE value < 2) SELECT id FROM inserted",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn composes_insert_update_and_delete_ctes_through_returning_rows() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER)")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "WITH inserted AS (INSERT INTO items VALUES (1, 10) RETURNING id, value) SELECT left_row.id, right_row.value FROM inserted AS left_row CROSS JOIN inserted AS right_row",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(10)]]
        );
        assert_eq!(
            session
                .query(
                    "WITH updated AS (UPDATE items SET value = value + 5 RETURNING id, value) SELECT id, value FROM updated",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1), Value::Int4(15)]]
        );
        assert_eq!(
            session
                .query(
                    "WITH removed AS (DELETE FROM items RETURNING id, value), copied AS (INSERT INTO items SELECT id + 1, value FROM removed RETURNING id, value) SELECT id, value FROM copied",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(2), Value::Int4(15)]]
        );

        let statement = session
            .prepare(
                "WITH inserted AS (INSERT INTO items VALUES ($1, $2) RETURNING id, value) SELECT id, value FROM inserted",
            )
            .unwrap();
        assert_eq!(
            statement.get_parameter_types(),
            &[crate::value::BaseType::Int4, crate::value::BaseType::Int4]
        );
        assert_eq!(
            session
                .query_prepared(&statement, &[Value::Int4(3), Value::Int4(30)])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(3), Value::Int4(30)]]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn executes_unreferenced_mutations_and_rolls_back_a_failing_cte_statement() {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY)")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "WITH unreferenced AS (INSERT INTO items VALUES (1)) SELECT 42",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(42)]]
        );
        assert_eq!(
            session
                .query(
                    "WITH missing_rows AS (INSERT INTO items VALUES (2)) SELECT * FROM missing_rows",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            session
                .query(
                    "WITH first_insert AS (INSERT INTO items VALUES (3) RETURNING id), failing_insert AS (INSERT INTO items VALUES (1) RETURNING id) SELECT * FROM first_insert",
                    &[],
                )
                .unwrap_err()
                .sqlstate,
            SqlState::UniqueViolation
        );
        assert_eq!(
            session
                .query("SELECT id FROM items ORDER BY id", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int4(1)]]
        );
    }
}
