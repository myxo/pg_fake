use bigdecimal::ToPrimitive;
use rand_chacha::{ChaCha12Rng, rand_core::RngCore};

use crate::{
    api::{ColumnMeta, QueryResult, StatementResult},
    catalog::{Catalog, ColumnDef, ForeignKey, ForeignKeyAction, TableId, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    storage::{RowId, Table},
    txn::{
        RowLockKey, RowLockManager, RowLockMode, Snapshot, TransactionManager, TransactionStatus,
        WaitForGraph, Xid, visible_version,
    },
    value::{BaseType, PgType, Value},
};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, CastKind, ColumnOption, ConstraintReferenceMatchKind,
    DateTimeField, Delete, Expr, FromTable, Function, FunctionArg, FunctionArgExpr,
    FunctionArguments, GroupByExpr, Ident, IndexColumn, LockType, ObjectType, ReferentialAction,
    SelectItem, SelectItemQualifiedWildcardKind, SetExpr, Statement, TableConstraint, TableFactor,
    TableObject, TableWithJoins, UnaryOperator, Value as AstValue,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

mod arithmetic;
mod expressions;
mod foreign_keys;
mod locks;
mod query;
mod scope;
mod writes;

use arithmetic::{
    arithmetic, boolean_binary, distinct, interval_arithmetic_type, temporal_arithmetic, unary,
};
use expressions::{
    column_default, comparison, default_expression, evaluate, evaluate_as, expression_value,
    number_literal, unknown_string, validate_check_constraint_types, validate_check_constraints,
    validate_not_null, value_ordering,
};
pub(crate) use expressions::{constant_schema, expression_type, null_expression};
use foreign_keys::{
    apply_parent_actions, foreign_key_action, foreign_key_column_indexes, foreign_key_name,
    validate_foreign_key_definitions, validate_row_foreign_keys,
};
pub(crate) use foreign_keys::{contains_deferred_foreign_keys, validate_deferred_foreign_keys};
pub(crate) use locks::required_row_locks;
pub(crate) use scope::query_output_columns;
use scope::{BoundColumn, bind_select_scope};
pub(crate) use scope::{BoundScope, RowScope, bind_query_scope, describe_expression_subqueries};
use writes::{delete_rows, insert_rows, update_rows};

#[derive(Clone)]
pub(crate) struct ExecutionContext {
    pub(crate) transaction_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) statement_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) clock_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) rng: Arc<Mutex<ChaCha12Rng>>,
}

pub(crate) struct DatabaseState {
    pub(crate) catalog: Catalog,
    pub(crate) tables: BTreeMap<TableId, Table>,
    pub(crate) transactions: TransactionManager,
    pub(crate) row_locks: RowLockManager,
    pub(crate) wait_for: WaitForGraph,
}
pub(crate) struct RequiredRowLock {
    pub(crate) key: RowLockKey,
    pub(crate) mode: RowLockMode,
}

pub(crate) use query::materialize_scalar_subqueries;
pub(crate) use query::query_columns;

impl DatabaseState {
    pub(crate) fn new() -> Self {
        DatabaseState {
            catalog: Catalog::new(),
            tables: BTreeMap::new(),
            transactions: TransactionManager::new(),
            row_locks: RowLockManager::new(),
            wait_for: WaitForGraph::new(),
        }
    }
}

pub(crate) fn dispatch(
    state: &mut DatabaseState,
    statement: &Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    context: &ExecutionContext,
) -> Result<StatementResult> {
    let statement = query::materialize_scalar_subqueries(state, statement, xid, snapshot, context)?;
    match &statement {
        Statement::CreateTable(create) => {
            let table_name = name(&create.name)?;
            if create.if_not_exists && state.catalog.table(&table_name).is_ok() {
                return Ok(StatementResult::Affected(0));
            }
            let mut columns = Vec::new();
            let mut constraints = Vec::new();
            for column in &create.columns {
                let data_type = coercion::type_from_ast(&column.data_type)?;
                let mut nullable = true;
                let mut default = None;
                for option in &column.options {
                    match &option.option {
                        ColumnOption::Null => nullable = true,
                        ColumnOption::NotNull => nullable = false,
                        ColumnOption::Default(expr) => default = Some(expr.clone()),
                        ColumnOption::PrimaryKey(_) => {
                            let columns = vec![identifier_name(&column.name)];
                            constraints.push(crate::catalog::Constraint::PrimaryKey(columns));
                        }
                        ColumnOption::Unique(_) => {
                            let columns = vec![identifier_name(&column.name)];
                            constraints.push(crate::catalog::Constraint::Unique(columns));
                        }
                        ColumnOption::Check(check) => {
                            constraints.push(crate::catalog::Constraint::Check(check.expr.clone()))
                        }
                        ColumnOption::ForeignKey(foreign_key) => {
                            let name = foreign_key_name(
                                option.name.as_ref(),
                                format!("{}_{}_fkey", table_name, identifier_name(&column.name)),
                            );
                            constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                                name,
                                columns: vec![identifier_name(&column.name)],
                                foreign_table: crate::executor::name(&foreign_key.foreign_table)?,
                                referred_columns: foreign_key
                                    .referred_columns
                                    .iter()
                                    .map(identifier_name)
                                    .collect(),
                                on_delete: foreign_key_action(foreign_key.on_delete),
                                on_update: foreign_key_action(foreign_key.on_update),
                                deferrable: foreign_key.characteristics.is_some_and(
                                    |characteristics| characteristics.deferrable.unwrap_or(false),
                                ),
                                initially_deferred: foreign_key.characteristics.is_some_and(
                                    |characteristics| {
                                        characteristics.initially
                                            == Some(sqlparser::ast::DeferrableInitial::Deferred)
                                    },
                                ),
                                match_kind: foreign_key.match_kind,
                            }))
                        }
                        option => {
                            return Err(PgError::new(
                                SqlState::FeatureNotSupported,
                                format!("column option is not implemented: {option}"),
                            ));
                        }
                    }
                }
                let column = ColumnDef {
                    name: identifier_name(&column.name),
                    data_type,
                    nullable,
                    default,
                };
                if column.default.is_some() {
                    column_default(&column, context)?;
                }
                columns.push(column);
            }
            for constraint in &create.constraints {
                match constraint {
                    TableConstraint::PrimaryKey(primary_key) => {
                        constraints.push(crate::catalog::Constraint::PrimaryKey(
                            primary_key
                                .columns
                                .iter()
                                .map(index_column_name)
                                .collect::<Result<Vec<_>>>()?,
                        ))
                    }
                    TableConstraint::Unique(unique) => {
                        constraints.push(crate::catalog::Constraint::Unique(
                            unique
                                .columns
                                .iter()
                                .map(index_column_name)
                                .collect::<Result<Vec<_>>>()?,
                        ))
                    }
                    TableConstraint::Check(check) => {
                        constraints.push(crate::catalog::Constraint::Check(check.expr.clone()))
                    }
                    TableConstraint::ForeignKey(foreign_key) => {
                        let name = foreign_key_name(
                            foreign_key.name.as_ref(),
                            format!("{}_fkey", table_name),
                        );
                        constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                            name,
                            columns: foreign_key.columns.iter().map(identifier_name).collect(),
                            foreign_table: crate::executor::name(&foreign_key.foreign_table)?,
                            referred_columns: foreign_key
                                .referred_columns
                                .iter()
                                .map(identifier_name)
                                .collect(),
                            on_delete: foreign_key_action(foreign_key.on_delete),
                            on_update: foreign_key_action(foreign_key.on_update),
                            deferrable: foreign_key.characteristics.is_some_and(
                                |characteristics| characteristics.deferrable.unwrap_or(false),
                            ),
                            initially_deferred: foreign_key.characteristics.is_some_and(
                                |characteristics| {
                                    characteristics.initially
                                        == Some(sqlparser::ast::DeferrableInitial::Deferred)
                                },
                            ),
                            match_kind: foreign_key.match_kind,
                        }))
                    }
                    constraint => {
                        return Err(PgError::new(
                            SqlState::FeatureNotSupported,
                            format!("table constraint is not implemented: {constraint}"),
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
                            PgError::new(
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
                .table(&table_name)
                .expect("created table must exist");
            state.tables.insert(id, Table::new(table.clone()));
            Ok(StatementResult::Affected(0))
        }
        Statement::Drop {
            object_type: ObjectType::Table,
            names,
            if_exists,
            ..
        } => {
            let mut affected = 0;
            for object in names {
                let table_name = name(object)?;
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
        Statement::Insert(insert) => insert_rows(
            state,
            insert,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
        ),
        Statement::Update(update) => {
            if update.from.is_some() || update.returning.is_some() || update.or.is_some() {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "UPDATE feature is not implemented",
                ));
            }
            update_rows(
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
        Statement::Delete(delete) => delete_rows(
            state,
            delete,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
        ),
        Statement::Query(query) => query::select_rows(state, query, xid, snapshot, context),
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "statement is not implemented",
        )),
    }
}
pub(crate) fn name(name: &sqlparser::ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "schemas are not implemented",
        ));
    }
    let Some(identifier) = name.0[0].as_ident() else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "dynamic object names are not implemented",
        ));
    };
    Ok(identifier_name(identifier))
}

pub(crate) fn insert_table_name(table: &TableObject) -> Result<String> {
    let TableObject::TableName(table_name) = table else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "insert target is not a table",
        ));
    };
    name(table_name)
}

pub(crate) fn identifier_name(identifier: &Ident) -> String {
    if identifier.quote_style.is_some() {
        identifier.value.clone()
    } else {
        identifier.value.to_ascii_lowercase()
    }
}

fn index_column_name(column: &IndexColumn) -> Result<String> {
    let Expr::Identifier(identifier) = &column.column.expr else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "index expressions are not implemented",
        ));
    };
    Ok(identifier_name(identifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_ordering_covers_all_phase_one_types() {
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
            assert_eq!(value_ordering(&lower, &higher).unwrap(), Ordering::Less);
            assert_eq!(value_ordering(&higher, &lower).unwrap(), Ordering::Greater);
        }

        assert_eq!(
            value_ordering(&Value::Float4(f32::NAN), &Value::Float4(1.0)).unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            value_ordering(&Value::Float8(f64::NAN), &Value::Float8(1.0)).unwrap(),
            Ordering::Greater
        );
    }
}
