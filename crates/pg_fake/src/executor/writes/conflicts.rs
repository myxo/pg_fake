use super::{
    MutationAssignment, build_mutation_assignments, evaluate_mutation_assignment,
    targets::matches_mutation_row,
};
use crate::executor::{
    DatabaseState, PreparedConflictUpdate, StatementContext,
    expressions::{
        evaluate_column_default, is_default_expression, is_null_literal,
        validate_check_constraints, validate_index_predicate, validate_not_null,
    },
    foreign_keys::{apply_referencing_foreign_key_actions, validate_row_foreign_keys},
    normalize_identifier, prepared, procedural, query,
    scope::{BoundScope, bind_target_scope, combine_bound_scopes},
};
use crate::{
    catalog::{Constraint, ConstraintId, TableSchema},
    error::{PgError, Result, SqlState, reject_unsupported},
    storage::{RowId, Table},
    txn::{Snapshot, Xid},
    value::{BaseType, Value},
};
use sqlparser::ast;
use std::collections::BTreeSet;

pub(super) struct ConflictUpdatePlan<'a> {
    pub(super) scope: BoundScope,
    assigned: BTreeSet<usize>,
    pub(super) assignments: Vec<MutationAssignment<'a>>,
    selection: Option<&'a ast::Expr>,
}

pub(super) enum InsertConflictOutcome {
    Insert,
    Skip,
    Update { row_id: RowId, row: Vec<Value> },
}

pub(in crate::executor) enum ConflictArbiter {
    Any,
    Index {
        columns: Vec<usize>,
        predicate: Option<ast::Expr>,
    },
}

impl ConflictArbiter {
    pub(super) fn get_columns(&self) -> Option<&[usize]> {
        match self {
            ConflictArbiter::Any => None,
            ConflictArbiter::Index { columns, .. } => Some(columns),
        }
    }

    pub(super) fn get_predicate(&self) -> Option<&ast::Expr> {
        match self {
            ConflictArbiter::Any => None,
            ConflictArbiter::Index { predicate, .. } => predicate.as_ref(),
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn resolve_conflict_arbiter(
    schema: &TableSchema,
    on: Option<&ast::OnInsert>,
) -> Result<Option<ConflictArbiter>> {
    let Some(on) = on else {
        return Ok(None);
    };
    let ast::OnInsert::OnConflict(conflict) = on else {
        return reject_unsupported("INSERT conflict action is not implemented");
    };
    let Some(target) = &conflict.conflict_target else {
        if matches!(conflict.action, ast::OnConflictAction::DoUpdate(_)) {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "ON CONFLICT DO UPDATE requires inference specification or constraint name",
            ));
        }
        return Ok(Some(ConflictArbiter::Any));
    };
    let constraint_columns = match target {
        ast::ConflictTarget::Columns { columns, predicate } => {
            let requested = columns
                .iter()
                .map(|column| {
                    let name = normalize_identifier(column);
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
                .collect::<Result<BTreeSet<_>>>()?;
            if requested.len() != columns.len() {
                return Err(PgError::create(
                    SqlState::InvalidColumnReference,
                    "there is no unique or exclusion constraint matching the ON CONFLICT specification",
                ));
            }
            if let Some(predicate) = predicate {
                validate_index_predicate(predicate, schema)?;
            }
            let constraint = schema.constraints.iter().find_map(|constraint| {
                if predicate.is_some() {
                    return None;
                }
                let columns = match constraint {
                    Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } => {
                        columns
                    }
                    Constraint::Check { .. } | Constraint::ForeignKey(_) => return None,
                };
                let indexes = columns
                    .iter()
                    .map(|name| {
                        schema
                            .columns
                            .iter()
                            .position(|column| column.name == *name)
                            .expect("constraint columns must exist")
                    })
                    .collect::<Vec<_>>();
                (indexes.len() == requested.len()
                    && indexes.iter().all(|index| requested.contains(index)))
                .then_some((indexes, None))
            });
            constraint.or_else(|| {
                schema.indexes.iter().find_map(|index| {
                    if !index.unique {
                        return None;
                    }
                    let indexes = index
                        .columns
                        .iter()
                        .map(|column| {
                            schema
                                .columns
                                .iter()
                                .position(|definition| definition.name == column.name)
                                .expect("index columns must exist")
                        })
                        .collect::<Vec<_>>();
                    let predicate_matches = match (&index.predicate, predicate) {
                        (None, _) => true,
                        (Some(index), Some(target)) => index == target,
                        (Some(_), None) => false,
                    };
                    (predicate_matches
                        && indexes.len() == requested.len()
                        && indexes.iter().all(|column| requested.contains(column)))
                    .then(|| (indexes, index.predicate.clone()))
                })
            })
        }
        ast::ConflictTarget::OnConstraint(name) => {
            let name = crate::executor::normalize_unqualified_object_name(name)?;
            let Some(constraint) = schema
                .constraints
                .iter()
                .find(|constraint| match constraint {
                    Constraint::PrimaryKey {
                        name: constraint_name,
                        ..
                    }
                    | Constraint::Unique {
                        name: constraint_name,
                        ..
                    } => constraint_name == &name,
                    Constraint::ForeignKey(foreign_key) => foreign_key.name == name,
                    Constraint::Check { .. } => false,
                })
            else {
                return Err(PgError::create(
                    SqlState::UndefinedObject,
                    format!(
                        "constraint {name:?} for table {:?} does not exist",
                        schema.name
                    ),
                ));
            };
            match constraint {
                Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } => {
                    Some((
                        columns
                            .iter()
                            .map(|name| {
                                schema
                                    .columns
                                    .iter()
                                    .position(|column| column.name == *name)
                                    .expect("constraint columns must exist")
                            })
                            .collect(),
                        None,
                    ))
                }
                Constraint::Check { .. } | Constraint::ForeignKey(_) => {
                    return Err(PgError::create(
                        SqlState::WrongObjectType,
                        format!("constraint {name:?} has no associated index"),
                    ));
                }
            }
        }
    };
    constraint_columns
        .map(|(columns, predicate)| ConflictArbiter::Index { columns, predicate })
        .map(Some)
        .ok_or_else(|| {
            PgError::create(
                SqlState::InvalidColumnReference,
                "there is no unique or exclusion constraint matching the ON CONFLICT specification",
            )
        })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn build_conflict_update_plan<'a>(
    state: &DatabaseState,
    schema: &TableSchema,
    alias: Option<&ast::Ident>,
    update: &'a ast::DoUpdate,
) -> Result<ConflictUpdatePlan<'a>> {
    let target = bind_target_scope(schema, alias);
    let excluded_name = ast::Ident::new("excluded");
    let mut excluded = bind_target_scope(schema, Some(&excluded_name));
    for column in &mut excluded.columns {
        column.unqualified = false;
        column.table_id = None;
    }
    let scope = combine_bound_scopes(target, excluded);
    if let Some(selection) = &update.selection {
        let base = query::infer_query_expression_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let (assigned, assignments) =
        build_mutation_assignments(state, schema, &scope, &update.assignments)?;
    Ok(ConflictUpdatePlan {
        scope,
        assigned,
        assignments,
        selection: update.selection.as_ref(),
    })
}

pub(super) fn prepare_conflict_update(
    state: &DatabaseState,
    schema: &TableSchema,
    table: &Table,
    row: &Vec<Value>,
    arbiter: Option<&ConflictArbiter>,
    update: Option<&ConflictUpdatePlan<'_>>,
    affected_rows: Option<&BTreeSet<RowId>>,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementContext,
) -> Result<Option<PreparedConflictUpdate>> {
    let (Some(arbiter), Some(update)) = (arbiter, update) else {
        return Ok(None);
    };
    let columns = arbiter
        .get_columns()
        .expect("DO UPDATE requires a unique arbiter");
    let Some((row_id, version)) = table.find_visible_unique_conflict(
        row,
        snapshot,
        xid,
        &state.transactions,
        columns,
        arbiter.get_predicate(),
        context,
    ) else {
        return Ok(None);
    };
    if affected_rows.is_some_and(|affected| affected.contains(&row_id)) {
        return Err(PgError::create(
            SqlState::CardinalityViolation,
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
        ));
    }
    let current = version.row.clone();
    let mut bound_row = current.clone();
    bound_row.extend_from_slice(row);
    if !matches_mutation_row(
        state,
        update.selection,
        &update.scope,
        &bound_row,
        xid,
        snapshot,
        context,
    )? {
        return Ok(Some(PreparedConflictUpdate {
            row_id,
            version_xmin: version.xmin,
            current,
            updated: None,
        }));
    }
    let mut updated = current.clone();
    for assignment in &update.assignments {
        updated[assignment.index] = if is_default_expression(assignment.expression) {
            evaluate_column_default(&schema.columns[assignment.index], context)?
        } else if let Some(prepared) = &assignment.prepared {
            prepared::evaluate_prepared_expression(prepared, &bound_row, &[], context.deadline)?
        } else {
            evaluate_mutation_assignment(
                state,
                assignment.expression,
                schema.columns[assignment.index].data_type,
                &update.scope,
                &bound_row,
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
    Ok(Some(PreparedConflictUpdate {
        row_id,
        version_xmin: version.xmin,
        current,
        updated,
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_insert_conflict(
    state: &mut DatabaseState,
    schema: &TableSchema,
    row: &Vec<Value>,
    arbiter: Option<&ConflictArbiter>,
    update: Option<&ConflictUpdatePlan<'_>>,
    affected_rows: &BTreeSet<RowId>,
    has_referencing_foreign_keys: bool,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementContext,
    prepared: Option<&PreparedConflictUpdate>,
) -> Result<InsertConflictOutcome> {
    let Some(arbiter) = arbiter else {
        return Ok(InsertConflictOutcome::Insert);
    };
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let Some(update) = update else {
        return Ok(
            if table.has_visible_unique_conflict(
                row,
                snapshot,
                xid,
                &state.transactions,
                None,
                None,
                arbiter.get_columns(),
                arbiter.get_predicate(),
                context,
            ) {
                InsertConflictOutcome::Skip
            } else {
                InsertConflictOutcome::Insert
            },
        );
    };
    if let Some(prepared) = prepared {
        if affected_rows.contains(&prepared.row_id) {
            return Err(PgError::create(
                SqlState::CardinalityViolation,
                "ON CONFLICT DO UPDATE command cannot affect row a second time",
            ));
        }
        let Some(updated) = &prepared.updated else {
            return Ok(InsertConflictOutcome::Skip);
        };
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .has_visible_unique_conflict(
                updated,
                snapshot,
                xid,
                &state.transactions,
                Some(prepared.row_id),
                schema.triggers.is_empty().then_some(&update.assigned),
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
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .append_updated_version(
                prepared.row_id,
                prepared.version_xmin,
                xid,
                context.command_id,
                updated.clone(),
                schema.triggers.is_empty().then_some(&update.assigned),
            );
        state.mark_table_touched(xid, schema.id);
        validate_row_foreign_keys(
            state,
            schema,
            updated,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &[],
        )?;
        if has_referencing_foreign_keys {
            apply_referencing_foreign_key_actions(
                state,
                schema,
                &prepared.current,
                Some(updated),
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                &mut BTreeSet::new(),
                context,
            )?;
        }
        return Ok(InsertConflictOutcome::Update {
            row_id: prepared.row_id,
            row: updated.clone(),
        });
    }
    let columns = arbiter
        .get_columns()
        .expect("DO UPDATE requires a unique arbiter");
    let conflict = table
        .find_visible_unique_conflict(
            row,
            snapshot,
            xid,
            &state.transactions,
            columns,
            arbiter.get_predicate(),
            context,
        )
        .map(|(row_id, version)| (row_id, version.xmin, version.row.clone()));
    let Some((row_id, version_xmin, current)) = conflict else {
        return Ok(InsertConflictOutcome::Insert);
    };
    let mut bound_row = current.clone();
    bound_row.extend_from_slice(row);
    if !matches_mutation_row(
        state,
        update.selection,
        &update.scope,
        &bound_row,
        xid,
        snapshot,
        context,
    )? {
        return Ok(InsertConflictOutcome::Skip);
    }
    if affected_rows.contains(&row_id) {
        return Err(PgError::create(
            SqlState::CardinalityViolation,
            "ON CONFLICT DO UPDATE command cannot affect row a second time",
        ));
    }
    let mut updated = current.clone();
    for assignment in &update.assignments {
        updated[assignment.index] = if is_default_expression(assignment.expression) {
            evaluate_column_default(&schema.columns[assignment.index], context)?
        } else if let Some(prepared) = &assignment.prepared {
            prepared::evaluate_prepared_expression(prepared, &bound_row, &[], context.deadline)?
        } else {
            evaluate_mutation_assignment(
                state,
                assignment.expression,
                schema.columns[assignment.index].data_type,
                &update.scope,
                &bound_row,
                xid,
                snapshot,
                context,
            )?
        };
    }
    let Some(updated) = procedural::execute_before_row_triggers(
        state,
        schema,
        procedural::TriggerEventKind::Update,
        updated,
        context,
    )?
    else {
        return Ok(InsertConflictOutcome::Skip);
    };
    validate_not_null(schema, &updated)?;
    validate_check_constraints(schema, &updated, context)?;
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
            schema.triggers.is_empty().then_some(&update.assigned),
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
            schema.triggers.is_empty().then_some(&update.assigned),
        );
    state.mark_table_touched(xid, schema.id);
    validate_row_foreign_keys(
        state,
        schema,
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
            schema,
            &current,
            Some(&updated),
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &mut BTreeSet::new(),
            context,
        )?;
    }
    Ok(InsertConflictOutcome::Update {
        row_id,
        row: updated,
    })
}
