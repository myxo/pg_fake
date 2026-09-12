use super::{
    conflicts::{
        InsertConflictOutcome, build_conflict_update_plan, execute_insert_conflict,
        resolve_conflict_arbiter,
    },
    insert_preparation::evaluate_triggered_insert_rows,
    require_mutation_table,
    returning::{ReturningPlan, build_returning_plan, create_write_result, evaluate_returning_row},
};
use crate::executor::{
    DatabaseState, StatementExecutionContext, foreign_keys::validate_row_foreign_keys,
    normalize_unqualified_object_name, resolve_insert_table_name, scope::bind_target_scope,
};
use crate::{
    StatementResult,
    catalog::{Constraint, ConstraintId, TableSchema},
    error::{PgError, Result, SqlState},
    storage::RowId,
    txn::{RowLockAttempt, RowLockKey, RowLockMode, Snapshot, Xid},
    value::Value,
};
use sqlparser::ast;
use std::collections::BTreeSet;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn resolve_insert_column_indexes(
    schema: &TableSchema,
    columns: &[ast::ObjectName],
) -> Result<Vec<usize>> {
    if columns.is_empty() {
        return Ok((0..schema.columns.len()).collect());
    }
    columns
        .iter()
        .map(|name| {
            let name = normalize_unqualified_object_name(name)?;
            schema
                .columns
                .iter()
                .position(|column| column.name == name)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {:?} does not exist", name),
                    )
                })
        })
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn execute_insert(
    state: &mut DatabaseState,
    insert: &ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    let table_name = resolve_insert_table_name(&insert.table)?;
    let schema = require_mutation_table(state, &table_name)?;
    let conflict_arbiter = resolve_conflict_arbiter(&schema, insert.on.as_ref())?;
    let conflict_update = match insert.on.as_ref() {
        Some(ast::OnInsert::OnConflict(ast::OnConflict {
            action: ast::OnConflictAction::DoUpdate(update),
            ..
        })) => Some(build_conflict_update_plan(
            state,
            &schema,
            insert.table_alias.as_ref().map(|alias| &alias.alias),
            update,
        )?),
        _ => None,
    };
    let returning_scope = bind_target_scope(
        &schema,
        insert.table_alias.as_ref().map(|alias| &alias.alias),
    );
    let returning = build_returning_plan(
        state,
        returning_scope,
        schema.columns.len(),
        insert.returning.as_deref(),
    )?;
    let column_indexes = resolve_insert_column_indexes(&schema, &insert.columns)?;
    let (rows, prepared_conflicts, prepared_returned_rows, prepared_error) =
        match context.take_prepared_trigger_insert(insert) {
            Some(prepared) => (
                prepared.rows,
                prepared.conflicts,
                prepared.returned_rows,
                prepared.error,
            ),
            None => {
                let prepared = evaluate_triggered_insert_rows(
                    state,
                    insert,
                    &schema,
                    &column_indexes,
                    returning.as_ref(),
                    None,
                    false,
                    xid,
                    snapshot,
                    context,
                )?;
                (
                    prepared.rows,
                    prepared.conflicts,
                    prepared.returned_rows,
                    prepared.error,
                )
            }
        };
    let prepared_conflicts = Some(prepared_conflicts);
    let can_move_inserted_row = returning.is_none()
        && !schema
            .constraints
            .iter()
            .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)));
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let mut returned_rows = Vec::new();
    let mut affected = 0;
    let mut affected_rows = BTreeSet::new();
    let mut inserted_rows = Vec::new();
    for (row_index, row) in rows.into_iter().enumerate() {
        match execute_insert_conflict(
            state,
            &schema,
            &row,
            conflict_arbiter.as_ref(),
            conflict_update.as_ref(),
            &affected_rows,
            has_referencing_foreign_keys,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
            prepared_conflicts
                .as_ref()
                .and_then(|conflicts| conflicts[row_index].as_ref()),
        )? {
            InsertConflictOutcome::Skip => continue,
            InsertConflictOutcome::Update { row_id, row } => {
                affected += 1;
                affected_rows.insert(row_id);
                let prepared_returned = prepared_returned_rows
                    .as_ref()
                    .and_then(|rows| rows.get(row_index))
                    .and_then(|returned| returned.as_ref());
                if let Some(returned) = prepared_returned {
                    returned_rows.push(returned.clone());
                } else {
                    evaluate_returning_row(
                        state,
                        returning.as_ref(),
                        &row,
                        &mut returned_rows,
                        xid,
                        snapshot,
                        context,
                    )?;
                }
                continue;
            }
            InsertConflictOutcome::Insert => {}
        }
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(
                &row,
                snapshot,
                xid,
                &state.transactions,
                None,
                None,
                None,
                None,
                context,
            )
        {
            return Err(PgError::create(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        affected += 1;
        if !can_move_inserted_row {
            inserted_rows.push(row.clone());
        }
        let prepared_returned = prepared_returned_rows
            .as_ref()
            .and_then(|rows| rows.get(row_index))
            .and_then(|returned| returned.as_ref());
        if let Some(returned) = prepared_returned {
            returned_rows.push(returned.clone());
        }
        let row_id = insert_new_row(
            state,
            &schema,
            row,
            can_move_inserted_row,
            prepared_returned
                .is_none()
                .then_some(returning.as_ref())
                .flatten(),
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
        affected_rows.insert(row_id);
    }
    if let Some(error) = prepared_error {
        return Err(error);
    }
    for row in &inserted_rows {
        validate_row_foreign_keys(
            state,
            &schema,
            row,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &inserted_rows,
        )?;
    }
    Ok(create_write_result(affected, returning, returned_rows))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn insert_new_row(
    state: &mut DatabaseState,
    schema: &TableSchema,
    row: Vec<Value>,
    can_move_row: bool,
    returning: Option<&ReturningPlan<'_>>,
    returned_rows: &mut Vec<Vec<Value>>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<RowId> {
    let retained_row = (!can_move_row).then(|| row.clone());
    let row_id = state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage")
        .insert(xid, context.command_id, row);
    let lock = state.row_locks.acquire(
        RowLockKey {
            table_id: schema.id,
            row_id,
        },
        xid,
        RowLockMode::Update,
    );
    assert!(matches!(lock, RowLockAttempt::Acquired));
    state.mark_table_touched(xid, schema.id);
    if let Some(row) = retained_row {
        evaluate_returning_row(
            state,
            returning,
            &row,
            returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    Ok(row_id)
}
