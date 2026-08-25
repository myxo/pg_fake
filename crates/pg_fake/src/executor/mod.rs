use bigdecimal::ToPrimitive;
use rand_chacha::{ChaCha12Rng, rand_core::RngCore};

use crate::{
    api::{ColumnMeta, QueryResult, StatementResult},
    catalog::{
        Catalog, ColumnDef, ForeignKey, ForeignKeyAction, IdentityKind, SequenceSchema, TableId,
        TableSchema,
    },
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    storage::{RowId, Table},
    txn::{
        RowLockKey, RowLockManager, RowLockMode, Snapshot, TransactionRegistry, TransactionStatus,
        WaitForGraph, Xid, find_visible_version,
    },
    value::{BaseType, DAYS_PER_MONTH, MICROSECONDS_PER_DAY, PgType, Value},
};
use sqlparser::ast;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

mod aggregates;
mod arithmetic;
mod expressions;
mod foreign_keys;
mod locks;
mod query;
mod scope;
mod sequences;
mod writes;

use aggregates::{evaluate_aggregate_function, infer_aggregate_return_type, is_aggregate_function};
use arithmetic::{
    evaluate_boolean_operator, evaluate_distinctness, evaluate_numeric_operator,
    evaluate_temporal_arithmetic, evaluate_unary_operator, infer_interval_arithmetic_type,
};
use expressions::{
    compare_values, evaluate, evaluate_and_coerce, evaluate_assignment_expression,
    evaluate_column_default, evaluate_comparison, extract_number_literal, is_default_expression,
    resolve_operator_type, validate_check_constraint_types, validate_check_constraints,
    validate_column_default, validate_not_null,
};
pub(crate) use expressions::{
    create_constant_expression_schema, extract_unknown_string_literal, infer_expression_type,
    is_null_literal,
};
use foreign_keys::{
    apply_referencing_foreign_key_actions, convert_referential_action,
    resolve_foreign_key_column_indexes, resolve_foreign_key_name, validate_foreign_key_definitions,
    validate_row_foreign_keys,
};
pub(crate) use foreign_keys::{contains_deferred_foreign_keys, validate_deferred_foreign_keys};
pub(crate) use locks::collect_required_row_locks;
pub(crate) use scope::infer_query_output_columns;
use scope::{BoundColumn, bind_select_scope};
pub(crate) use scope::{
    BoundScope, RowScope, bind_from_scope, bind_query_scope, bind_target_scope,
    combine_bound_scopes, identify_unknown_query_columns, substitute_typed_subqueries,
};
pub(crate) use sequences::{
    SequenceExecutionContext, SequenceSessionState, SequenceSessionStorage, SequenceStorage,
    SequenceValueState,
};
use writes::{execute_delete, execute_insert, execute_update};

#[derive(Clone)]
pub(crate) struct StatementExecutionContext {
    pub(crate) transaction_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) statement_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) clock_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) rng: Arc<Mutex<ChaCha12Rng>>,
    pub(crate) sequences: SequenceExecutionContext,
}

pub(crate) struct DatabaseState {
    pub(crate) catalog: Catalog,
    pub(crate) tables: BTreeMap<TableId, Table>,
    pub(crate) transactions: TransactionRegistry,
    pub(crate) row_locks: RowLockManager,
    pub(crate) wait_for: WaitForGraph,
    pub(crate) sequence_values: SequenceStorage,
}
pub(crate) struct RequiredRowLock {
    pub(crate) key: RowLockKey,
    pub(crate) mode: RowLockMode,
}

pub(crate) use query::describe_query_result_columns;
pub(crate) use query::materialize_uncorrelated_subqueries;

impl DatabaseState {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        DatabaseState {
            catalog: Catalog::create(),
            tables: BTreeMap::new(),
            transactions: TransactionRegistry::create(),
            row_locks: RowLockManager::create(),
            wait_for: WaitForGraph::create(),
            sequence_values: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn execute_statement(
    state: &mut DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    match statement {
        ast::Statement::CreateTable(create) => {
            if create.temporary || create.on_commit.is_some() {
                return reject_unsupported("temporary tables are not implemented");
            }
            if create.query.is_some() {
                return reject_unsupported("CREATE TABLE AS is not implemented");
            }
            if create.like.is_some() || create.clone.is_some() {
                return reject_unsupported("CREATE TABLE LIKE is not implemented");
            }
            if create.inherits.is_some() {
                return reject_unsupported("table inheritance is not implemented");
            }
            if create.partition_by.is_some()
                || create.partition_of.is_some()
                || create.for_values.is_some()
            {
                return reject_unsupported("table partitioning is not implemented");
            }
            if !matches!(create.table_options, ast::CreateTableOptions::None) {
                return reject_unsupported("CREATE TABLE options are not implemented");
            }
            let table_name = normalize_unqualified_object_name(&create.name)?;
            if create.if_not_exists && state.catalog.has_relation(&table_name) {
                return Ok(StatementResult::Affected(0));
            }
            let mut columns = Vec::new();
            let mut constraints = Vec::new();
            let mut sequence_schemas = Vec::new();
            for column in &create.columns {
                let column_name = normalize_identifier(&column.name);
                let serial_type = match column.data_type.to_string().to_ascii_lowercase().as_str() {
                    "smallserial" | "serial2" => Some(BaseType::Int2),
                    "serial" | "serial4" => Some(BaseType::Int4),
                    "bigserial" | "serial8" => Some(BaseType::Int8),
                    _ => None,
                };
                let data_type = match serial_type {
                    Some(base) => PgType::create(base),
                    None => coercion::convert_ast_data_type(&column.data_type)?,
                };
                let mut nullable = true;
                let mut default = None;
                let mut default_sequence = None;
                let mut identity = None;
                for option in &column.options {
                    match &option.option {
                        ast::ColumnOption::Null => nullable = true,
                        ast::ColumnOption::NotNull => nullable = false,
                        ast::ColumnOption::Default(expr) => {
                            if serial_type.is_some() || identity.is_some() {
                                return Err(PgError::create(
                                    SqlState::SyntaxError,
                                    "multiple default values specified for column",
                                ));
                            }
                            default = Some(expr.clone());
                        }
                        ast::ColumnOption::PrimaryKey(_) => {
                            let columns = vec![column_name.clone()];
                            constraints.push(crate::catalog::Constraint::PrimaryKey(columns));
                        }
                        ast::ColumnOption::Unique(_) => {
                            let columns = vec![column_name.clone()];
                            constraints.push(crate::catalog::Constraint::Unique(columns));
                        }
                        ast::ColumnOption::Check(check) => {
                            constraints.push(crate::catalog::Constraint::Check(check.expr.clone()))
                        }
                        ast::ColumnOption::ForeignKey(foreign_key) => {
                            let name = resolve_foreign_key_name(
                                option.name.as_ref(),
                                format!("{}_{}_fkey", table_name, column_name),
                            );
                            constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                                name,
                                columns: vec![column_name.clone()],
                                foreign_table: crate::executor::normalize_unqualified_object_name(
                                    &foreign_key.foreign_table,
                                )?,
                                referred_columns: foreign_key
                                    .referred_columns
                                    .iter()
                                    .map(normalize_identifier)
                                    .collect(),
                                on_delete: convert_referential_action(foreign_key.on_delete),
                                on_update: convert_referential_action(foreign_key.on_update),
                                deferrable: foreign_key.characteristics.is_some_and(
                                    |characteristics| characteristics.deferrable.unwrap_or(false),
                                ),
                                initially_deferred: foreign_key.characteristics.is_some_and(
                                    |characteristics| {
                                        characteristics.initially
                                            == Some(ast::DeferrableInitial::Deferred)
                                    },
                                ),
                                match_kind: foreign_key.match_kind,
                            }))
                        }
                        ast::ColumnOption::Generated {
                            generated_as,
                            sequence_options,
                            generation_expr,
                            generation_expr_mode,
                            generated_keyword,
                        } => {
                            if serial_type.is_some()
                                || default.is_some()
                                || identity.is_some()
                                || generation_expr.is_some()
                                || generation_expr_mode.is_some()
                                || !generated_keyword
                            {
                                return Err(PgError::create(
                                    SqlState::SyntaxError,
                                    "invalid identity column declaration",
                                ));
                            }
                            let kind = match generated_as {
                                ast::GeneratedAs::Always => IdentityKind::Always,
                                ast::GeneratedAs::ByDefault => IdentityKind::ByDefault,
                                ast::GeneratedAs::ExpStored => {
                                    return Err(PgError::create(
                                        SqlState::SyntaxError,
                                        "invalid identity column declaration",
                                    ));
                                }
                            };
                            if !matches!(
                                data_type.base,
                                BaseType::Int2 | BaseType::Int4 | BaseType::Int8
                            ) {
                                return Err(PgError::create(
                                    SqlState::DatatypeMismatch,
                                    "identity column type must be smallint, integer, or bigint",
                                ));
                            }
                            let sequence_name = create_generated_sequence_name(
                                &state.catalog,
                                &sequence_schemas,
                                &table_name,
                                &column_name,
                            );
                            let mut sequence = sequences::create_sequence_schema_for_type(
                                sequence_name.clone(),
                                data_type.base,
                                sequence_options.as_deref().unwrap_or(&[]),
                            )?;
                            sequence.owned_by = Some((table_name.clone(), column_name.clone()));
                            sequence_schemas.push(sequence);
                            nullable = false;
                            default_sequence = Some(sequence_name);
                            identity = Some(kind);
                        }
                        option => {
                            return reject_unsupported(format!(
                                "column option is not implemented: {option}"
                            ));
                        }
                    }
                }
                if let Some(base) = serial_type {
                    let sequence_name = create_generated_sequence_name(
                        &state.catalog,
                        &sequence_schemas,
                        &table_name,
                        &column_name,
                    );
                    let mut sequence = sequences::create_sequence_schema_for_type(
                        sequence_name.clone(),
                        base,
                        &[],
                    )?;
                    sequence.owned_by = Some((table_name.clone(), column_name.clone()));
                    sequence_schemas.push(sequence);
                    nullable = false;
                    default_sequence = Some(sequence_name);
                }
                let column = ColumnDef {
                    name: column_name,
                    data_type,
                    nullable,
                    default,
                    default_sequence,
                    identity,
                };
                validate_column_default(&column)?;
                columns.push(column);
            }
            for constraint in &create.constraints {
                match constraint {
                    ast::TableConstraint::PrimaryKey(primary_key) => {
                        constraints.push(crate::catalog::Constraint::PrimaryKey(
                            primary_key
                                .columns
                                .iter()
                                .map(resolve_index_column_name)
                                .collect::<Result<Vec<_>>>()?,
                        ))
                    }
                    ast::TableConstraint::Unique(unique) => {
                        constraints.push(crate::catalog::Constraint::Unique(
                            unique
                                .columns
                                .iter()
                                .map(resolve_index_column_name)
                                .collect::<Result<Vec<_>>>()?,
                        ))
                    }
                    ast::TableConstraint::Check(check) => {
                        constraints.push(crate::catalog::Constraint::Check(check.expr.clone()))
                    }
                    ast::TableConstraint::ForeignKey(foreign_key) => {
                        let name = resolve_foreign_key_name(
                            foreign_key.name.as_ref(),
                            format!("{}_fkey", table_name),
                        );
                        constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                            name,
                            columns: foreign_key
                                .columns
                                .iter()
                                .map(normalize_identifier)
                                .collect(),
                            foreign_table: crate::executor::normalize_unqualified_object_name(
                                &foreign_key.foreign_table,
                            )?,
                            referred_columns: foreign_key
                                .referred_columns
                                .iter()
                                .map(normalize_identifier)
                                .collect(),
                            on_delete: convert_referential_action(foreign_key.on_delete),
                            on_update: convert_referential_action(foreign_key.on_update),
                            deferrable: foreign_key.characteristics.is_some_and(
                                |characteristics| characteristics.deferrable.unwrap_or(false),
                            ),
                            initially_deferred: foreign_key.characteristics.is_some_and(
                                |characteristics| {
                                    characteristics.initially
                                        == Some(ast::DeferrableInitial::Deferred)
                                },
                            ),
                            match_kind: foreign_key.match_kind,
                        }))
                    }
                    constraint => {
                        return reject_unsupported(format!(
                            "table constraint is not implemented: {constraint}"
                        ));
                    }
                }
            }
            for constraint in &constraints {
                let (constraint_columns, primary_key) = match constraint {
                    crate::catalog::Constraint::PrimaryKey(columns) => (columns, true),
                    crate::catalog::Constraint::Unique(columns) => (columns, false),
                    crate::catalog::Constraint::Check(_)
                    | crate::catalog::Constraint::ForeignKey(_) => continue,
                };
                for name in constraint_columns {
                    let column = columns
                        .iter_mut()
                        .find(|column| column.name == *name)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedColumn,
                                format!("column {name:?} does not exist"),
                            )
                        })?;
                    if primary_key {
                        column.nullable = false;
                    }
                }
            }
            validate_check_constraint_types(&TableSchema {
                id: TableId(0),
                name: table_name.clone(),
                columns: columns.clone(),
                constraints: constraints.clone(),
            })?;
            let proposed = TableSchema {
                id: TableId(0),
                name: table_name.clone(),
                columns: columns.clone(),
                constraints: constraints.clone(),
            };
            validate_foreign_key_definitions(&state.catalog, &proposed)?;
            let id = state
                .catalog
                .create_table(table_name.clone(), columns, constraints)?;
            let table = state
                .catalog
                .require_table(&table_name)
                .expect("created table must exist");
            state.tables.insert(id, Table::create(table.clone()));
            for sequence in sequence_schemas {
                let initial = SequenceValueState {
                    last_value: sequence.start_value,
                    is_called: false,
                };
                let id = state.catalog.create_sequence(sequence)?;
                state
                    .sequence_values
                    .lock()
                    .expect("sequence storage is poisoned")
                    .insert(id, initial);
            }
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::CreateSequence {
            temporary,
            if_not_exists,
            name,
            data_type,
            sequence_options,
            owned_by,
        } => {
            if *temporary {
                return reject_unsupported("temporary sequences are not implemented");
            }
            let name = normalize_unqualified_object_name(name)?;
            if *if_not_exists && state.catalog.has_relation(&name) {
                return Ok(StatementResult::Affected(0));
            }
            let owned_by = match owned_by {
                None => None,
                Some(owned_by)
                    if owned_by.0.len() == 1
                        && owned_by.0[0]
                            .as_ident()
                            .is_some_and(|name| name.value.eq_ignore_ascii_case("none")) =>
                {
                    None
                }
                Some(owned_by) if owned_by.0.len() == 2 => {
                    let Some(table) = owned_by.0[0].as_ident() else {
                        return reject_unsupported("sequence ownership is not implemented");
                    };
                    let Some(column) = owned_by.0[1].as_ident() else {
                        return reject_unsupported("sequence ownership is not implemented");
                    };
                    let table_name = normalize_identifier(table);
                    let column_name = normalize_identifier(column);
                    let table = state.catalog.require_table(&table_name)?;
                    if !table
                        .columns
                        .iter()
                        .any(|column| column.name == column_name)
                    {
                        return Err(PgError::create(
                            SqlState::UndefinedColumn,
                            format!(
                                "column {column_name:?} of relation {table_name:?} does not exist"
                            ),
                        ));
                    }
                    Some((table_name, column_name))
                }
                Some(_) => return reject_unsupported("sequence ownership is not implemented"),
            };
            let mut sequence =
                sequences::create_sequence_schema(name, data_type.as_ref(), sequence_options)?;
            sequence.owned_by = owned_by;
            let initial = SequenceValueState {
                last_value: sequence.start_value,
                is_called: false,
            };
            let id = state.catalog.create_sequence(sequence)?;
            state
                .sequence_values
                .lock()
                .expect("sequence storage is poisoned")
                .insert(id, initial);
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Table,
            names,
            if_exists,
            cascade,
            restrict,
            ..
        } => {
            if *cascade || *restrict {
                return reject_unsupported(
                    "DROP TABLE with CASCADE or RESTRICT is not implemented",
                );
            }
            let mut affected = 0;
            for object in names {
                let table_name = normalize_unqualified_object_name(object)?;
                match state.catalog.drop_table(&table_name) {
                    Ok(schema) => {
                        state.tables.remove(&schema.id);
                        for sequence in state.catalog.drop_owned_sequences(&schema.name) {
                            state
                                .sequence_values
                                .lock()
                                .expect("sequence storage is poisoned")
                                .remove(&sequence.id)
                                .expect("catalog sequence must have storage");
                        }
                        affected += 1;
                    }
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedTable => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(StatementResult::Affected(affected))
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Sequence,
            names,
            if_exists,
            cascade,
            restrict,
            ..
        } => {
            if *cascade || *restrict {
                return reject_unsupported(
                    "DROP SEQUENCE with CASCADE or RESTRICT is not implemented",
                );
            }
            for object in names {
                let name = normalize_unqualified_object_name(object)?;
                match state.catalog.drop_sequence(&name) {
                    Ok(sequence) => {
                        state
                            .sequence_values
                            .lock()
                            .expect("sequence storage is poisoned")
                            .remove(&sequence.id)
                            .expect("catalog sequence must have storage");
                    }
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedTable => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::Insert(insert) => {
            if insert.on.is_some() {
                return reject_unsupported("INSERT ON CONFLICT is not implemented");
            }
            execute_insert(
                state,
                insert,
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                context,
            )
        }
        ast::Statement::Update(update) => {
            if update.or.is_some() {
                return reject_unsupported("UPDATE feature is not implemented");
            }
            execute_update(
                state,
                &update.table,
                &update.assignments,
                update.from.as_ref(),
                update.selection.as_ref(),
                update.returning.as_deref(),
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                context,
            )
        }
        ast::Statement::Delete(delete) => execute_delete(
            state,
            delete,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
        ),
        ast::Statement::Query(query) => query::execute_query(state, query, xid, snapshot, context),
        _ => reject_unsupported("statement is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_generated_sequence_name(
    catalog: &Catalog,
    sequences: &[SequenceSchema],
    table_name: &str,
    column_name: &str,
) -> String {
    let base = format!("{table_name}_{column_name}_seq");
    let mut number = 0;
    loop {
        let name = if number == 0 {
            base.clone()
        } else {
            format!("{base}{number}")
        };
        if !catalog.has_relation(&name) && !sequences.iter().any(|sequence| sequence.name == name) {
            return name;
        }
        number += 1;
    }
}
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn normalize_unqualified_object_name(name: &ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return reject_unsupported("schemas are not implemented");
    }
    let Some(identifier) = name.0[0].as_ident() else {
        return reject_unsupported("dynamic object names are not implemented");
    };
    Ok(normalize_identifier(identifier))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn resolve_insert_table_name(table: &ast::TableObject) -> Result<String> {
    let ast::TableObject::TableName(table_name) = table else {
        return reject_unsupported("insert target is not a table");
    };
    normalize_unqualified_object_name(table_name)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn normalize_identifier(identifier: &ast::Ident) -> String {
    if identifier.quote_style.is_some() {
        identifier.value.clone()
    } else {
        identifier.value.to_ascii_lowercase()
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_index_column_name(column: &ast::IndexColumn) -> Result<String> {
    let ast::Expr::Identifier(identifier) = &column.column.expr else {
        return reject_unsupported("index expressions are not implemented");
    };
    Ok(normalize_identifier(identifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn compares_all_phase_one_value_types() {
        let pairs = [
            (Value::Bool(false), Value::Bool(true)),
            (Value::Int2(1), Value::Int2(2)),
            (Value::Int4(1), Value::Int4(2)),
            (Value::Int8(1), Value::Int8(2)),
            (Value::Float4(1.0), Value::Float4(2.0)),
            (Value::Float8(1.0), Value::Float8(2.0)),
            (
                Value::Numeric("1".parse().unwrap()),
                Value::Numeric("2".parse().unwrap()),
            ),
            (Value::Text("a".into()), Value::Text("b".into())),
            (Value::Bytea(vec![1]), Value::Bytea(vec![2])),
        ];

        for (lower, higher) in pairs {
            assert_eq!(compare_values(&lower, &higher).unwrap(), Ordering::Less);
            assert_eq!(compare_values(&higher, &lower).unwrap(), Ordering::Greater);
        }

        assert_eq!(
            compare_values(&Value::Float4(f32::NAN), &Value::Float4(1.0)).unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            compare_values(&Value::Float8(f64::NAN), &Value::Float8(1.0)).unwrap(),
            Ordering::Greater
        );
    }
}
