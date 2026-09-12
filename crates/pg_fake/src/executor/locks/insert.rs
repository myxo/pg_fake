use super::{RequiredRowLock, foreign_keys::collect_foreign_key_locks_for_rows};
use crate::executor::{
    DatabaseState, StatementContext,
    column_defaults::{evaluate_column_default, is_default_expression},
    expressions::{create_constant_expression_schema, evaluate_assignment_expression},
    normalize_unqualified_object_name, resolve_insert_table_name, writes,
};
use crate::{
    catalog::TableSchema,
    error::{PgError, Result, SqlState},
    txn::{RowLockKey, RowLockMode, Snapshot, Xid},
};
use sqlparser::ast;

pub(super) fn collect_triggered_insert_fallback_locks(
    state: &DatabaseState,
    insert: &ast::Insert,
    schema: &TableSchema,
    xid: Xid,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    let mut locks = Vec::new();
    if writes::resolve_conflict_arbiter(schema, insert.on.as_ref())?.is_some() {
        let table = state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage");
        locks.extend(
            table
                .find_unique_candidate_rows(xid, &state.transactions, None, None, context)
                .into_iter()
                .map(|row_id| RequiredRowLock {
                    key: RowLockKey {
                        table_id: schema.id,
                        row_id,
                    },
                    mode: RowLockMode::Update,
                    mutation_candidate: None,
                }),
        );
    }
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
        locks.extend(
            table
                .find_unique_candidate_rows(xid, &state.transactions, None, None, context)
                .into_iter()
                .map(|row_id| RequiredRowLock {
                    key: RowLockKey {
                        table_id: foreign_schema.id,
                        row_id,
                    },
                    mode: RowLockMode::Share,
                    mutation_candidate: None,
                }),
        );
    }
    Ok(locks)
}

pub(super) fn collect_triggered_insert_locks(
    state: &DatabaseState,
    insert: &ast::Insert,
    schema: &TableSchema,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    let column_indexes = writes::resolve_insert_column_indexes(schema, &insert.columns)?;
    let (rows, preview_conflicts) = writes::prepare_insert_rows(
        state,
        insert,
        schema,
        &column_indexes,
        xid,
        snapshot,
        context,
    )?;
    let mut locks = Vec::new();
    let mut conflicting_rows = vec![false; rows.len()];
    if writes::resolve_conflict_arbiter(schema, insert.on.as_ref())?.is_some() {
        let table = state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage");
        for (index, row) in rows.iter().enumerate() {
            if let Some(row_id) =
                table.find_conflicting_row(row, xid, &state.transactions, None, None, context)
            {
                conflicting_rows[index] = true;
                locks.push(RequiredRowLock {
                    key: RowLockKey {
                        table_id: schema.id,
                        row_id,
                    },
                    mode: RowLockMode::Update,
                    mutation_candidate: None,
                });
            }
        }
    }
    if matches!(
        insert.on,
        Some(ast::OnInsert::OnConflict(ast::OnConflict {
            action: ast::OnConflictAction::DoUpdate(_),
            ..
        }))
    ) && schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, crate::catalog::Constraint::ForeignKey(_)))
        && schema.triggers.iter().any(|trigger| {
            trigger
                .definition
                .events
                .iter()
                .any(|event| matches!(event, ast::TriggerEvent::Update(_)))
        })
    {
        if locks.iter().any(|required| {
            required.key.table_id == schema.id
                && !state
                    .row_locks
                    .is_held(required.key, xid, RowLockMode::Update)
        }) {
            context.request_row_lock_recheck();
        } else {
            locks.extend(collect_foreign_key_locks_for_rows(
                state,
                schema,
                preview_conflicts
                    .iter()
                    .filter_map(|update| update.as_ref()?.updated.as_ref()),
                xid,
            )?);
        }
    }
    locks.extend(collect_foreign_key_locks_for_rows(
        state,
        schema,
        rows.iter()
            .enumerate()
            .filter_map(|(index, row)| (!conflicting_rows[index]).then_some(row)),
        xid,
    )?);
    Ok(locks)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_insert_conflict_locks(
    state: &DatabaseState,
    insert: &ast::Insert,
    xid: Xid,
    context: &StatementContext,
) -> Result<Vec<RequiredRowLock>> {
    let schema = state
        .catalog
        .require_named_table(&resolve_insert_table_name(&insert.table)?)?;
    let Some(_) = writes::resolve_conflict_arbiter(schema, insert.on.as_ref())? else {
        return Ok(Vec::new());
    };
    let updates_conflict = matches!(
        insert.on,
        Some(ast::OnInsert::OnConflict(ast::OnConflict {
            action: ast::OnConflictAction::DoUpdate(_),
            ..
        }))
    );
    let values = insert.source.as_ref().and_then(|source| {
        if let ast::SetExpr::Values(values) = source.body.as_ref() {
            Some(values)
        } else {
            None
        }
    });
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|column| {
                let name = normalize_unqualified_object_name(column)?;
                schema
                    .columns
                    .iter()
                    .position(|definition| definition.name == name)
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("column {name:?} does not exist"),
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let mut locks = Vec::new();
    let mut needs_fallback = values.is_none() || updates_conflict || !schema.triggers.is_empty();
    for expressions in values.into_iter().flat_map(|values| &values.rows) {
        if expressions.len() != column_indexes.len() {
            continue;
        }
        let mut row = Vec::with_capacity(schema.columns.len());
        let mut evaluable = true;
        for (index, column) in schema.columns.iter().enumerate() {
            let expression = column_indexes
                .iter()
                .position(|provided| *provided == index)
                .map(|position| &expressions[position]);
            let value = match expression {
                Some(expression) if !is_default_expression(expression) => {
                    let mut has_function = false;
                    let _ = ast::visit_expressions(expression, |nested| {
                        if matches!(nested, ast::Expr::Function(_)) {
                            has_function = true;
                            return std::ops::ControlFlow::Break(());
                        }
                        std::ops::ControlFlow::Continue(())
                    });
                    if has_function {
                        evaluable = false;
                        break;
                    }
                    evaluate_assignment_expression(
                        expression,
                        column.data_type,
                        &create_constant_expression_schema(),
                        &[],
                        context,
                    )?
                }
                Some(_) | None => {
                    if column.default_sequence.is_some()
                        || !matches!(column.default, None | Some(ast::Expr::Value(_)))
                    {
                        evaluable = false;
                        break;
                    }
                    evaluate_column_default(column, context)?
                }
            };
            row.push(value);
        }
        if !evaluable {
            needs_fallback = true;
            continue;
        }
        if let Some(row_id) =
            table.find_conflicting_row(&row, xid, &state.transactions, None, None, context)
        {
            locks.push(RequiredRowLock {
                key: RowLockKey {
                    table_id: schema.id,
                    row_id,
                },
                mode: RowLockMode::Update,
                mutation_candidate: None,
            });
        }
    }
    if needs_fallback {
        locks.clear();
        locks.extend(
            table
                .find_unique_candidate_rows(xid, &state.transactions, None, None, context)
                .into_iter()
                .map(|row_id| RequiredRowLock {
                    key: RowLockKey {
                        table_id: schema.id,
                        row_id,
                    },
                    mode: RowLockMode::Update,
                    mutation_candidate: None,
                }),
        );
    }
    Ok(locks)
}
