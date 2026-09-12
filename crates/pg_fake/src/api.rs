use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use rand_chacha::{ChaCha12Rng, rand_core::SeedableRng};
use sqlparser::ast::{self, Visit as _, VisitMut as _};

use crate::{
    analyzer,
    catalog::{
        ConstraintId, RelationName, ResolvedRelationName, SchemaId, SequenceSchema, TEMP_SCHEMA,
        TableId, TablePersistence, TableSchema, ViewDependency, ViewSchema,
    },
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{self, DatabaseState},
    parser,
    txn::{
        RelationLockAttempt, RelationLockMode, RowLockAttempt, Snapshot, TransactionStatus, Xid,
    },
    value::{BaseType, Oid, PgType, Value},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_oid: Oid,
    pub typmod: i32,
}

/// A read-only, OID-free description of the committed database catalog.
///
/// This is intended for differential conformance tests and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogInspection {
    pub tables: Vec<CatalogTableInspection>,
    pub sequences: Vec<CatalogSequenceInspection>,
    pub views: Vec<CatalogViewInspection>,
    pub functions: Vec<CatalogFunctionInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTableInspection {
    pub schema: String,
    pub name: String,
    pub columns: Vec<CatalogColumnInspection>,
    pub constraints: Vec<CatalogConstraintInspection>,
    pub indexes: Vec<CatalogIndexInspection>,
    pub triggers: Vec<CatalogTriggerInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogColumnInspection {
    pub name: String,
    pub type_name: String,
    pub typmod: i32,
    pub default: Option<String>,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogConstraintInspection {
    pub name: String,
    pub kind: String,
    pub columns: Vec<String>,
    pub referenced_relation: Option<String>,
    pub referenced_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
    pub validated: bool,
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogIndexInspection {
    pub name: String,
    pub unique: bool,
    pub keys: Vec<(String, bool)>,
    pub included_columns: Vec<String>,
    pub predicate: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTriggerInspection {
    pub name: String,
    pub timing: String,
    pub events: Vec<String>,
    pub level: String,
    pub function: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSequenceInspection {
    pub schema: String,
    pub name: String,
    pub type_name: String,
    pub increment: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub start: i64,
    pub cycle: bool,
    pub cache: i64,
    pub owner: Option<(String, String)>,
    pub last_value: i64,
    pub is_called: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogViewInspection {
    pub schema: String,
    pub name: String,
    pub columns: Vec<(String, String, i32)>,
    pub definition: String,
    pub comment: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFunctionInspection {
    pub schema: String,
    pub name: String,
    pub argument_count: usize,
    pub return_type: Option<String>,
    pub language: Option<String>,
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
    catalog_identity: crate::catalog::CatalogIdentity,
    relation_locks: Option<Vec<(String, RelationLockMode)>>,
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
    skip_function_name: bool,
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

    fn pre_visit_table_factor(
        &mut self,
        factor: &ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if matches!(factor, ast::TableFactor::Table { args: Some(_), .. }) {
            self.skip_function_name = true;
        }
        std::ops::ControlFlow::Continue(())
    }
    fn pre_visit_relation(
        &mut self,
        relation: &ast::ObjectName,
    ) -> std::ops::ControlFlow<Self::Break> {
        if std::mem::take(&mut self.skip_function_name) {
            return std::ops::ControlFlow::Continue(());
        }
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

fn collect_prepared_catalog_dependencies<'a>(
    catalog: &crate::catalog::Catalog,
    statements: impl IntoIterator<Item = &'a ast::Statement>,
) -> Result<Vec<PreparedCatalogDependency>> {
    let mut collector = PreparedDependencyCollector {
        skip_function_name: false,
        catalog,
        dependencies: Vec::new(),
        cte_scopes: Vec::new(),
        error: None,
    };
    for statement in statements {
        match statement {
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
        if let ast::Statement::Insert(insert) = statement
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

fn does_prepared_table_match(current: &TableSchema, prepared: &TableSchema) -> bool {
    current.id == prepared.id
        && current.schema_id == prepared.schema_id
        && current.name == prepared.name
        && current.columns == prepared.columns
        && current.constraints == prepared.constraints
        && current.indexes == prepared.indexes
        && current.persistence == prepared.persistence
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
    statement_timeout: Duration,
    timezone: String,
    settings_undo: Option<SessionSettings>,
    settings_on_commit: Option<SessionSettings>,
    deferred_constraints: BTreeSet<ConstraintId>,
    defer_all_constraints: bool,
    deferred_foreign_keys_dirty: bool,
    sequence_session: executor::SequenceSessionStorage,
}
#[derive(Clone)]
struct SessionSettings {
    default_isolation: IsolationLevel,
    lock_timeout: Duration,
    statement_timeout: Duration,
    timezone: String,
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
fn create_invalid_timeout_error(parameter: &str) -> PgError {
    PgError::create(
        SqlState::InvalidParameterValue,
        format!("invalid value for parameter {parameter}"),
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_timeout(expression: &ast::Expr, parameter: &str) -> Result<Duration> {
    let text = match expression {
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Number(value, _) => value.as_str(),
            ast::Value::SingleQuotedString(value) => value.trim(),
            _ => return Err(create_invalid_timeout_error(parameter)),
        },
        _ => return Err(create_invalid_timeout_error(parameter)),
    };
    let text = text.trim();
    let unsigned = text
        .strip_prefix('-')
        .or_else(|| text.strip_prefix('+'))
        .unwrap_or(text);
    let hexadecimal = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"));
    let (value, unit) = if let Some(hexadecimal) = hexadecimal {
        let digit_count = hexadecimal
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .count();
        let value_end = text.len() - hexadecimal.len() + digit_count;
        let unit = text[value_end..].trim();
        (&text[..value_end], (!unit.is_empty()).then_some(unit))
    } else if let Some(value) = text.strip_suffix("min") {
        (value, Some("min"))
    } else if let Some(value) = text.strip_suffix("ms") {
        (value, Some("ms"))
    } else if let Some(value) = text.strip_suffix("us") {
        (value, Some("us"))
    } else if let Some(value) = text.strip_suffix('s') {
        (value, Some("s"))
    } else if let Some(value) = text.strip_suffix('h') {
        (value, Some("h"))
    } else if let Some(value) = text.strip_suffix('d') {
        (value, Some("d"))
    } else {
        (text, None)
    };
    let (multiplier, next_smaller_multiplier) = match unit {
        Some("d") => (86_400_000.0, Some(3_600_000.0)),
        Some("h") => (3_600_000.0, Some(60_000.0)),
        Some("min") => (60_000.0, Some(1_000.0)),
        Some("s") => (1_000.0, Some(1.0)),
        Some("ms") => (1.0, Some(0.001)),
        Some("us") => (0.001, None),
        Some(_) => return Err(create_invalid_timeout_error(parameter)),
        None => (1.0, None),
    };
    let value = value.trim();
    let (negative, digits) = if let Some(digits) = value.strip_prefix('-') {
        (true, digits)
    } else if let Some(digits) = value.strip_prefix('+') {
        (false, digits)
    } else {
        (false, value)
    };
    let (digits, radix) = if let Some(digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (digits, 16)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (digits, 8)
    } else {
        (digits, 10)
    };
    let digit_count = digits
        .bytes()
        .take_while(|digit| char::from(*digit).to_digit(radix).is_some())
        .count();
    let retry_as_float = radix != 16
        && digits
            .as_bytes()
            .get(digit_count)
            .is_some_and(|digit| matches!(digit, b'.' | b'e' | b'E'));
    let integer = (!retry_as_float && digit_count == digits.len() && digit_count != 0)
        .then(|| u64::from_str_radix(&digits[..digit_count], radix).ok())
        .flatten()
        .map(|value| {
            if negative {
                -(value as f64)
            } else {
                value as f64
            }
        });
    let value = integer.or_else(|| retry_as_float.then(|| value.parse::<f64>().ok()).flatten());
    let milliseconds = value
        .filter(|value| value.is_finite())
        .map(|value| value * multiplier)
        .map(|value| {
            next_smaller_multiplier
                .map(|next| (value / next).round_ties_even() * next)
                .unwrap_or(value)
                .round_ties_even()
        })
        .filter(|milliseconds| *milliseconds >= 0.0 && *milliseconds <= i32::MAX as f64)
        .ok_or_else(|| create_invalid_timeout_error(parameter))?;
    Ok(Duration::from_millis(milliseconds as u64))
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
fn create_statement_timeout_error() -> PgError {
    PgError::create(
        SqlState::QueryCanceled,
        "canceling statement due to statement timeout",
    )
}

fn check_statement_timeout(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(create_statement_timeout_error())
    } else {
        Ok(())
    }
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
        ast::Statement::CreateFunction(create) => {
            let name =
                catalog.resolve_function_name(&executor::normalize_relation_name(&create.name)?)?;
            locks.insert(
                format!("function:{}:{}", name.schema_id.0, name.name),
                RelationLockMode::Exclusive,
            );
        }
        ast::Statement::DropFunction(drop) => {
            for description in &drop.func_desc {
                let name = executor::normalize_relation_name(&description.name)?;
                let resolved = catalog.resolve_function_name(&name)?;
                locks.insert(
                    format!("function:{}:{}", resolved.schema_id.0, resolved.name),
                    RelationLockMode::Exclusive,
                );
                if let Ok(function) = catalog.require_named_function(&name) {
                    for table in catalog.iterate_tables().filter(|table| {
                        table
                            .triggers
                            .iter()
                            .any(|trigger| trigger.function_id == function.id)
                    }) {
                        locks.insert(
                            ResolvedRelationName {
                                schema_id: table.schema_id,
                                name: table.name.clone(),
                            }
                            .get_lock_name(),
                            RelationLockMode::Exclusive,
                        );
                    }
                }
            }
        }
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
                            .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                            .or_insert(RelationLockMode::RowShare);
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
                        .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                        .or_insert(RelationLockMode::RowShare);
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
                &[ast::Statement::Query(create.query.clone())],
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
        ast::Statement::CreateTrigger(create) => {
            let table = catalog
                .require_named_table(&executor::normalize_relation_name(&create.table_name)?)?;
            locks.insert(
                ResolvedRelationName {
                    schema_id: table.schema_id,
                    name: table.name.clone(),
                }
                .get_lock_name(),
                RelationLockMode::Exclusive,
            );
            if let Some(body) = &create.exec_body {
                let name = executor::normalize_relation_name(&body.func_desc.name)?;
                let resolved = catalog.resolve_function_name(&name)?;
                locks.insert(
                    format!("function:{}:{}", resolved.schema_id.0, resolved.name),
                    RelationLockMode::Shared,
                );
            }
        }
        ast::Statement::DropTrigger(drop) => {
            if let Some(table_name) = &drop.table_name {
                let table_name = executor::normalize_relation_name(table_name)?;
                let table = match catalog.require_named_table(&table_name) {
                    Ok(table) => table,
                    Err(error) if drop.if_exists && error.sqlstate == SqlState::UndefinedTable => {
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
            }
        }
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

fn collect_foreign_key_relation_locks<'a>(
    state: &DatabaseState,
    statements: impl IntoIterator<Item = &'a ast::Statement>,
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
                            .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                            .or_insert(RelationLockMode::RowShare);
                    }
                }
                if let Some(ast::OnInsert::OnConflict(ast::OnConflict {
                    action: ast::OnConflictAction::DoUpdate(update),
                    ..
                })) = &insert.on
                {
                    pending.push(ForeignKeyMutation::Update {
                        table: table.id,
                        columns: if table.triggers.is_empty() {
                            collect_assignment_columns(&update.assignments)?
                        } else {
                            table
                                .columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect()
                        },
                    });
                }
            }
            ast::Statement::Update(update) => {
                let ast::TableFactor::Table { name, .. } = &update.table.relation else {
                    continue;
                };
                let name = executor::normalize_relation_name(name)?;
                if state.catalog.require_named_view(&name).is_ok() {
                    continue;
                }
                let table = state.catalog.require_named_table(&name)?;
                pending.push(ForeignKeyMutation::Update {
                    table: table.id,
                    columns: if table.triggers.is_empty() {
                        collect_assignment_columns(&update.assignments)?
                    } else {
                        table
                            .columns
                            .iter()
                            .map(|column| column.name.clone())
                            .collect()
                    },
                });
            }
            ast::Statement::Delete(delete) => {
                let ast::FromTable::WithFromKeyword(from) = &delete.from else {
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
                        .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowShare))
                        .or_insert(RelationLockMode::RowShare);
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
            let action = if updated_columns.is_some() {
                foreign_key.on_update
            } else {
                foreign_key.on_delete
            };
            if matches!(
                action,
                crate::catalog::ForeignKeyAction::Cascade
                    | crate::catalog::ForeignKeyAction::SetNull
                    | crate::catalog::ForeignKeyAction::SetDefault
            ) {
                for trigger in &child.triggers {
                    let function = state.catalog.require_function_by_id(trigger.function_id)?;
                    locks.insert(
                        format!("function:{}:{}", function.schema_id.0, function.name),
                        RelationLockMode::Shared,
                    );
                }
            }
            let mode = match action {
                crate::catalog::ForeignKeyAction::Cascade
                | crate::catalog::ForeignKeyAction::SetNull
                | crate::catalog::ForeignKeyAction::SetDefault => RelationLockMode::RowExclusive,
                crate::catalog::ForeignKeyAction::NoAction
                | crate::catalog::ForeignKeyAction::Restrict => RelationLockMode::RowShare,
            };
            locks
                .entry(
                    ResolvedRelationName {
                        schema_id: child.schema_id,
                        name: child.name.clone(),
                    }
                    .get_lock_name(),
                )
                .and_modify(|held| *held = (*held).max(mode))
                .or_insert(mode);
            match action {
                crate::catalog::ForeignKeyAction::Cascade if updated_columns.is_none() => {
                    pending.push(ForeignKeyMutation::Delete(child.id));
                }
                crate::catalog::ForeignKeyAction::Cascade
                | crate::catalog::ForeignKeyAction::SetNull
                | crate::catalog::ForeignKeyAction::SetDefault => {
                    pending.push(ForeignKeyMutation::Update {
                        table: child.id,
                        columns: if child.triggers.is_empty() {
                            foreign_key.columns
                        } else {
                            child
                                .columns
                                .iter()
                                .map(|column| column.name.clone())
                                .collect()
                        },
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
    if let ast::Statement::Lock(lock) = statement {
        if lock.nowait {
            return reject_unsupported("LOCK TABLE NOWAIT is not implemented");
        }
        let mode = match lock
            .lock_mode
            .as_ref()
            .unwrap_or(&ast::LockTableMode::AccessExclusive)
        {
            ast::LockTableMode::Exclusive => RelationLockMode::TableExclusive,
            ast::LockTableMode::AccessExclusive => RelationLockMode::Exclusive,
            _ => return reject_unsupported("LOCK TABLE mode is not implemented"),
        };
        let mut locks = Vec::new();
        let mut seen = BTreeSet::new();
        for target in &lock.tables {
            if target.only || target.has_asterisk {
                return reject_unsupported("LOCK TABLE inheritance targets are not implemented");
            }
            let name = executor::normalize_relation_name(&target.name)?;
            let table = state.catalog.require_named_table(&name)?;
            if seen.insert(table.id) {
                locks.push((
                    table.id,
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name.clone(),
                    }
                    .get_lock_name(),
                ));
            }
        }
        locks.sort_by_key(|(table_id, _)| *table_id);
        return Ok(locks.into_iter().map(|(_, name)| (name, mode)).collect());
    }
    if matches!(parser::classify(statement), parser::StatementKind::Ddl) {
        return collect_ddl_relation_locks(&state.catalog, statement);
    }
    let (expanded_statement, mutations) = executor::expand_ctes_for_analysis(statement, state)?;
    let locking_read = matches!(
        expanded_statement.as_ref(),
        ast::Statement::Query(query) if !query.locks.is_empty()
    );
    let discovered_dependencies;
    let dependencies = match prepared_dependencies {
        Some(dependencies) => dependencies,
        None => {
            discovered_dependencies = collect_prepared_catalog_dependencies(
                &state.catalog,
                std::iter::once(expanded_statement.as_ref()).chain(mutations.iter()),
            )?;
            &discovered_dependencies
        }
    };
    let mut locks = std::collections::BTreeMap::new();
    for dependency in dependencies {
        match dependency {
            PreparedCatalogDependency::Table { schema: table, .. } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name.clone(),
                    }
                    .get_lock_name(),
                    if locking_read {
                        RelationLockMode::RowShare
                    } else {
                        RelationLockMode::Shared
                    },
                );
            }
            PreparedCatalogDependency::Sequence {
                schema: sequence, ..
            } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: sequence.schema_id,
                        name: sequence.name.clone(),
                    }
                    .get_lock_name(),
                    RelationLockMode::Shared,
                );
            }
            PreparedCatalogDependency::View { schema: view, .. } => {
                locks.insert(
                    ResolvedRelationName {
                        schema_id: view.schema_id,
                        name: view.name.clone(),
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
        std::iter::once(expanded_statement.as_ref()).chain(mutations.iter()),
        &mut locks,
    )?;
    for mutation in std::iter::once(expanded_statement.as_ref()).chain(mutations.iter()) {
        let name = match mutation {
            ast::Statement::Insert(insert) => {
                Some(executor::resolve_insert_table_name(&insert.table)?)
            }
            ast::Statement::Update(update) => match &update.table.relation {
                ast::TableFactor::Table { name, .. } => {
                    Some(executor::normalize_relation_name(name)?)
                }
                _ => None,
            },
            ast::Statement::Delete(delete) => match &delete.from {
                ast::FromTable::WithFromKeyword(from) => from
                    .first()
                    .and_then(|table| {
                        let ast::TableFactor::Table { name, .. } = &table.relation else {
                            return None;
                        };
                        Some(executor::normalize_relation_name(name))
                    })
                    .transpose()?,
                _ => None,
            },
            _ => None,
        };
        let Some(name) = name else {
            continue;
        };
        if let Ok(table) = state.catalog.require_named_table(&name) {
            for trigger in &table.triggers {
                let function = state.catalog.require_function_by_id(trigger.function_id)?;
                locks.insert(
                    format!("function:{}:{}", function.schema_id.0, function.name),
                    RelationLockMode::Shared,
                );
            }
            locks
                .entry(
                    ResolvedRelationName {
                        schema_id: table.schema_id,
                        name: table.name.clone(),
                    }
                    .get_lock_name(),
                )
                .and_modify(|mode| *mode = (*mode).max(RelationLockMode::RowExclusive))
                .or_insert(RelationLockMode::RowExclusive);
        }
    }
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
    statement_deadline: Option<Instant>,
    mut state: MutexGuard<'a, DatabaseState>,
    statement: &ast::Statement,
    prepared_dependencies: Option<&[PreparedCatalogDependency]>,
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
fn acquire_row_locks<'a>(
    condvar: &Condvar,
    timeout: Duration,
    statement_deadline: Option<Instant>,
    mut state: MutexGuard<'a, DatabaseState>,
    target: RowLockTarget<'_>,
    xid: Xid,
    temporary_schema_id: SchemaId,
    isolation: IsolationLevel,
    mut snapshot: Snapshot,
    context: &executor::StatementExecutionContext,
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
            if context.take_trigger_lock_recheck() {
                continue;
            }
            if let Some(pending) = context.take_pending_cte_mutation() {
                let result = executor::execute_statement(
                    &mut state,
                    &pending.statement,
                    xid,
                    &snapshot,
                    deferred_constraints,
                    defer_all,
                    context,
                    None,
                )?;
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
            statement_timeout: Duration::ZERO,
            timezone: "UTC".into(),
            settings_undo: None,
            settings_on_commit: None,
            deferred_constraints: BTreeSet::new(),
            defer_all_constraints: false,
            deferred_foreign_keys_dirty: false,
            sequence_session: Arc::new(Mutex::new(executor::SequenceSessionState::default())),
        }
    }

    /// Inspect the committed catalog without exposing internal object identifiers.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn inspect_catalog(&self) -> CatalogInspection {
        let state = self.state.lock().expect("database mutex is poisoned");
        let catalog = &state.catalog;
        let sequence_values = state
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");

        let mut tables = catalog
            .iterate_tables()
            .map(|table| {
                let schema = catalog.get_schema_name(table.schema_id).to_owned();
                let mut constraints = table
                    .constraints
                    .iter()
                    .map(|constraint| match constraint {
                        crate::catalog::Constraint::PrimaryKey { name, columns, .. } => {
                            CatalogConstraintInspection {
                                name: name.clone(),
                                kind: "PRIMARY KEY".into(),
                                columns: columns.clone(),
                                referenced_relation: None,
                                referenced_columns: Vec::new(),
                                on_update: None,
                                on_delete: None,
                                validated: true,
                                predicate: None,
                            }
                        }
                        crate::catalog::Constraint::Unique { name, columns, .. } => {
                            CatalogConstraintInspection {
                                name: name.clone(),
                                kind: "UNIQUE".into(),
                                columns: columns.clone(),
                                referenced_relation: None,
                                referenced_columns: Vec::new(),
                                on_update: None,
                                on_delete: None,
                                validated: true,
                                predicate: None,
                            }
                        }
                        crate::catalog::Constraint::Check {
                            name,
                            expression,
                            validated,
                            ..
                        } => CatalogConstraintInspection {
                            name: name.clone(),
                            kind: "CHECK".into(),
                            columns: Vec::new(),
                            referenced_relation: None,
                            referenced_columns: Vec::new(),
                            on_update: None,
                            on_delete: None,
                            validated: *validated,
                            predicate: Some(expression.to_string()),
                        },
                        crate::catalog::Constraint::ForeignKey(foreign_key) => {
                            let foreign_table = catalog
                                .require_table_by_id(foreign_key.foreign_table_id)
                                .expect("foreign key target must remain in the catalog");
                            CatalogConstraintInspection {
                                name: foreign_key.name.clone(),
                                kind: "FOREIGN KEY".into(),
                                columns: foreign_key.columns.clone(),
                                referenced_relation: Some(format!(
                                    "{}.{}",
                                    catalog.get_schema_name(foreign_table.schema_id),
                                    foreign_table.name
                                )),
                                referenced_columns: foreign_key.referred_columns.clone(),
                                on_update: Some(format!("{:?}", foreign_key.on_update)),
                                on_delete: Some(format!("{:?}", foreign_key.on_delete)),
                                validated: foreign_key.validated,
                                predicate: None,
                            }
                        }
                    })
                    .collect::<Vec<_>>();
                constraints.sort_by(|left, right| left.name.cmp(&right.name));

                let mut indexes = table
                    .indexes
                    .iter()
                    .map(|index| CatalogIndexInspection {
                        name: index.name.clone(),
                        unique: index.unique,
                        keys: index
                            .columns
                            .iter()
                            .map(|column| (column.name.clone(), column.descending))
                            .collect(),
                        included_columns: index.include.clone(),
                        predicate: index.predicate.as_ref().map(ToString::to_string),
                    })
                    .collect::<Vec<_>>();
                indexes.sort_by(|left, right| left.name.cmp(&right.name));

                let mut triggers = table
                    .triggers
                    .iter()
                    .map(|trigger| {
                        let function = catalog
                            .require_function_by_id(trigger.function_id)
                            .expect("trigger function must remain in the catalog");
                        CatalogTriggerInspection {
                            name: trigger.name.clone(),
                            timing: trigger
                                .definition
                                .period
                                .map_or_else(String::new, |period| period.to_string()),
                            events: trigger
                                .definition
                                .events
                                .iter()
                                .map(ToString::to_string)
                                .collect(),
                            level: trigger
                                .definition
                                .trigger_object
                                .as_ref()
                                .map_or_else(String::new, ToString::to_string),
                            function: format!(
                                "{}.{}",
                                catalog.get_schema_name(function.schema_id),
                                function.name
                            ),
                        }
                    })
                    .collect::<Vec<_>>();
                triggers.sort_by(|left, right| left.name.cmp(&right.name));

                CatalogTableInspection {
                    schema,
                    name: table.name.clone(),
                    columns: table
                        .columns
                        .iter()
                        .map(|column| CatalogColumnInspection {
                            name: column.name.clone(),
                            type_name: column.data_type.base.get_postgres_name().into(),
                            typmod: column.data_type.typmod,
                            default: column.default.as_ref().map(ToString::to_string),
                            nullable: column.nullable,
                        })
                        .collect(),
                    constraints,
                    indexes,
                    triggers,
                }
            })
            .collect::<Vec<_>>();
        tables.sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        let mut sequences = catalog
            .iterate_sequences()
            .map(|sequence| {
                let value = sequence_values
                    .get(&sequence.id)
                    .expect("visible sequence must have value state");
                let owner = sequence.owned_by.as_ref().map(|(table_id, column)| {
                    let table = catalog
                        .require_table_by_id(*table_id)
                        .expect("sequence owner must remain in the catalog");
                    (
                        format!(
                            "{}.{}",
                            catalog.get_schema_name(table.schema_id),
                            table.name
                        ),
                        column.clone(),
                    )
                });
                CatalogSequenceInspection {
                    schema: catalog.get_schema_name(sequence.schema_id).to_owned(),
                    name: sequence.name.clone(),
                    type_name: sequence.data_type.get_postgres_name().into(),
                    increment: sequence.increment,
                    minimum: sequence.min_value,
                    maximum: sequence.max_value,
                    start: sequence.start_value,
                    cycle: sequence.cycle,
                    cache: sequence.cache,
                    owner,
                    last_value: value.last_value,
                    is_called: value.is_called,
                }
            })
            .collect::<Vec<_>>();
        sequences
            .sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        let mut views = catalog
            .iterate_views()
            .map(|view| {
                let mut dependencies = view
                    .dependencies
                    .iter()
                    .map(|dependency| match dependency {
                        ViewDependency::Table(id) => {
                            let table = catalog
                                .require_table_by_id(*id)
                                .expect("view table dependency must remain visible");
                            format!(
                                "table:{}.{}",
                                catalog.get_schema_name(table.schema_id),
                                table.name
                            )
                        }
                        ViewDependency::View(id) => {
                            let dependency = catalog
                                .iterate_views()
                                .find(|candidate| candidate.id == *id)
                                .expect("view dependency must remain visible");
                            format!(
                                "view:{}.{}",
                                catalog.get_schema_name(dependency.schema_id),
                                dependency.name
                            )
                        }
                        ViewDependency::Sequence(id) => {
                            let dependency = catalog
                                .iterate_sequences()
                                .find(|candidate| candidate.id == *id)
                                .expect("view sequence dependency must remain visible");
                            format!(
                                "sequence:{}.{}",
                                catalog.get_schema_name(dependency.schema_id),
                                dependency.name
                            )
                        }
                        ViewDependency::Constraint(id) => {
                            let (table, constraint) = catalog
                                .iterate_tables()
                                .find_map(|table| {
                                    table
                                        .constraints
                                        .iter()
                                        .find(|constraint| constraint.get_id() == *id)
                                        .map(|constraint| (table, constraint))
                                })
                                .expect("view constraint dependency must remain visible");
                            format!(
                                "constraint:{}.{}.{}",
                                catalog.get_schema_name(table.schema_id),
                                table.name,
                                constraint.get_name().expect("stored constraints are named")
                            )
                        }
                    })
                    .collect::<Vec<_>>();
                dependencies.sort();
                CatalogViewInspection {
                    schema: catalog.get_schema_name(view.schema_id).to_owned(),
                    name: view.name.clone(),
                    columns: view
                        .columns
                        .iter()
                        .map(|column| {
                            (
                                column.name.clone(),
                                column.data_type.base.get_postgres_name().into(),
                                column.data_type.typmod,
                            )
                        })
                        .collect(),
                    definition: view.query.to_string(),
                    comment: view.comment.clone(),
                    dependencies,
                }
            })
            .collect::<Vec<_>>();
        views.sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        let mut functions = catalog
            .iterate_functions()
            .map(|function| CatalogFunctionInspection {
                schema: catalog.get_schema_name(function.schema_id).to_owned(),
                name: function.name.clone(),
                argument_count: function.definition.args.as_ref().map_or(0, Vec::len),
                return_type: function
                    .definition
                    .return_type
                    .as_ref()
                    .map(ToString::to_string),
                language: function
                    .definition
                    .language
                    .as_ref()
                    .map(ToString::to_string),
            })
            .collect::<Vec<_>>();
        functions
            .sort_by(|left, right| (&left.schema, &left.name).cmp(&(&right.schema, &right.name)));

        CatalogInspection {
            tables,
            sequences,
            views,
            functions,
        }
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

#[derive(Clone)]
struct ProceduralLocal {
    data_type: PgType,
    value: Value,
}

#[derive(Clone, Copy)]
struct ProceduralStatementContext {
    deadline: Option<Instant>,
    statement_timestamp: chrono::DateTime<chrono::Utc>,
}

struct ProceduralLocalSubstituter<'a> {
    locals: &'a BTreeMap<String, ProceduralLocal>,
    output_aliases: Vec<BTreeSet<String>>,
    protected_order_identifiers: Vec<bool>,
    group_by_depth: usize,
    group_expression_depth: usize,
}

impl ast::VisitorMut for ProceduralLocalSubstituter<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.output_aliases.push(match query.body.as_ref() {
            ast::SetExpr::Select(select) => select
                .projection
                .iter()
                .filter_map(|item| match item {
                    ast::SelectItem::ExprWithAlias { alias, .. } => {
                        Some(executor::normalize_identifier(alias))
                    }
                    _ => None,
                })
                .collect(),
            _ => BTreeSet::new(),
        });
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.output_aliases
            .pop()
            .expect("visited query pushed output aliases");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_order_by_expr(
        &mut self,
        order_by: &mut ast::OrderByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.protected_order_identifiers.push(
            matches!(&order_by.expr, ast::Expr::Identifier(identifier)
                if self.output_aliases.last().is_some_and(|aliases| aliases.contains(&executor::normalize_identifier(identifier)))),
        );
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_order_by_expr(
        &mut self,
        _order_by: &mut ast::OrderByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.protected_order_identifiers
            .pop()
            .expect("visited ORDER BY expression pushed alias protection");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_group_by(
        &mut self,
        _group_by: &mut ast::GroupByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.group_by_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_group_by(
        &mut self,
        _group_by: &mut ast::GroupByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.group_by_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let protected_group_identifier =
            self.group_by_depth != 0 && self.group_expression_depth == 0;
        if self.group_by_depth != 0 {
            self.group_expression_depth += 1;
        }
        let ast::Expr::Identifier(identifier) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let name = executor::normalize_identifier(identifier);
        if (self.protected_order_identifiers.last() == Some(&true) || protected_group_identifier)
            && self
                .output_aliases
                .last()
                .is_some_and(|aliases| aliases.contains(&name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        let Some(local) = self.locals.get(&name) else {
            return std::ops::ControlFlow::Continue(());
        };
        *expression = analyzer::create_typed_literal(local.value.clone(), local.data_type);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_expr(
        &mut self,
        _expression: &mut ast::Expr,
    ) -> std::ops::ControlFlow<Self::Break> {
        if self.group_by_depth != 0 {
            self.group_expression_depth -= 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

fn substitute_procedural_statement_locals(
    statement: &mut ast::Statement,
    locals: &BTreeMap<String, ProceduralLocal>,
) {
    let mut substituter = ProceduralLocalSubstituter {
        locals,
        output_aliases: Vec::new(),
        protected_order_identifiers: Vec::new(),
        group_by_depth: 0,
        group_expression_depth: 0,
    };
    let _ = statement.visit(&mut substituter);
}

fn format_procedural_exception(format: &str, arguments: &[Value]) -> Result<String> {
    let mut result = String::new();
    let mut arguments = arguments.iter();
    let mut characters = format.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            result.push(character);
            continue;
        }
        if characters.clone().next() == Some('%') {
            characters.next();
            result.push('%');
            continue;
        }
        let argument = arguments.next().ok_or_else(|| {
            PgError::create(
                SqlState::SyntaxError,
                "too few parameters specified for RAISE",
            )
        })?;
        if argument.is_null() {
            result.push_str("<NULL>");
        } else {
            result.push_str(&argument.format_postgres_text());
        }
    }
    if arguments.next().is_some() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "too many parameters specified for RAISE",
        ));
    }
    Ok(result)
}

fn validate_procedural_raise_arity(statements: &[ast::PlPgSqlStatement]) -> Result<()> {
    for statement in statements {
        match statement {
            ast::PlPgSqlStatement::If {
                branches,
                else_statements,
            } => {
                for branch in branches {
                    validate_procedural_raise_arity(&branch.statements)?;
                }
                if let Some(statements) = else_statements {
                    validate_procedural_raise_arity(statements)?;
                }
            }
            ast::PlPgSqlStatement::RaiseException {
                format, arguments, ..
            } => {
                let format = format.clone().into_string().ok_or_else(|| {
                    PgError::create(SqlState::SyntaxError, "RAISE format must be a string")
                })?;
                format_procedural_exception(&format, &vec![Value::Null; arguments.len()])?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_procedural_targets(
    statements: &[ast::PlPgSqlStatement],
    locals: &BTreeSet<String>,
) -> Result<()> {
    for statement in statements {
        match statement {
            ast::PlPgSqlStatement::Assignment { target, .. } => {
                let [ast::ObjectNamePart::Identifier(identifier)] = target.0.as_slice() else {
                    return reject_unsupported("DO assignment target is not implemented");
                };
                let name = executor::normalize_identifier(identifier);
                if !locals.contains(&name) {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        format!("variable {name:?} does not exist"),
                    ));
                }
            }
            ast::PlPgSqlStatement::GetRowCount { target } => {
                let name = executor::normalize_identifier(target);
                if !locals.contains(&name) {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        format!("variable {name:?} does not exist"),
                    ));
                }
            }
            ast::PlPgSqlStatement::Sql(statement) => {
                if let ast::Statement::Query(query) = statement.as_ref()
                    && let ast::SetExpr::Select(select) = query.body.as_ref()
                    && let Some(into) = &select.into
                {
                    for target in &into.targets {
                        let ast::Expr::Identifier(identifier) = target else {
                            return reject_unsupported("SELECT INTO target is not implemented");
                        };
                        let name = executor::normalize_identifier(identifier);
                        if !locals.contains(&name) {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                format!("variable {name:?} does not exist"),
                            ));
                        }
                    }
                }
            }
            ast::PlPgSqlStatement::If {
                branches,
                else_statements,
            } => {
                for branch in branches {
                    validate_procedural_targets(&branch.statements, locals)?;
                }
                if let Some(statements) = else_statements {
                    validate_procedural_targets(statements, locals)?;
                }
            }
            ast::PlPgSqlStatement::Return(_) => {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "cannot return a value from an anonymous block",
                ));
            }
            ast::PlPgSqlStatement::RaiseException { .. } => {}
        }
    }
    Ok(())
}

fn does_procedural_query_return_rows(expression: &ast::SetExpr) -> bool {
    match expression {
        ast::SetExpr::Insert(statement)
        | ast::SetExpr::Update(statement)
        | ast::SetExpr::Delete(statement) => does_procedural_statement_return_rows(statement),
        ast::SetExpr::Query(query) => does_procedural_query_return_rows(&query.body),
        _ => true,
    }
}

fn does_procedural_statement_return_rows(statement: &ast::Statement) -> bool {
    match statement {
        ast::Statement::Query(query) => does_procedural_query_return_rows(&query.body),
        ast::Statement::Insert(insert) => insert.returning.is_some(),
        ast::Statement::Update(update) => update.returning.is_some(),
        ast::Statement::Delete(delete) => delete.returning.is_some(),
        _ => false,
    }
}

impl Session {
    fn substitute_scoped_procedural_locals(
        &self,
        statement: &mut ast::Statement,
        locals: &BTreeMap<String, ProceduralLocal>,
    ) -> Result<()> {
        let scope = executor::create_value_scope(
            locals
                .iter()
                .map(|(name, local)| (name.clone(), local.data_type)),
        );
        let row = locals
            .values()
            .map(|local| local.value.clone())
            .collect::<Vec<_>>();
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        let transaction = match self.transaction {
            Some(SessionTransactionState::Active(transaction)) => transaction,
            _ => unreachable!("procedural SQL executes in an active transaction"),
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
        executor::substitute_procedural_references(&state, statement, &scope, &row)
    }

    fn evaluate_procedural_expression(
        &mut self,
        expression: &ast::Expr,
        locals: &BTreeMap<String, ProceduralLocal>,
        procedural: ProceduralStatementContext,
    ) -> Result<Value> {
        let mut statements = parser::parse(&format!("SELECT {expression}"))?;
        let mut statement = statements
            .pop()
            .expect("generated expression query contains one statement");
        assert!(
            statements.is_empty(),
            "generated expression query is singular"
        );
        self.substitute_scoped_procedural_locals(&mut statement, locals)?;
        let StatementResult::Query(query) =
            self.execute_statement(&statement, None, None, Some(procedural))?
        else {
            unreachable!("generated expression query returns rows")
        };
        Ok(query.rows[0][0].clone())
    }

    fn coerce_procedural_expression(
        &mut self,
        expression: &ast::Expr,
        target: PgType,
        locals: &BTreeMap<String, ProceduralLocal>,
        procedural: ProceduralStatementContext,
    ) -> Result<Value> {
        if let Some(text) = executor::extract_unknown_string_literal(expression) {
            return coercion::coerce_unknown(text, target, CastContext::Assignment);
        }
        let value = self.evaluate_procedural_expression(expression, locals, procedural)?;
        let Some(source) = value.get_base_type() else {
            return Ok(Value::Null);
        };
        executor::coerce_procedural_value(value, source, target)
    }

    fn execute_procedural_sql(
        &mut self,
        statement: &ast::Statement,
        locals: &mut BTreeMap<String, ProceduralLocal>,
        row_count: &mut u64,
        procedural: ProceduralStatementContext,
    ) -> Result<()> {
        let mut statement = statement.clone();
        let into = match &mut statement {
            ast::Statement::Query(query) => match query.body.as_mut() {
                ast::SetExpr::Select(select) => select.into.take(),
                _ => None,
            },
            _ => None,
        };
        let query_with_ctes =
            matches!(&statement, ast::Statement::Query(query) if query.with.is_some());
        let uses_scoped_substitution = !query_with_ctes;
        if into.is_none() && does_procedural_statement_return_rows(&statement) {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "query has no destination for result data",
            ));
        }
        if uses_scoped_substitution {
            self.substitute_scoped_procedural_locals(&mut statement, locals)?;
        } else if query_with_ctes {
            let (mut expanded, mut mutations) = {
                let state = self.db.state.lock().expect("database mutex is poisoned");
                executor::expand_ctes_for_analysis(&statement, &state)?
            };
            self.substitute_scoped_procedural_locals(expanded.to_mut(), locals)?;
            for mutation in &mut mutations {
                self.substitute_scoped_procedural_locals(mutation, locals)?;
            }
            substitute_procedural_statement_locals(&mut statement, locals);
        }
        if into.is_some() {
            let ast::Statement::Query(query) = &mut statement else {
                unreachable!("SELECT INTO is represented by a query")
            };
            let current_limit = match &query.limit_clause {
                Some(ast::LimitClause::LimitOffset { limit, .. }) => limit.clone(),
                Some(ast::LimitClause::OffsetCommaLimit { .. }) => {
                    return reject_unsupported("SELECT INTO limit form is not implemented");
                }
                None => None,
            };
            let limit = match current_limit {
                Some(limit) => {
                    let value =
                        self.evaluate_procedural_expression(&limit, &BTreeMap::new(), procedural)?;
                    let value = match value.get_base_type() {
                        Some(source) => executor::coerce_procedural_value(
                            value,
                            source,
                            PgType::create(BaseType::Int8),
                        )?,
                        None => Value::Null,
                    };
                    match value {
                        Value::Int2(value) => Value::Int2(value.min(1)),
                        Value::Int4(value) => Value::Int4(value.min(1)),
                        Value::Int8(value) => Value::Int8(value.min(1)),
                        Value::Null => Value::Int8(1),
                        _ => {
                            return Err(PgError::create(
                                SqlState::DatatypeMismatch,
                                "LIMIT must be an integer",
                            ));
                        }
                    }
                }
                None => Value::Int8(1),
            };
            match &mut query.limit_clause {
                Some(ast::LimitClause::LimitOffset { limit: target, .. }) => {
                    *target = Some(analyzer::create_typed_literal(
                        limit.clone(),
                        PgType::create(
                            limit
                                .get_base_type()
                                .expect("SELECT INTO limit is a typed integer"),
                        ),
                    ));
                }
                None => {
                    query.limit_clause = Some(ast::LimitClause::LimitOffset {
                        limit: Some(analyzer::create_typed_literal(
                            Value::Int8(1),
                            PgType::create(BaseType::Int8),
                        )),
                        offset: None,
                        limit_by: Vec::new(),
                    });
                }
                Some(ast::LimitClause::OffsetCommaLimit { .. }) => unreachable!(),
            }
        }
        let result = self.execute_statement(&statement, None, None, Some(procedural))?;
        let Some(into) = into else {
            *row_count = match &result {
                StatementResult::Affected(affected) => *affected,
                StatementResult::Query(query) => query.rows.len() as u64,
            };
            return Ok(());
        };
        if into.temporary || into.unlogged || into.table {
            return reject_unsupported("SELECT INTO table is not implemented in PL/pgSQL");
        }
        let StatementResult::Query(query) = result else {
            unreachable!("SELECT returns a query result")
        };
        *row_count = u64::from(!query.rows.is_empty());
        for (index, target) in into.targets.iter().enumerate() {
            let ast::Expr::Identifier(identifier) = target else {
                return reject_unsupported("SELECT INTO target is not implemented");
            };
            let name = executor::normalize_identifier(identifier);
            let local = locals.get_mut(&name).ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedColumn,
                    format!("variable {name:?} does not exist"),
                )
            })?;
            let value = query
                .rows
                .first()
                .and_then(|row| row.get(index))
                .cloned()
                .unwrap_or(Value::Null);
            local.value = if value.is_null() {
                Value::Null
            } else {
                let source = BaseType::resolve_oid(
                    query
                        .columns
                        .get(index)
                        .expect("a non-NULL SELECT INTO value has column metadata")
                        .type_oid,
                )
                .expect("query results use supported PostgreSQL types");
                executor::coerce_procedural_value(value, source, local.data_type)?
            };
        }
        Ok(())
    }

    fn execute_procedural_statements(
        &mut self,
        statements: &[ast::PlPgSqlStatement],
        locals: &mut BTreeMap<String, ProceduralLocal>,
        row_count: &mut u64,
        procedural: ProceduralStatementContext,
    ) -> Result<()> {
        for statement in statements {
            match statement {
                ast::PlPgSqlStatement::Sql(statement) => {
                    self.execute_procedural_sql(statement, locals, row_count, procedural)?;
                }
                ast::PlPgSqlStatement::GetRowCount { target } => {
                    let name = executor::normalize_identifier(target);
                    let local = locals.get_mut(&name).ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("variable {name:?} does not exist"),
                        )
                    })?;
                    local.value = executor::coerce_procedural_value(
                        Value::Int8(*row_count as i64),
                        BaseType::Int8,
                        local.data_type,
                    )?;
                }
                ast::PlPgSqlStatement::If {
                    branches,
                    else_statements,
                } => {
                    let mut selected = None;
                    for branch in branches {
                        match self.evaluate_procedural_expression(
                            &branch.condition,
                            locals,
                            procedural,
                        )? {
                            Value::Bool(true) => {
                                selected = Some(branch.statements.as_slice());
                                break;
                            }
                            Value::Bool(false) | Value::Null => {}
                            _ => {
                                return Err(PgError::create(
                                    SqlState::DatatypeMismatch,
                                    "IF condition must be type boolean",
                                ));
                            }
                        }
                    }
                    if let Some(statements) = selected.or(else_statements.as_deref()) {
                        self.execute_procedural_statements(
                            statements, locals, row_count, procedural,
                        )?;
                    }
                }
                ast::PlPgSqlStatement::RaiseException {
                    format,
                    arguments,
                    hint,
                } => {
                    let format = format.clone().into_string().ok_or_else(|| {
                        PgError::create(SqlState::SyntaxError, "RAISE format must be a string")
                    })?;
                    let arguments = arguments
                        .iter()
                        .map(|argument| {
                            self.evaluate_procedural_expression(argument, locals, procedural)
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let message = format_procedural_exception(&format, &arguments)?;
                    let mut error = PgError::create(SqlState::RaiseException, message);
                    if let Some(hint) = hint {
                        let hint = self.evaluate_procedural_expression(hint, locals, procedural)?;
                        if hint.is_null() {
                            return self.abort_with_error(PgError::create(
                                SqlState::NullValueNotAllowed,
                                "RAISE statement option cannot be null",
                            ));
                        }
                        error.hint = Some(hint.format_postgres_text());
                    }
                    return self.abort_with_error(error);
                }
                ast::PlPgSqlStatement::Assignment { target, value } => {
                    let [ast::ObjectNamePart::Identifier(identifier)] = target.0.as_slice() else {
                        return reject_unsupported("DO assignment target is not implemented");
                    };
                    let name = executor::normalize_identifier(identifier);
                    let target_type = locals
                        .get(&name)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedColumn,
                                format!("variable {name:?} does not exist"),
                            )
                        })?
                        .data_type;
                    let value =
                        self.coerce_procedural_expression(value, target_type, locals, procedural)?;
                    locals.get_mut(&name).expect("required local exists").value = value;
                }
                ast::PlPgSqlStatement::Return(_) => {
                    return reject_unsupported("DO statement is not implemented");
                }
            }
        }
        Ok(())
    }

    fn execute_do(
        &mut self,
        statement: &ast::DoStatement,
        procedural: ProceduralStatementContext,
    ) -> Result<StatementResult> {
        if let Some(language) = &statement.language
            && !language.value.eq_ignore_ascii_case("plpgsql")
        {
            return self.abort_with_error(if language.value.eq_ignore_ascii_case("sql") {
                PgError::create(
                    SqlState::FeatureNotSupported,
                    "language does not support inline code execution",
                )
            } else {
                PgError::create(
                    SqlState::UndefinedObject,
                    format!("language {:?} does not exist", language.value),
                )
            });
        }
        let body = statement.body.clone().into_string().ok_or_else(|| {
            PgError::create(SqlState::SyntaxError, "DO body must be a string literal")
        })?;
        let mut parser = sqlparser::parser::Parser::new(&sqlparser::dialect::PostgreSqlDialect {})
            .try_with_sql(&body)
            .map_err(|error| PgError::create(SqlState::SyntaxError, error.to_string()))?;
        let block = parser
            .parse_plpgsql()
            .map_err(|error| PgError::create(SqlState::SyntaxError, error.to_string()))?;
        validate_procedural_raise_arity(&block.statements)?;
        let local_names = block
            .declarations
            .iter()
            .map(|declaration| executor::normalize_identifier(&declaration.name))
            .collect::<BTreeSet<_>>();
        validate_procedural_targets(&block.statements, &local_names)?;
        let mut locals = BTreeMap::new();
        for declaration in block.declarations {
            let data_type = coercion::convert_ast_data_type(&declaration.data_type)?;
            if !matches!(data_type.base, BaseType::Int8 | BaseType::Text) {
                return reject_unsupported("DO variable type is not implemented");
            }
            let name = executor::normalize_identifier(&declaration.name);
            if locals.contains_key(&name) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    format!("duplicate declaration of variable {name:?}"),
                ));
            }
            let value = match declaration.initializer {
                Some(expression) => {
                    self.coerce_procedural_expression(&expression, data_type, &locals, procedural)?
                }
                None => Value::Null,
            };
            locals.insert(name, ProceduralLocal { data_type, value });
        }
        let mut row_count = 0;
        self.execute_procedural_statements(
            &block.statements,
            &mut locals,
            &mut row_count,
            procedural,
        )?;
        Ok(StatementResult::Affected(0))
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
                self.start_transaction(self.default_isolation, true);
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
        self.prepare_with_parameter_types(sql, &[])
    }

    pub fn prepare_with_parameter_types(
        &mut self,
        sql: &str,
        parameter_types: &[Option<crate::value::BaseType>],
    ) -> Result<PreparedStatement> {
        let mut statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => {
                return self.abort_with_error(error);
            }
        };
        if statements.len() != 1 {
            return self.abort_with_error(PgError::create(
                SqlState::SyntaxError,
                "prepared statements require exactly one statement",
            ));
        }
        let mut statement = statements.pop().expect("statement count was checked");
        let parameter_count = match analyzer::count_parameters(&statement) {
            Ok(count) => count,
            Err(error) => return self.abort_with_error(error),
        };
        let _ = ast::visit_expressions_mut(&mut statement, |expression| {
            if let ast::Expr::Value(value) = expression
                && let ast::Value::Placeholder(placeholder) = &value.value
            {
                let index = analyzer::parse_placeholder_index(placeholder)
                    .expect("parameter indices were validated");
                if let Some(Some(base)) = parameter_types.get(index) {
                    *expression = analyzer::create_typed_cast(
                        expression.clone(),
                        crate::value::PgType::create(*base),
                    );
                }
            }
            std::ops::ControlFlow::<()>::Continue(())
        });
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
                    let parameter_count = parameter_count.max(parameter_types.len());
                    executor::expand_ctes_for_analysis(&statement, &state)
                        .map(|(statement, mutations)| (statement, mutations, parameter_count))
                })
                .and_then(|(statement, mutations, parameter_count)| {
                    let catalog_dependencies = collect_prepared_catalog_dependencies(
                        &state.catalog,
                        std::iter::once(statement.as_ref()).chain(mutations.iter()),
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
                        analyzer::analyze_prepared_statement_parameters(
                            &described,
                            &mutations,
                            &state.catalog,
                            parameter_count,
                            parameter_types,
                        )
                        .and_then(|(parameter_types, described)| {
                            let columns =
                                executor::describe_query_result_columns(&state, &described)?;
                            let query_plan = executor::build_prepared_query_plan(
                                &state,
                                &statement,
                                &parameter_types,
                                Some(&columns),
                            )?;
                            let relation_locks = if can_cache_read_locks(&statement)
                                && catalog_dependencies
                                    .iter()
                                    .all(|dependency| match dependency {
                                        PreparedCatalogDependency::View { schema, .. } => {
                                            can_cache_read_locks(&ast::Statement::Query(
                                                schema.query.clone(),
                                            ))
                                        }
                                        _ => true,
                                    }) {
                                collect_relation_locks(
                                    &state,
                                    &statement,
                                    Some(&catalog_dependencies),
                                )
                                .ok()
                            } else {
                                None
                            };
                            Ok((
                                parameter_types,
                                columns,
                                query_plan,
                                catalog_dependencies,
                                relation_locks,
                                state.catalog.create_identity(),
                            ))
                        })
                    },
                )
        };
        match prepared {
            Ok((
                parameter_types,
                columns,
                query_plan,
                catalog_dependencies,
                relation_locks,
                catalog_identity,
            )) => Ok(PreparedStatement {
                statement,
                parameter_types,
                columns,
                query_plan,
                catalog_dependencies,
                relation_locks,
                catalog_identity,
            }),
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
                if statement.relation_locks.is_some() {
                    None
                } else {
                    Some(
                        match analyzer::bind_parameters(
                            &statement.statement,
                            &statement.parameter_types,
                            params,
                        ) {
                            Ok(statement) => statement,
                            Err(error) => return self.abort_with_error(error),
                        },
                    )
                },
                Some((
                    query_plan,
                    parameters.as_slice(),
                    statement.columns.as_slice(),
                )),
            )
        } else if statement.parameter_types.is_empty() && params.is_empty() {
            (None, None)
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
        let execution_statement = bound_statement.as_deref().unwrap_or(&statement.statement);
        let started_implicit_transaction = self.transaction.is_none();
        if started_implicit_transaction {
            self.start_transaction(self.default_isolation, true);
        }
        match self.execute_statement(execution_statement, prepared_query, Some(statement), None) {
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
    fn capture_settings(&self) -> SessionSettings {
        SessionSettings {
            default_isolation: self.default_isolation,
            lock_timeout: self.lock_timeout,
            statement_timeout: self.statement_timeout,
            timezone: self.timezone.clone(),
        }
    }
    fn restore_settings(&mut self, settings: SessionSettings) {
        self.default_isolation = settings.default_isolation;
        self.lock_timeout = settings.lock_timeout;
        self.statement_timeout = settings.statement_timeout;
        self.timezone = settings.timezone;
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn start_transaction(&mut self, isolation: IsolationLevel, implicit_batch: bool) {
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
        prepared_statement: Option<&PreparedStatement>,
        procedural: Option<ProceduralStatementContext>,
    ) -> Result<StatementResult> {
        let prepared_dependencies =
            prepared_statement.map(|statement| statement.catalog_dependencies.as_slice());
        match statement {
            ast::Statement::Analyze(_) if !self.db.strict => {
                return Ok(StatementResult::Affected(0));
            }
            ast::Statement::Reset(reset)
                if !self.db.strict && is_tolerated_planner_reset(&reset.reset) =>
            {
                return Ok(StatementResult::Affected(0));
            }
            ast::Statement::Set(ast::Set::SetTimeZone { local, value }) => {
                self.timezone = parse_timezone(value)?;
                if !local {
                    self.settings_on_commit
                        .as_mut()
                        .expect("SET runs in a transaction")
                        .timezone = self.timezone.clone();
                }
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
                    self.settings_on_commit
                        .as_mut()
                        .expect("SET runs in a transaction")
                        .default_isolation = isolation;
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
                    if *scope != Some(ast::ContextModifier::Local) {
                        self.settings_on_commit
                            .as_mut()
                            .expect("SET runs in a transaction")
                            .timezone = self.timezone.clone();
                    }
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
                    if *hivevar || values.len() != 1 {
                        return self.abort_with_error(PgError::create(
                            SqlState::FeatureNotSupported,
                            "lock_timeout setting variant is not implemented",
                        ));
                    }
                    self.lock_timeout = match parse_timeout(&values[0], "lock_timeout") {
                        Ok(timeout) => timeout,
                        Err(error) => return self.abort_with_error(error),
                    };
                    if *scope != Some(ast::ContextModifier::Local) {
                        self.settings_on_commit
                            .as_mut()
                            .expect("SET runs in a transaction")
                            .lock_timeout = self.lock_timeout;
                    }
                    return Ok(StatementResult::Affected(0));
                }
                if variable
                    .to_string()
                    .eq_ignore_ascii_case("statement_timeout")
                {
                    if matches!(
                        self.transaction,
                        Some(SessionTransactionState::Aborted { .. })
                    ) {
                        return Err(PgError::create(
                            SqlState::InFailedSqlTransaction,
                            "current transaction is aborted",
                        ));
                    }
                    if *scope != Some(ast::ContextModifier::Local) || *hivevar || values.len() != 1
                    {
                        return self.abort_with_error(PgError::create(
                            SqlState::FeatureNotSupported,
                            "statement_timeout setting variant is not implemented",
                        ));
                    }
                    self.statement_timeout = match parse_timeout(&values[0], "statement_timeout") {
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
        if let ast::Statement::Do(statement) = statement {
            let procedural = procedural.unwrap_or_else(|| ProceduralStatementContext {
                deadline: (self.statement_timeout != Duration::ZERO)
                    .then(|| Instant::now() + self.statement_timeout),
                statement_timestamp: self.db.read_clock(),
            });
            return match self.execute_do(statement, procedural) {
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
                (self.statement_timeout != Duration::ZERO)
                    .then(|| Instant::now() + self.statement_timeout)
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
        let acquired = match acquire_relation_locks(
            &condvar,
            self.lock_timeout,
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
        let prepared_dependencies = prepared_statement
            .filter(|statement| !state.catalog.matches_identity(&statement.catalog_identity))
            .map(|statement| statement.catalog_dependencies.as_slice());
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
                        Ok(table) if does_prepared_table_match(table, schema) => None,
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
            statement_timestamp: statement_timestamp.expect("fallback captures statement time"),
            clock_timestamp: self.db.read_clock(),
            deadline: statement_deadline,
            rng: self.db.rng.clone(),
            sequences,
            source_state: contains_triggered_insert(&state, statement)
                .then(|| Arc::new(state.clone())),
            source_snapshot: snapshot,
            prepared_trigger_inserts: Arc::new(Mutex::new(Default::default())),
            prepared_trigger_updates: Arc::new(Mutex::new(Default::default())),
            prepared_mutation_targets: Arc::new(Mutex::new(Default::default())),
            prepared_cte_results: Arc::new(Mutex::new(Vec::new())),
            executed_ctes: Arc::new(Mutex::new(Vec::new())),
            pending_cte_mutations: Arc::new(Mutex::new(Vec::new())),
            prepared_subquery_results: Arc::new(Mutex::new(Default::default())),
            prepares_subquery_results: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            trigger_lock_recheck: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            trigger_lock_recheck_locks: Arc::new(Mutex::new(Vec::new())),
        };
        let (contains_cte, contains_subquery) = executor::detect_statement_features(statement);
        let mut acquired_row_locks = false;
        let cte_statement = if contains_cte {
            let (acquired_state, acquired_snapshot, locked_rows) = match acquire_row_locks(
                &condvar,
                self.lock_timeout,
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
        let result = executor::execute_statement(
            &mut state,
            statement,
            transaction.xid,
            &snapshot,
            &self.deferred_constraints,
            self.defer_all_constraints,
            &context,
            mutation_targets,
        )
        .and_then(|result| {
            context.check_timeout()?;
            Ok(result)
        });
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
fn can_cache_read_locks(statement: &ast::Statement) -> bool {
    let only_queries = ast::visit_statements(statement, |statement| {
        if matches!(statement, ast::Statement::Query(_)) {
            std::ops::ControlFlow::Continue(())
        } else {
            std::ops::ControlFlow::Break(())
        }
    })
    .is_continue();
    only_queries && !contains_sequence_function(statement)
}

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
#[path = "api_test.rs"]
mod tests;
