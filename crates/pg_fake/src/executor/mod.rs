use bigdecimal::ToPrimitive;
use rand_chacha::{ChaCha12Rng, rand_core::RngCore};

use crate::{
    api::{ColumnMeta, QueryResult, StatementResult},
    catalog::{Catalog, ColumnDef, ForeignKey, ForeignKeyAction, TableId, TableSchema},
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
mod writes;

use aggregates::{evaluate_aggregate_function, infer_aggregate_return_type, is_aggregate_function};
use arithmetic::{
    evaluate_boolean_operator, evaluate_distinctness, evaluate_numeric_operator,
    evaluate_temporal_arithmetic, evaluate_unary_operator, infer_interval_arithmetic_type,
};
use expressions::{
    compare_values, evaluate, evaluate_and_coerce, evaluate_assignment_expression,
    evaluate_column_default, evaluate_comparison, extract_number_literal,
    extract_unknown_string_literal, is_default_expression, validate_check_constraint_types,
    validate_check_constraints, validate_not_null,
};
pub(crate) use expressions::{
    create_constant_expression_schema, infer_expression_type, is_null_literal,
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
pub(crate) use scope::{BoundScope, RowScope, bind_query_scope, substitute_typed_subqueries};
use writes::{execute_delete, execute_insert, execute_update};

#[derive(Clone)]
pub(crate) struct StatementExecutionContext {
    pub(crate) transaction_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) statement_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) clock_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) rng: Arc<Mutex<ChaCha12Rng>>,
}

pub(crate) struct DatabaseState {
    pub(crate) catalog: Catalog,
    pub(crate) tables: BTreeMap<TableId, Table>,
    pub(crate) transactions: TransactionRegistry,
    pub(crate) row_locks: RowLockManager,
    pub(crate) wait_for: WaitForGraph,
}
pub(crate) struct RequiredRowLock {
    pub(crate) key: RowLockKey,
    pub(crate) mode: RowLockMode,
}

pub(crate) use query::describe_query_result_columns;
pub(crate) use query::materialize_uncorrelated_subqueries;

impl DatabaseState {
    pub(crate) fn create() -> Self {
        DatabaseState {
            catalog: Catalog::create(),
            tables: BTreeMap::new(),
            transactions: TransactionRegistry::create(),
            row_locks: RowLockManager::create(),
            wait_for: WaitForGraph::create(),
        }
    }
}

pub(crate) fn execute_statement(
    state: &mut DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    let statement =
        query::materialize_uncorrelated_subqueries(state, statement, xid, snapshot, context)?;
    match &statement {
        ast::Statement::CreateTable(create) => {
            let table_name = normalize_unqualified_object_name(&create.name)?;
            if create.if_not_exists && state.catalog.require_table(&table_name).is_ok() {
                return Ok(StatementResult::Affected(0));
            }
            let mut columns = Vec::new();
            let mut constraints = Vec::new();
            for column in &create.columns {
                let data_type = coercion::convert_ast_data_type(&column.data_type)?;
                let mut nullable = true;
                let mut default = None;
                for option in &column.options {
                    match &option.option {
                        ast::ColumnOption::Null => nullable = true,
                        ast::ColumnOption::NotNull => nullable = false,
                        ast::ColumnOption::Default(expr) => default = Some(expr.clone()),
                        ast::ColumnOption::PrimaryKey(_) => {
                            let columns = vec![normalize_identifier(&column.name)];
                            constraints.push(crate::catalog::Constraint::PrimaryKey(columns));
                        }
                        ast::ColumnOption::Unique(_) => {
                            let columns = vec![normalize_identifier(&column.name)];
                            constraints.push(crate::catalog::Constraint::Unique(columns));
                        }
                        ast::ColumnOption::Check(check) => {
                            constraints.push(crate::catalog::Constraint::Check(check.expr.clone()))
                        }
                        ast::ColumnOption::ForeignKey(foreign_key) => {
                            let name = resolve_foreign_key_name(
                                option.name.as_ref(),
                                format!(
                                    "{}_{}_fkey",
                                    table_name,
                                    normalize_identifier(&column.name)
                                ),
                            );
                            constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                                name,
                                columns: vec![normalize_identifier(&column.name)],
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
                        option => {
                            return reject_unsupported(format!(
                                "column option is not implemented: {option}"
                            ));
                        }
                    }
                }
                let column = ColumnDef {
                    name: normalize_identifier(&column.name),
                    data_type,
                    nullable,
                    default,
                };
                if column.default.is_some() {
                    evaluate_column_default(&column, context)?;
                }
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
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Table,
            names,
            if_exists,
            ..
        } => {
            let mut affected = 0;
            for object in names {
                let table_name = normalize_unqualified_object_name(object)?;
                match state.catalog.drop_table(&table_name) {
                    Ok(schema) => {
                        state.tables.remove(&schema.id);
                        affected += 1;
                    }
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedTable => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(StatementResult::Affected(affected))
        }
        ast::Statement::Insert(insert) => execute_insert(
            state,
            insert,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
        ),
        ast::Statement::Update(update) => {
            if update.from.is_some() || update.returning.is_some() || update.or.is_some() {
                return reject_unsupported("UPDATE feature is not implemented");
            }
            execute_update(
                state,
                &update.table,
                &update.assignments,
                update.selection.as_ref(),
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
pub(crate) fn normalize_unqualified_object_name(name: &ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return reject_unsupported("schemas are not implemented");
    }
    let Some(identifier) = name.0[0].as_ident() else {
        return reject_unsupported("dynamic object names are not implemented");
    };
    Ok(normalize_identifier(identifier))
}

pub(crate) fn resolve_insert_table_name(table: &ast::TableObject) -> Result<String> {
    let ast::TableObject::TableName(table_name) = table else {
        return reject_unsupported("insert target is not a table");
    };
    normalize_unqualified_object_name(table_name)
}

pub(crate) fn normalize_identifier(identifier: &ast::Ident) -> String {
    if identifier.quote_style.is_some() {
        identifier.value.clone()
    } else {
        identifier.value.to_ascii_lowercase()
    }
}

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
