use super::{
    build_mutation_assignments, evaluate_mutation_assignment, require_mutation_table,
    returning::{build_returning_plan, create_write_result, evaluate_returning_row},
    targets::{
        collect_mutation_targets, create_mutation_scope, has_mutated_target_in_command,
        materialize_mutation_source_rows,
    },
};
use crate::executor::{
    DatabaseState, PreparedTriggerUpdate, RequiredRowLock, StatementExecutionContext,
    expressions::{
        evaluate_column_default, is_default_expression, is_null_literal,
        validate_check_constraints, validate_not_null,
    },
    foreign_keys::{apply_referencing_foreign_key_actions, validate_row_foreign_keys},
    normalize_relation_name, prepared, procedural, query,
};
use crate::{
    StatementResult,
    catalog::{Constraint, ConstraintId, TableSchema},
    error::{PgError, Result, SqlState, reject_unsupported},
    txn::{Snapshot, Xid},
    value::BaseType,
};
use sqlparser::ast::{self, Spanned as _};
use std::collections::BTreeSet;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn execute_update(
    state: &mut DatabaseState,
    update: &ast::Update,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<StatementResult> {
    let update_table = &update.table;
    let assignments = &update.assignments;
    let from = update.from.as_ref();
    let selection = update.selection.as_ref();
    let returning_items = update.returning.as_deref();
    if !update_table.joins.is_empty() {
        return reject_unsupported("UPDATE joins are not implemented");
    }
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args,
        ..
    } = &update_table.relation
    else {
        return reject_unsupported("UPDATE target is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("UPDATE table functions are not implemented");
    }
    let schema = require_mutation_table(state, &normalize_relation_name(table_name)?)?;
    let from = match from {
        None => &[][..],
        Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
        Some(ast::UpdateTableFromKind::BeforeSet(_)) => {
            return reject_unsupported("UPDATE FROM before SET is not implemented");
        }
    };
    let scope = create_mutation_scope(
        state,
        &schema,
        alias.as_ref().map(|alias| &alias.name),
        from,
    )?;
    let returning =
        build_returning_plan(state, scope.clone(), schema.columns.len(), returning_items)?;
    if let Some(selection) = selection {
        let base = query::infer_query_expression_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let (assigned, assignments) = build_mutation_assignments(state, &schema, &scope, assignments)?;
    let prepared_updates = context.take_prepared_trigger_update(update).map(|updates| {
        updates
            .into_iter()
            .filter(|prepared| {
                !has_mutated_target_in_command(
                    state,
                    schema.id,
                    prepared.row_id,
                    prepared.version_xmin,
                    xid,
                    context.command_id,
                )
            })
            .collect::<Vec<_>>()
    });
    let prepared_targets =
        context.take_prepared_mutation_targets(update.span(), snapshot.commit_seq);
    let targets = match &prepared_updates {
        Some(prepared) => prepared
            .iter()
            .map(|prepared| {
                (
                    prepared.row_id,
                    prepared.version_xmin,
                    prepared.current.clone(),
                    prepared.bound_row.clone(),
                )
            })
            .collect(),
        None => match prepared_targets {
            Some(targets) => targets
                .into_iter()
                .filter(|target| {
                    !has_mutated_target_in_command(
                        state,
                        schema.id,
                        target.row_id,
                        target.version_xmin,
                        xid,
                        context.command_id,
                    )
                })
                .map(|target| {
                    (
                        target.row_id,
                        target.version_xmin,
                        target.current,
                        target.bound_row,
                    )
                })
                .collect(),
            None => {
                let source_rows = materialize_mutation_source_rows(
                    state,
                    from,
                    &scope,
                    schema.columns.len(),
                    xid,
                    snapshot,
                    context,
                )?;
                collect_mutation_targets(
                    state,
                    &schema,
                    selection,
                    &scope,
                    &source_rows,
                    xid,
                    snapshot,
                    context,
                    mutation_targets,
                )?
            }
        },
    };
    let updates_were_prepared = prepared_updates.is_some();
    let mut prepared_updates = prepared_updates.map(|updates| {
        updates
            .into_iter()
            .map(|prepared| (prepared.row_id, prepared.updated))
    });
    let mut affected = 0;
    let has_referencing_foreign_keys = state.catalog.has_referencing_foreign_keys(schema.id);
    let has_foreign_keys = schema
        .constraints
        .iter()
        .any(|constraint| matches!(constraint, Constraint::ForeignKey(_)));
    let can_move_updated_row =
        !has_referencing_foreign_keys && !has_foreign_keys && returning.is_none();
    let mut returned_rows = Vec::new();
    for (row_id, version_xmin, row, mut bound_row) in targets {
        let (old_row, mut updated) = if has_referencing_foreign_keys {
            (Some(row.clone()), row)
        } else {
            (None, row)
        };
        let updated = if let Some(prepared_updates) = &mut prepared_updates {
            let (prepared_row_id, updated) = prepared_updates
                .next()
                .expect("prepared trigger UPDATE retains every target row");
            assert_eq!(prepared_row_id, row_id);
            let Some(updated) = updated else {
                continue;
            };
            updated
        } else {
            let assignment_row = bound_row.as_deref().unwrap_or(&updated).to_vec();
            for assignment in &assignments {
                let target = schema.columns[assignment.index].data_type;
                updated[assignment.index] = if is_default_expression(assignment.expression) {
                    evaluate_column_default(&schema.columns[assignment.index], context)?
                } else if let Some(prepared) = &assignment.prepared {
                    prepared::evaluate_prepared_expression(
                        prepared,
                        &assignment_row,
                        &[],
                        context.deadline,
                    )?
                } else {
                    evaluate_mutation_assignment(
                        state,
                        assignment.expression,
                        target,
                        &scope,
                        &assignment_row,
                        xid,
                        snapshot,
                        context,
                    )?
                };
            }
            let Some(updated) = procedural::execute_before_row_triggers(
                state,
                &schema,
                procedural::TriggerEventKind::Update,
                updated,
                context,
            )?
            else {
                continue;
            };
            updated
        };
        affected += 1;
        if !updates_were_prepared {
            validate_not_null(&schema, &updated)?;
            validate_check_constraints(&schema, &updated, context)?;
        }
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(
                &updated,
                snapshot,
                xid,
                &state.transactions,
                Some(row_id),
                schema.triggers.is_empty().then_some(&assigned),
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
        if can_move_updated_row {
            state
                .tables
                .get_mut(&schema.id)
                .expect("catalog table must have storage")
                .append_updated_version(
                    row_id,
                    version_xmin,
                    xid,
                    context.command_id,
                    updated,
                    schema.triggers.is_empty().then_some(&assigned),
                );
            state.mark_table_touched(xid, schema.id);
            continue;
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .append_updated_version(
                row_id,
                version_xmin,
                xid,
                context.command_id,
                updated.clone(),
                schema.triggers.is_empty().then_some(&assigned),
            );
        state.mark_table_touched(xid, schema.id);
        validate_row_foreign_keys(
            state,
            &schema,
            &updated,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &[],
        )?;
        if has_referencing_foreign_keys {
            apply_referencing_foreign_key_actions(
                state,
                &schema,
                old_row
                    .as_ref()
                    .expect("referencing foreign keys retain the old row"),
                Some(&updated),
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                &mut BTreeSet::new(),
                context,
            )?;
        }
        if let Some(bound_row) = &mut bound_row {
            bound_row[..schema.columns.len()].clone_from_slice(&updated);
        }
        evaluate_returning_row(
            state,
            returning.as_ref(),
            bound_row.as_deref().unwrap_or(&updated),
            &mut returned_rows,
            xid,
            snapshot,
            context,
        )?;
    }
    assert!(prepared_updates.is_none_or(|mut rows| rows.next().is_none()));
    Ok(create_write_result(affected, returning, returned_rows))
}

pub(in crate::executor) fn prepare_triggered_update_rows(
    state: &DatabaseState,
    update: &ast::Update,
    schema: &TableSchema,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Vec<PreparedTriggerUpdate>> {
    let (index, prepared) = context.get_prepared_trigger_update(update);
    if let Some(rows) = prepared {
        return Ok(rows);
    }
    let alias = match &update.table.relation {
        ast::TableFactor::Table { alias, .. } => alias.as_ref().map(|alias| &alias.name),
        _ => unreachable!("lockable UPDATE targets a table"),
    };
    let from = match &update.from {
        None => &[][..],
        Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
        Some(ast::UpdateTableFromKind::BeforeSet(_)) => {
            return reject_unsupported("UPDATE FROM before SET is not implemented");
        }
    };
    let scope = create_mutation_scope(state, schema, alias, from)?;
    let (_, assignments) = build_mutation_assignments(state, schema, &scope, &update.assignments)?;
    let targets = match context.get_prepared_mutation_targets(update.span(), snapshot.commit_seq) {
        Some(targets) => targets
            .into_iter()
            .filter(|target| {
                !has_mutated_target_in_command(
                    state,
                    schema.id,
                    target.row_id,
                    target.version_xmin,
                    xid,
                    context.command_id,
                )
            })
            .map(|target| {
                (
                    target.row_id,
                    target.version_xmin,
                    target.current,
                    target.bound_row,
                )
            })
            .collect(),
        None => {
            let source_rows = materialize_mutation_source_rows(
                state,
                from,
                &scope,
                schema.columns.len(),
                xid,
                snapshot,
                context,
            )?;
            collect_mutation_targets(
                state,
                schema,
                update.selection.as_ref(),
                &scope,
                &source_rows,
                xid,
                snapshot,
                context,
                None,
            )?
        }
    };
    let mut rows = Vec::new();
    let mut validation_table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .clone();
    for (row_id, version_xmin, current, bound_row) in targets {
        let mut updated = current.clone();
        let assignment_row = bound_row.as_deref().unwrap_or(&current);
        for assignment in &assignments {
            updated[assignment.index] = if is_default_expression(assignment.expression) {
                evaluate_column_default(&schema.columns[assignment.index], context)?
            } else if let Some(prepared) = &assignment.prepared {
                prepared::evaluate_prepared_expression(
                    prepared,
                    &assignment_row,
                    &[],
                    context.deadline,
                )?
            } else {
                evaluate_mutation_assignment(
                    state,
                    assignment.expression,
                    schema.columns[assignment.index].data_type,
                    &scope,
                    &assignment_row,
                    xid,
                    snapshot,
                    context,
                )?
            };
        }
        let updated = procedural::execute_before_row_triggers(
            state,
            schema,
            procedural::TriggerEventKind::Update,
            updated,
            context,
        )?;
        if let Some(updated) = &updated {
            validate_not_null(schema, updated)?;
            validate_check_constraints(schema, updated, context)?;
            if validation_table.has_visible_unique_conflict(
                updated,
                snapshot,
                xid,
                &state.transactions,
                Some(row_id),
                None,
                None,
                None,
                context,
            ) {
                return Err(PgError::create(
                    SqlState::UniqueViolation,
                    format!(
                        "duplicate key value violates unique constraint on {:?}",
                        schema.name
                    ),
                ));
            }
            validation_table.append_updated_version(
                row_id,
                version_xmin,
                xid,
                context.command_id,
                updated.clone(),
                None,
            );
        }
        rows.push(PreparedTriggerUpdate {
            row_id,
            version_xmin,
            current,
            bound_row,
            updated,
        });
    }
    context.set_prepared_trigger_update(index, update.clone(), rows.clone());
    Ok(rows)
}
