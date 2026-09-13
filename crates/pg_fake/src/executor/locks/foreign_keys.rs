use super::RequiredRowLock;
use crate::executor::{
    DatabaseState, StatementContext,
    column_defaults::is_default_expression,
    expressions::{create_constant_expression_schema, evaluate_assignment_expression},
    foreign_keys::resolve_foreign_key_column_indexes,
    normalize_unqualified_object_name, resolve_insert_table_name,
};
use crate::{
    catalog::TableSchema,
    error::{PgError, Result, SqlState},
    txn::{RowLockKey, RowLockMode, Snapshot, Xid, find_visible_version},
    value::Value,
};
use sqlparser::ast;

pub(in crate::executor) fn collect_foreign_key_locks_for_rows<'a>(
    state: &DatabaseState,
    schema: &TableSchema,
    rows: impl Iterator<Item = &'a Vec<Value>>,
    xid: Xid,
) -> Result<Vec<RequiredRowLock>> {
    let rows = rows.collect::<Vec<_>>();
    let mut locks = Vec::new();
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
            continue;
        };
        let local = resolve_foreign_key_column_indexes(schema, &foreign_key.columns)?;
        let foreign_schema = state
            .catalog
            .require_table_by_id(foreign_key.foreign_table_id)?;
        let referred = if foreign_key.referred_columns.is_empty() {
            foreign_schema
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns.clone()),
                    _ => None,
                })
                .expect("foreign key definition was validated")
        } else {
            foreign_key.referred_columns.clone()
        };
        let referred = resolve_foreign_key_column_indexes(foreign_schema, &referred)?;
        let table = state
            .tables
            .get(&foreign_schema.id)
            .expect("catalog table must have storage");
        for row in &rows {
            let key = local
                .iter()
                .map(|index| row[*index].clone())
                .collect::<Vec<_>>();
            if key.iter().any(Value::is_null) {
                continue;
            }
            if let Some(row_id) =
                table.find_unique_candidate_row(&referred, &key, xid, &state.transactions)
            {
                locks.push(RequiredRowLock {
                    key: RowLockKey {
                        table_id: foreign_schema.id,
                        row_id,
                    },
                    mode: RowLockMode::KeyShare,
                    mutation_candidate: None,
                });
            }
        }
    }
    Ok(locks)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_insert_foreign_key_locks(
    state: &DatabaseState,
    insert: &ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    let schema = state
        .catalog
        .require_named_table(&resolve_insert_table_name(&insert.table)?)?;
    if !schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, crate::catalog::Constraint::ForeignKey(_)))
    {
        return Ok(Vec::new());
    }
    if !schema.triggers.is_empty() {
        let mut locks = Vec::new();
        for constraint in &schema.constraints {
            let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
                continue;
            };
            let foreign_schema = state
                .catalog
                .require_table_by_id(foreign_key.foreign_table_id)?;
            let table = state
                .tables
                .get(&foreign_schema.id)
                .expect("catalog table must have storage");
            for (row_id, chain) in table.iterate_version_chains() {
                if find_visible_version(chain, snapshot, xid, &state.transactions).is_some() {
                    locks.push(RequiredRowLock {
                        key: RowLockKey {
                            table_id: foreign_schema.id,
                            row_id,
                        },
                        mode: RowLockMode::KeyShare,
                        mutation_candidate: None,
                    });
                }
            }
        }
        return Ok(locks);
    }
    let Some(source) = &insert.source else {
        return Ok(Vec::new());
    };
    let ast::SetExpr::Values(values) = source.body.as_ref() else {
        return Ok(Vec::new());
    };
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|column| {
                let column_name = normalize_unqualified_object_name(column)?;
                schema
                    .columns
                    .iter()
                    .position(|definition| definition.name == column_name)
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", column),
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut locks = Vec::new();
    for expressions in &values.rows {
        if expressions.len() != column_indexes.len() {
            continue;
        }
        for constraint in &schema.constraints {
            let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
                continue;
            };
            let local = resolve_foreign_key_column_indexes(schema, &foreign_key.columns)?;
            let mut key = Vec::new();
            for index in local {
                let Some(position) = column_indexes
                    .iter()
                    .position(|provided| *provided == index)
                else {
                    key.clear();
                    break;
                };
                let expression = &expressions[position];
                let mut has_side_effect = false;
                let _ = ast::visit_expressions(expression, |nested| {
                    if let ast::Expr::Function(function) = nested
                        && normalize_unqualified_object_name(&function.name).is_ok_and(|name| {
                            matches!(
                                name.as_str(),
                                "gen_random_uuid" | "uuidv4" | "uuidv7" | "nextval" | "setval"
                            )
                        })
                    {
                        has_side_effect = true;
                        return std::ops::ControlFlow::Break(());
                    }
                    std::ops::ControlFlow::Continue(())
                });
                if is_default_expression(expression) || has_side_effect {
                    key.clear();
                    break;
                }
                key.push(evaluate_assignment_expression(
                    expression,
                    schema.columns[index].data_type,
                    &create_constant_expression_schema(),
                    &[],
                    context,
                )?);
            }
            if key.len() != foreign_key.columns.len() {
                continue;
            }
            if key.iter().any(Value::is_null) {
                continue;
            }
            let foreign_schema = state
                .catalog
                .require_table_by_id(foreign_key.foreign_table_id)?;
            let referred = if foreign_key.referred_columns.is_empty() {
                foreign_schema
                    .constraints
                    .iter()
                    .find_map(|constraint| match constraint {
                        crate::catalog::Constraint::PrimaryKey { columns, .. } => {
                            Some(columns.clone())
                        }
                        _ => None,
                    })
                    .expect("foreign key definition was validated")
            } else {
                foreign_key.referred_columns.clone()
            };
            let referred = resolve_foreign_key_column_indexes(foreign_schema, &referred)?;
            let table = state
                .tables
                .get(&foreign_schema.id)
                .expect("catalog table must have storage");
            if let Some(row_id) =
                table.find_unique_row(&referred, &key, snapshot, xid, &state.transactions)
            {
                locks.push(RequiredRowLock {
                    key: RowLockKey {
                        table_id: foreign_schema.id,
                        row_id,
                    },
                    mode: RowLockMode::KeyShare,
                    mutation_candidate: None,
                });
            }
        }
    }
    Ok(locks)
}
